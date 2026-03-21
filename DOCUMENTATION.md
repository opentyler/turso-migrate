# turso-converge Documentation

> Comprehensive reference for the turso-converge library — declarative schema convergence for [Turso](https://turso.tech/) databases.

---

## Table of Contents

- [Introduction](#introduction)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Core Concepts](#core-concepts)
  - [Declarative Schema Convergence](#declarative-schema-convergence)
  - [Fast Path vs Slow Path](#fast-path-vs-slow-path)
  - [The Convergence Algorithm](#the-convergence-algorithm)
- [API Reference](#api-reference)
  - [converge](#converge)
  - [converge_with_options](#converge_with_options)
  - [converge_from_path](#converge_from_path)
  - [converge_multi](#converge_multi)
  - [converge_multi_with_options](#converge_multi_with_options)
  - [validate_schema](#validate_schema)
  - [schema_version](#schema_version)
  - [rollback_to_previous](#rollback_to_previous)
  - [is_read_only](#is_read_only)
  - [compute_diff](#compute_diff)
  - [generate_plan](#generate_plan)
  - [execute_plan](#execute_plan)
- [Configuration Reference](#configuration-reference)
  - [ConvergeOptions](#convergeoptions)
  - [ConvergePolicy](#convergepolicy)
  - [ConvergeReport](#convergereport)
  - [ConvergeMode](#convergemode)
  - [DataMigration](#datamigration)
  - [ColumnRenameHint](#columnrenamehint)
  - [DestructiveChangeSet](#destructivechangeset)
  - [Failpoint](#failpoint)
- [Schema Type System](#schema-type-system)
  - [SchemaSnapshot](#schemasnapshot)
  - [TableInfo](#tableinfo)
  - [ColumnInfo](#columninfo)
  - [IndexInfo](#indexinfo)
  - [ViewInfo](#viewinfo)
  - [TriggerInfo](#triggerinfo)
  - [ForeignKey](#foreignkey)
  - [Capabilities](#capabilities)
  - [CIString](#cistring)
- [Diff Engine](#diff-engine)
  - [SchemaDiff](#schemadiff)
  - [Diff Categories](#diff-categories)
  - [Column Change Classification](#column-change-classification)
  - [Column Rename Detection](#column-rename-detection)
  - [Index and View Diffing](#index-and-view-diffing)
- [Migration Planning](#migration-planning)
  - [MigrationPlan](#migrationplan)
  - [Statement Ordering](#statement-ordering)
  - [Foreign Key Ordering](#foreign-key-ordering)
  - [Table Rebuild Procedure](#table-rebuild-procedure)
- [Execution Engine](#execution-engine)
  - [Three-Phase Execution](#three-phase-execution)
  - [Phase 1: Transactional DDL](#phase-1-transactional-ddl)
  - [Phase 2: Non-Transactional FTS](#phase-2-non-transactional-fts)
  - [Phase 3: Views and Triggers](#phase-3-views-and-triggers)
  - [View Fixed-Point Resolution](#view-fixed-point-resolution)
  - [Lease Verification Between Phases](#lease-verification-between-phases)
- [Safety Features](#safety-features)
  - [Destructive Change Protection](#destructive-change-protection)
  - [NOT NULL Column Validation](#not-null-column-validation)
  - [Foreign Key Integrity](#foreign-key-integrity)
  - [Schema Drift Detection](#schema-drift-detection)
  - [Migration Lease](#migration-lease)
  - [Crash Recovery](#crash-recovery)
  - [Atomic State Updates](#atomic-state-updates)
  - [Protected Table Namespace](#protected-table-namespace)
  - [AUTOINCREMENT Preservation](#autoincrement-preservation)
  - [Busy Timeout](#busy-timeout)
  - [Feature Preflight Validation](#feature-preflight-validation)
  - [Pre-Destructive Backup](#pre-destructive-backup)
  - [Pre-Destructive Hook](#pre-destructive-hook)
  - [Read-Only Guard](#read-only-guard)
- [CLI Reference](#cli-reference)
  - [extract](#cli-extract)
  - [validate](#cli-validate)
  - [diff](#cli-diff)
  - [plan](#cli-plan)
  - [check](#cli-check)
  - [apply](#cli-apply)
- [SQL Normalization](#sql-normalization)
- [Introspection](#introspection)
  - [Database Introspection](#database-introspection)
  - [Schema SQL Introspection](#schema-sql-introspection)
  - [Snapshot Caching](#snapshot-caching)
  - [Capability Detection](#capability-detection)
  - [DDL Generation](#ddl-generation)
- [Connection Abstraction](#connection-abstraction)
- [Error Reference](#error-reference)
- [Internal State](#internal-state)
- [Architecture](#architecture)
- [Supported Features](#supported-features)
- [Known Limitations](#known-limitations)
- [Testing](#testing)

---

## Introduction

turso-converge is a Rust library that provides **declarative schema convergence** for Turso databases. Instead of writing and maintaining numbered migration files (`001_up.sql`, `002_up.sql`, ...), you define your desired schema in a single SQL file and turso-converge automatically diffs the live database against your desired schema, generates a migration plan, and executes it.

A BLAKE3 hash fast-path makes subsequent checks near-instant (sub-millisecond) when the schema hasn't changed.

**Key design principles:**

- **Single source of truth.** Your schema is defined once, not scattered across migration files.
- **Idempotent.** Running convergence multiple times with the same schema is a no-op.
- **Safe by default.** Destructive changes (table drops, column drops) are blocked unless explicitly allowed.
- **Crash-safe.** Interrupted migrations are detected and re-converged automatically.
- **Observable.** Structured tracing at every decision point; detailed `ConvergeReport` on completion.

**Source code:** [`src/`](src/) — see [Architecture](#architecture) for the module map.

---

## Installation

turso-converge is distributed as a git dependency (not published to crates.io).

```toml
# Cargo.toml
[dependencies]
turso-converge = { git = "https://github.com/opentyler/turso-converge" }
turso = "0.6.0-pre.3"
turso_core = { version = "0.6.0-pre.3", features = ["fts", "fs"] }
tokio = { version = "1", features = ["full"] }
```

**Requirements:**

| Requirement | Version |
|-------------|---------|
| Rust edition | 2024 |
| Minimum Rust version | 1.88+ |
| Async runtime | Tokio (multi-thread flavor) |
| Database | Turso |

**Optional Turso builder flags** (required for FTS, materialized views, and triggers):

```rust
turso::Builder::new_local("my.db")
    .experimental_index_method(true)        // Required for FTS indexes
    .experimental_materialized_views(true)  // Required for materialized views
    .experimental_triggers(true)            // Required for triggers
    .build()
    .await?;
```

---

## Quick Start

```rust
use turso_converge::converge;

const SCHEMA: &str = r#"
    CREATE TABLE users (
        id TEXT PRIMARY KEY,
        email TEXT NOT NULL UNIQUE,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    );
    CREATE TABLE posts (
        id TEXT PRIMARY KEY,
        user_id TEXT NOT NULL REFERENCES users(id),
        title TEXT NOT NULL,
        body TEXT
    );
    CREATE INDEX idx_posts_user ON posts(user_id);
"#;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = turso::Builder::new_local("my.db")
        .experimental_index_method(true)
        .experimental_materialized_views(true)
        .experimental_triggers(true)
        .build()
        .await?;
    let conn = db.connect()?;

    // First call creates all tables and indexes.
    // Subsequent calls with the same schema are <1ms (BLAKE3 hash match).
    converge(&conn, SCHEMA).await?;

    Ok(())
}
```

For production use with safety controls, see [`converge_with_options`](#converge_with_options).

---

## Core Concepts

### Declarative Schema Convergence

Traditional migration systems require developers to write imperative migration scripts that transform the database from state A to state B. turso-converge inverts this: you declare the **desired end-state** of your schema, and the library figures out what DDL operations are needed to get there.

```
Traditional migrations:           Declarative convergence:
001_create_users.sql             schema.sql (desired state)
002_add_email_column.sql              ↓
003_create_posts.sql             converge() — library computes diff
004_add_index.sql                     ↓
                                 Automatic migration plan
```

### Fast Path vs Slow Path

turso-converge uses a two-tier change detection strategy:

**Fast path** (sub-millisecond): The schema SQL is normalized and hashed with BLAKE3. If the hash matches the stored hash in `_schema_meta` AND `PRAGMA schema_version` hasn't changed, convergence returns immediately with `ConvergeMode::FastPath`. This is the common case in production.

**Slow path** (milliseconds to seconds): If the hash doesn't match, or drift is detected, or a crash recovery flag is set, turso-converge performs full introspection of both the desired and actual schemas, computes a diff, generates a migration plan, and executes it.

```
converge() called
    ↓
Normalize schema SQL → BLAKE3 hash
    ↓
Compare hash against _schema_meta.schema_hash
    ↓
Check PRAGMA schema_version for drift
    ↓
Both match? → FastPath return (<1ms)
    ↓ (mismatch)
Acquire migration lease
    ↓
Build desired snapshot (in-memory DB)
    ↓
Introspect actual database
    ↓
Compute diff → Check policy → Generate plan → Execute
    ↓
Update hash + version atomically
```

### The Convergence Algorithm

The full convergence algorithm (implemented in [`src/converge.rs`](src/converge.rs)) proceeds through these stages:

1. **Validation**: Reject empty schema SQL and read-only connections.
2. **Hash check**: Normalize the schema SQL and compute a BLAKE3 hash. Compare against the stored hash.
3. **Drift detection**: Even if the hash matches, check `PRAGMA schema_version` against the stored value to detect out-of-band changes.
4. **Crash recovery**: Check the `migration_in_progress` flag. If set, force a full convergence.
5. **Pending data migrations**: If new `DataMigration` entries exist that haven't been applied, force the execution path.
6. **Lease acquisition**: Acquire a cooperative migration lease to prevent concurrent migrations.
7. **Introspection**: Build a `SchemaSnapshot` from the desired schema SQL (using an in-memory Turso database) and from the live database.
8. **Diff computation**: Compare the two snapshots across 15 categories.
9. **Policy check**: If destructive changes are present and the policy forbids them, return `PolicyViolation`.
10. **Backup hook**: If `backup_before_destructive` is set and destructive changes are present, write a SQL backup.
11. **Pre-destructive hook**: If a callback is registered, invoke it with the destructive change set.
12. **Feature validation**: Probe the connection for FTS, vector, and materialized view support. Fail if the schema requires features the connection lacks.
13. **Plan generation**: Generate an FK-ordered migration plan with transactional and non-transactional statement lists.
14. **Dry-run check**: If `dry_run` is true, return the plan SQL without executing.
15. **Execution**: Execute the plan in three phases (see [Execution Engine](#execution-engine)).
16. **Data migrations**: Apply any pending idempotent data migration steps.
17. **State update**: Atomically update the schema hash, clear the in-progress flag, and increment the schema version.
18. **Lease release**: Release the migration lease.

---

## API Reference

All public functions are re-exported from the crate root. Source: [`src/lib.rs`](src/lib.rs).

### `converge`

```rust
pub async fn converge(
    conn: &turso::Connection,
    schema_sql: &str,
) -> Result<(), MigrateError>
```

The simplest entry point. Converges the database to match `schema_sql` using a **permissive policy** (allows all changes including table drops and column drops). This is the backward-compatible API.

**Parameters:**
- `conn` — A Turso database connection.
- `schema_sql` — The desired schema as a SQL string containing CREATE TABLE, CREATE INDEX, CREATE VIEW, and CREATE TRIGGER statements.

**Returns:** `Ok(())` on success, or a `MigrateError` on failure.

**Behavior:**
- Internally calls `converge_with_options` with `ConvergePolicy::permissive()`.
- Discards the `ConvergeReport` — use `converge_with_options` if you need it.

**Example:**
```rust
let schema = include_str!("../schema.sql");
turso_converge::converge(&conn, schema).await?;
```

**Source:** [`src/converge.rs:13-20`](src/converge.rs)

---

### `converge_with_options`

```rust
pub async fn converge_with_options(
    conn: &turso::Connection,
    schema_sql: &str,
    options: &ConvergeOptions,
) -> Result<ConvergeReport, MigrateError>
```

The full-featured convergence API. Returns a detailed `ConvergeReport` and supports policy enforcement, dry-run mode, data migrations, rename hints, backup hooks, and pre-destructive callbacks.

**Parameters:**
- `conn` — A Turso database connection.
- `schema_sql` — The desired schema as a SQL string.
- `options` — A `ConvergeOptions` struct controlling behavior. See [ConvergeOptions](#convergeoptions).

**Returns:** A `ConvergeReport` on success detailing what changed, or a `MigrateError` on failure.

**Example (safe defaults):**
```rust
use turso_converge::{converge_with_options, ConvergeOptions};

let options = ConvergeOptions::default(); // blocks destructive changes
let report = converge_with_options(&conn, SCHEMA, &options).await?;
println!("Mode: {:?}, Duration: {:?}", report.mode, report.duration);
```

**Example (dry-run):**
```rust
use turso_converge::{converge_with_options, ConvergeOptions, ConvergePolicy};

let options = ConvergeOptions {
    policy: ConvergePolicy::permissive(),
    dry_run: true,
    ..Default::default()
};
let report = converge_with_options(&conn, SCHEMA, &options).await?;
for stmt in &report.plan_sql {
    println!("{stmt};");
}
```

**Source:** [`src/converge.rs:23-95`](src/converge.rs)

---

### `converge_from_path`

```rust
pub async fn converge_from_path(
    conn: &turso::Connection,
    path: impl AsRef<Path>,
) -> Result<(), MigrateError>
```

Reads schema SQL from a file on disk, then converges using the permissive policy. A convenience wrapper around `converge()`.

**Parameters:**
- `conn` — A Turso database connection.
- `path` — Path to a `.sql` file containing the desired schema.

**Returns:** `Ok(())` on success, `MigrateError::Io` if the file can't be read.

**Example:**
```rust
turso_converge::converge_from_path(&conn, "schemas/schema.sql").await?;
```

**Source:** [`src/converge.rs:561-573`](src/converge.rs)

---

### `converge_multi`

```rust
pub async fn converge_multi(
    conn: &turso::Connection,
    schema_parts: &[&str],
) -> Result<(), MigrateError>
```

Compose schema from multiple SQL fragments (concatenated in order with newline separators), then converge as one unified schema using the permissive policy.

**Parameters:**
- `conn` — A Turso database connection.
- `schema_parts` — Slice of SQL strings to concatenate.

**Example:**
```rust
let parts = [
    include_str!("schemas/core.sql"),
    include_str!("schemas/indexes.sql"),
    include_str!("schemas/views.sql"),
];
turso_converge::converge_multi(&conn, &parts).await?;
```

**Source:** [`src/converge.rs:531-537`](src/converge.rs)

---

### `converge_multi_with_options`

```rust
pub async fn converge_multi_with_options(
    conn: &turso::Connection,
    schema_parts: &[&str],
    options: &ConvergeOptions,
) -> Result<ConvergeReport, MigrateError>
```

Like `converge_multi` but with full options support and a `ConvergeReport` return value.

**Source:** [`src/converge.rs:540-547`](src/converge.rs)

---

### `validate_schema`

```rust
pub async fn validate_schema(schema_sql: &str) -> Result<(), MigrateError>
```

Validates schema SQL by executing it against an in-memory Turso database. Does not modify any existing database. Useful for CI pipelines to catch schema errors early.

**Parameters:**
- `schema_sql` — The schema SQL to validate.

**Returns:** `Ok(())` if valid, or `MigrateError::Schema` with a descriptive message if invalid.

**Example:**
```rust
turso_converge::validate_schema(include_str!("../schema.sql")).await?;
```

**Source:** [`src/converge.rs:521-528`](src/converge.rs)

---

### `schema_version`

```rust
pub async fn schema_version(conn: &turso::Connection) -> Result<u32, MigrateError>
```

Returns the schema version counter from the `schema_version` table. This counter is incremented atomically each time DDL is applied by turso-converge.

**Returns:** The current version as a `u32`, or `0` if no version row exists.

**Source:** [`src/converge.rs:576-588`](src/converge.rs)

---

### `rollback_to_previous`

```rust
pub async fn rollback_to_previous(conn: &turso::Connection) -> Result<(), MigrateError>
```

Re-converges the database to the schema snapshot stored before the last migration. The previous schema is stored in `_schema_meta` under the key `previous_schema_sql` and is saved automatically before each DDL execution.

**Limitations:**
- Only single-step rollback is supported (the most recent prior schema, not arbitrary historical versions).
- Returns `MigrateError::Schema` if no previous schema is stored (i.e., the database has never been migrated).

**Source:** [`src/converge.rs:509-518`](src/converge.rs)

---

### `is_read_only`

```rust
pub async fn is_read_only(conn: &turso::Connection) -> Result<bool, MigrateError>
```

Returns `true` if the connection is in read-only or replica mode (checks `PRAGMA query_only`). All convergence operations require write access and will return `MigrateError::ReadOnly` on read-only connections.

**Source:** [`src/converge.rs:550-558`](src/converge.rs)

---

### `compute_diff`

```rust
pub fn compute_diff(
    desired: &SchemaSnapshot,
    actual: &SchemaSnapshot,
) -> SchemaDiff
```

Computes the diff between two schema snapshots without column rename hints. This is a synchronous function (no database access). See [Diff Engine](#diff-engine) for details.

**Source:** [`src/diff.rs:253-255`](src/diff.rs)

---

### `generate_plan`

```rust
pub fn generate_plan(
    diff: &SchemaDiff,
    desired: &SchemaSnapshot,
    actual: &SchemaSnapshot,
) -> Result<MigrationPlan, MigrateError>
```

Generates an FK-ordered migration plan from a schema diff. Returns a `MigrationPlan` with transactional and non-transactional statement lists. This is a synchronous function.

**Errors:** Returns `MigrateError::Schema` if a table rebuild would add a NOT NULL column without a DEFAULT value.

**Source:** [`src/plan.rs:29-279`](src/plan.rs)

---

### `execute_plan`

```rust
pub async fn execute_plan(
    conn: &turso::Connection,
    plan: &MigrationPlan,
) -> Result<(), MigrateError>
```

Executes a migration plan with the default 5-second busy timeout and no lease verification. This is a lower-level API; most users should use `converge` or `converge_with_options` instead.

**Source:** [`src/execute.rs:7-12`](src/execute.rs)

---

## Configuration Reference

### ConvergeOptions

Full configuration for `converge_with_options`. Source: [`src/options.rs:110-121`](src/options.rs).

```rust
pub struct ConvergeOptions {
    pub policy: ConvergePolicy,
    pub dry_run: bool,
    pub busy_timeout: Duration,
    pub max_retries: u32,
    pub backup_before_destructive: Option<PathBuf>,
    pub data_migrations: Vec<DataMigration>,
    pub rename_hints: Vec<ColumnRenameHint>,
    pub pre_destructive_hook: Option<PreDestructiveHook>,
    pub failpoint: Option<Failpoint>,
    pub capabilities: Option<Capabilities>,
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `policy` | `ConvergePolicy` | `ConvergePolicy::default()` | Controls which destructive changes are allowed. |
| `dry_run` | `bool` | `false` | If `true`, compute the plan but don't execute it. The plan SQL is returned in `ConvergeReport.plan_sql`. |
| `busy_timeout` | `Duration` | `5 seconds` | Applied as `PRAGMA busy_timeout` before executing the migration plan. Prevents busy errors. |
| `max_retries` | `u32` | `3` | Maximum number of retries for transient failures. |
| `backup_before_destructive` | `Option<PathBuf>` | `None` | If set and destructive changes are detected, writes the current schema DDL to this path before executing. If the path is a directory, creates a timestamped file inside it. |
| `data_migrations` | `Vec<DataMigration>` | `vec![]` | Idempotent post-DDL data migration steps. Each is tracked by ID in `_schema_meta`. |
| `rename_hints` | `Vec<ColumnRenameHint>` | `vec![]` | Explicit hints for column rename detection when heuristics are ambiguous. |
| `pre_destructive_hook` | `Option<PreDestructiveHook>` | `None` | Callback invoked before destructive changes are executed. Return `Err(message)` to reject. |
| `failpoint` | `Option<Failpoint>` | `None` | Test-only crash injection point. Should never be set in production. |
| `capabilities` | `Option<Capabilities>` | `None` | Pre-detected capabilities to skip runtime probing. When `None` (default), capabilities are auto-detected via `Capabilities::detect()`. When `Some`, the provided capabilities are used directly. |

The `PreDestructiveHook` type is defined as:

```rust
pub type PreDestructiveHook =
    Arc<dyn Fn(&DestructiveChangeSet) -> Result<(), String> + Send + Sync + 'static>;
```

---

### ConvergePolicy

Controls which destructive changes are allowed during convergence. Source: [`src/options.rs:79-107`](src/options.rs).

```rust
pub struct ConvergePolicy {
    pub allow_table_drops: bool,
    pub allow_column_drops: bool,
    pub allow_table_rebuilds: bool,
    pub max_tables_affected: Option<usize>,
}
```

| Field | Default | Permissive | Description |
|-------|---------|------------|-------------|
| `allow_table_drops` | `false` | `true` | Whether tables present in the database but absent from the schema may be dropped. |
| `allow_column_drops` | `false` | `true` | Whether columns present in the database but absent from the schema may be dropped. |
| `allow_table_rebuilds` | `true` | `true` | Whether table rebuilds (the 12-step ALTER TABLE procedure) are allowed. |
| `max_tables_affected` | `None` | `None` | Optional upper bound on the number of tables affected (created + dropped + rebuilt). |

**Constructors:**

- `ConvergePolicy::default()` — Safe defaults. Blocks table drops and column drops.
- `ConvergePolicy::permissive()` — Allows all changes. Used by `converge()`.

When a policy violation is detected, `converge_with_options` returns `Err(MigrateError::PolicyViolation { message, blocked_operations })`.

---

### ConvergeReport

Post-convergence report returned by `converge_with_options`. Source: [`src/options.rs:159-173`](src/options.rs).

```rust
pub struct ConvergeReport {
    pub mode: ConvergeMode,
    pub tables_created: usize,
    pub tables_rebuilt: usize,
    pub tables_dropped: usize,
    pub columns_added: usize,
    pub columns_dropped: usize,
    pub columns_renamed: usize,
    pub indexes_changed: usize,
    pub views_changed: usize,
    pub data_migrations_applied: usize,
    pub duration: Duration,
    pub plan_sql: Vec<String>,
}
```

| Field | Description |
|-------|-------------|
| `mode` | How the convergence completed — see [ConvergeMode](#convergemode). |
| `tables_created` | Number of new tables created. |
| `tables_rebuilt` | Number of existing tables rebuilt via the 12-step procedure. |
| `tables_dropped` | Number of tables dropped. |
| `columns_added` | Number of columns added via `ALTER TABLE ADD COLUMN`. |
| `columns_dropped` | Number of columns dropped via `ALTER TABLE DROP COLUMN`. |
| `columns_renamed` | Number of columns renamed via `ALTER TABLE RENAME COLUMN`. |
| `indexes_changed` | Number of indexes created or recreated (standard + FTS). |
| `views_changed` | Number of views created or recreated. |
| `data_migrations_applied` | Number of data migration steps applied in this run. |
| `duration` | Wall-clock time for the entire convergence operation. |
| `plan_sql` | Populated only in dry-run mode — contains the SQL statements that would be executed. |

**Methods:**

- `ConvergeReport::fast_path(duration)` — Creates a report with `ConvergeMode::FastPath` and all counters at zero.
- `report.had_changes()` — Returns `true` if any counter is non-zero.

---

### ConvergeMode

How the convergence completed. Source: [`src/options.rs:196-202`](src/options.rs).

```rust
pub enum ConvergeMode {
    FastPath,
    SlowPath,
    CrashRecovery,
    NoOp,
    DryRun,
}
```

| Variant | Description |
|---------|-------------|
| `FastPath` | Schema hash and PRAGMA schema_version both matched. No work done. Sub-millisecond. |
| `SlowPath` | Full diff was computed and DDL was executed, or data migrations were applied. |
| `CrashRecovery` | A previous migration was interrupted. Full re-convergence was performed. |
| `NoOp` | Slow path ran (hash mismatch triggered introspection) but no actual changes were needed. |
| `DryRun` | `dry_run: true` was set. Plan was computed but not executed. |

---

### DataMigration

Idempotent post-DDL data migration step. Source: [`src/options.rs:15-19`](src/options.rs).

```rust
pub struct DataMigration {
    pub id: String,
    pub statements: Vec<String>,
}
```

Data migrations run after schema convergence and are tracked in `_schema_meta` under the key `data_migration:{id}`. Once applied, a migration ID is never re-applied, even on subsequent calls.

**Rules:**
- The `id` must not be empty (returns `MigrateError::Schema` if it is).
- Each migration's statements run inside a `BEGIN IMMEDIATE` / `COMMIT` transaction.
- If any statement fails, the transaction is rolled back and a `MigrateError::Statement` is returned.
- Data migrations run even on the fast path if there are pending (unapplied) migrations.

**Example:**
```rust
use turso_converge::{ConvergeOptions, ConvergePolicy, DataMigration};

let options = ConvergeOptions {
    policy: ConvergePolicy::permissive(),
    data_migrations: vec![
        DataMigration {
            id: "seed-admin-user".to_string(),
            statements: vec![
                "INSERT OR IGNORE INTO users (id, email) VALUES ('admin', 'admin@example.com')".to_string(),
            ],
        },
        DataMigration {
            id: "backfill-created-at".to_string(),
            statements: vec![
                "UPDATE users SET created_at = datetime('now') WHERE created_at IS NULL".to_string(),
            ],
        },
    ],
    ..Default::default()
};
```

---

### ColumnRenameHint

Explicit hint for column rename detection. Source: [`src/options.rs:7-12`](src/options.rs).

```rust
pub struct ColumnRenameHint {
    pub table: String,
    pub from: String,
    pub to: String,
}
```

When the automatic rename heuristic can't determine that a column was renamed (e.g., type or position changed simultaneously), provide an explicit hint. The hint is only applied if both columns are compatible for renaming (same type, nullability, primary key status, and collation).

**Example:**
```rust
use turso_converge::ColumnRenameHint;

let hints = vec![ColumnRenameHint {
    table: "users".to_string(),
    from: "name".to_string(),
    to: "display_name".to_string(),
}];
```

---

### DestructiveChangeSet

Destructive changes detected in a migration plan. Source: [`src/options.rs:22-55`](src/options.rs).

```rust
pub struct DestructiveChangeSet {
    pub tables_to_drop: Vec<String>,
    pub columns_to_drop: Vec<(String, String)>,  // (table_name, column_name)
    pub tables_to_rebuild: Vec<String>,
}
```

**Methods:**
- `has_changes()` — Returns `true` if any destructive changes are present.
- `blocked_operations()` — Returns a `Vec<String>` of human-readable operation descriptions (e.g., `"DROP TABLE users"`, `"DROP COLUMN users.name"`).

This struct is passed to the `pre_destructive_hook` callback and used for backup decisions.

---

### Failpoint

Test-only crash injection points for verifying crash recovery. Source: [`src/options.rs:61-76`](src/options.rs).

```rust
pub enum Failpoint {
    BeforeIntrospect,
    BeforeExecute,
    AfterExecuteBeforeState,
}
```

| Variant | When it triggers | Use case |
|---------|-----------------|----------|
| `BeforeIntrospect` | Before schema introspection begins | Tests crash before any state changes |
| `BeforeExecute` | After plan generation, before DDL execution | Tests crash with in-progress flag set |
| `AfterExecuteBeforeState` | After DDL execution, before state update | Tests crash with DDL applied but state not updated |

**Never set this in production.** When triggered, returns `MigrateError::InjectedFailure`.

---

## Schema Type System

turso-converge uses a rich intermediate schema representation. All types are in [`src/schema.rs`](src/schema.rs).

### SchemaSnapshot

Complete database schema representation. Source: [`src/schema.rs:72-78`](src/schema.rs).

```rust
pub struct SchemaSnapshot {
    pub tables: BTreeMap<CIString, TableInfo>,
    pub indexes: BTreeMap<CIString, IndexInfo>,
    pub views: BTreeMap<CIString, ViewInfo>,
    pub triggers: BTreeMap<CIString, TriggerInfo>,
}
```

All maps use `CIString` keys for case-insensitive lookup (matching Turso's case-insensitive identifier semantics).

**Construction methods** (in [`src/introspect.rs`](src/introspect.rs)):

| Method | Description |
|--------|-------------|
| `SchemaSnapshot::from_connection(conn)` | Introspect a live database connection. |
| `SchemaSnapshot::from_schema_sql(sql)` | Execute schema SQL against an in-memory Turso database and introspect the result. Results are cached by BLAKE3 hash. |
| `SchemaSnapshot::validate(sql)` | Execute schema SQL against an in-memory database to verify syntax (no snapshot returned). |

**Lookup methods:**

| Method | Description |
|--------|-------------|
| `get_table(name)` | Case-insensitive table lookup by name. |
| `get_index(name)` | Case-insensitive index lookup by name. |
| `get_view(name)` | Case-insensitive view lookup by name. |
| `get_trigger(name)` | Case-insensitive trigger lookup by name. |
| `has_table(name)` | Returns `true` if a table with that name exists. |
| `has_index(name)` | Returns `true` if an index with that name exists. |
| `has_view(name)` | Returns `true` if a view with that name exists. |
| `has_trigger(name)` | Returns `true` if a trigger with that name exists. |

**Output method:**

| Method | Description |
|--------|-------------|
| `to_sql()` | Generate deterministic DDL. Tables are topologically sorted by foreign key dependencies. Standard indexes come first, then FTS indexes, then views, then triggers. |

---

### TableInfo

Parsed table metadata. Source: [`src/schema.rs:116-126`](src/schema.rs).

```rust
pub struct TableInfo {
    pub name: String,
    pub sql: String,
    pub columns: Vec<ColumnInfo>,
    pub foreign_keys: Vec<ForeignKey>,
    pub check_constraints: Vec<String>,
    pub is_strict: bool,
    pub is_without_rowid: bool,
    pub has_autoincrement: bool,
}
```

| Field | Description |
|-------|-------------|
| `name` | Table name as it appears in the system catalog (`sqlite_schema`). |
| `sql` | The full `CREATE TABLE` DDL statement. |
| `columns` | Column metadata from `PRAGMA table_xinfo`. |
| `foreign_keys` | Foreign key constraints from `PRAGMA foreign_key_list` (with SQL-based fallback). |
| `check_constraints` | CHECK constraint expressions parsed from the DDL, normalized via `normalize_sql`. Includes both column-level (`CHECK(val > 0)`) and table-level (`CHECK(a != b)`) constraints. Sorted for stable comparison. |
| `is_strict` | Whether the table uses `STRICT` mode. Detected from the DDL suffix. |
| `is_without_rowid` | Whether the table uses `WITHOUT ROWID`. Detected from DDL. |
| `has_autoincrement` | Whether any column uses `AUTOINCREMENT`. Detected from DDL. |

**Methods:**
- `referenced_tables()` — Returns a `BTreeSet<String>` of table names referenced by this table's foreign keys.

---

### ColumnInfo

Column metadata from `PRAGMA table_xinfo`. Source: [`src/schema.rs:138-151`](src/schema.rs).

```rust
pub struct ColumnInfo {
    pub name: String,
    pub col_type: String,
    pub notnull: bool,
    pub default_value: Option<String>,
    pub pk: i64,
    pub collation: Option<String>,
    pub is_generated: bool,
    pub is_hidden: bool,
}
```

| Field | Description |
|-------|-------------|
| `name` | Column name. |
| `col_type` | Column type affinity (e.g., `"TEXT"`, `"INTEGER"`, `"vector32(768)"`). |
| `notnull` | Whether the column has a `NOT NULL` constraint. |
| `default_value` | The `DEFAULT` expression, if any, as a string. |
| `pk` | Primary key index (0 if not a primary key, 1+ for composite PKs). |
| `collation` | COLLATE clause (e.g., `"NOCASE"`, `"BINARY"`). Extracted from DDL via pattern matching. |
| `is_generated` | Whether this is a `GENERATED ALWAYS AS` column (`table_xinfo` hidden value 2 or 3). |
| `is_hidden` | Whether this column is hidden (`table_xinfo` hidden value 1). |

**Methods:**
- `is_insertable()` — Returns `true` if the column can be the target of an INSERT statement. Generated and hidden columns are not insertable.

---

### IndexInfo

Index metadata. Source: [`src/schema.rs:172-180`](src/schema.rs).

```rust
pub struct IndexInfo {
    pub name: String,
    pub table_name: String,
    pub sql: String,
    pub is_fts: bool,
    pub is_unique: bool,
    pub columns: Vec<String>,
}
```

| Field | Description |
|-------|-------------|
| `name` | Index name from the system catalog (`sqlite_schema`). |
| `table_name` | Name of the table this index belongs to. |
| `sql` | The full `CREATE INDEX` DDL. |
| `is_fts` | `true` if the index SQL contains `USING fts` (Turso's tantivy FTS). |
| `is_unique` | `true` if the index enforces uniqueness (from `PRAGMA index_list`). |
| `columns` | Column names included in the index (from `PRAGMA index_info`). |

---

### ViewInfo

View metadata. Source: [`src/schema.rs:183-188`](src/schema.rs).

```rust
pub struct ViewInfo {
    pub name: String,
    pub sql: String,
    pub is_materialized: bool,
}
```

| Field | Description |
|-------|-------------|
| `name` | View name from the system catalog (`sqlite_schema`). |
| `sql` | The full `CREATE VIEW` or `CREATE MATERIALIZED VIEW` DDL. |
| `is_materialized` | `true` if the DDL starts with `CREATE MATERIALIZED VIEW`. |

---

### TriggerInfo

Trigger metadata. Source: [`src/schema.rs:191-196`](src/schema.rs).

```rust
pub struct TriggerInfo {
    pub name: String,
    pub table_name: String,
    pub sql: String,
}
```

| Field | Description |
|-------|-------------|
| `name` | Trigger name from the system catalog (`sqlite_schema`). |
| `table_name` | Name of the table this trigger is attached to. |
| `sql` | The full `CREATE TRIGGER` DDL. |

---

### ForeignKey

Foreign key constraint. Source: [`src/schema.rs:162-169`](src/schema.rs).

```rust
pub struct ForeignKey {
    pub from_columns: Vec<String>,
    pub to_table: String,
    pub to_columns: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
}
```

Foreign keys are detected via `PRAGMA foreign_key_list`. If that returns no results (e.g., on connections where FK enforcement is disabled), turso-converge falls back to parsing `REFERENCES` clauses directly from the `CREATE TABLE` SQL. The fallback parser is string-literal-aware and skips references inside comments and quoted strings.

---

### Capabilities

Detected capabilities of the target Turso database connection. Source: [`src/schema.rs:199-212`](src/schema.rs).

```rust
pub struct Capabilities {
    pub database_version: (u32, u32, u32),
    pub supports_drop_column: bool,     // >= 3.35.0
    pub supports_rename_column: bool,   // >= 3.25.0
    pub has_fts_module: bool,
    pub has_vector_module: bool,
    pub has_materialized_views: bool,
    pub supports_without_rowid: bool,
    pub supports_generated_columns: bool,
    pub has_triggers: bool,
}
```

Capabilities are detected via runtime probing (see [Capability Detection](#capability-detection)). The default values assume no experimental features are available:

```rust
Capabilities {
    database_version: (3, 45, 0),
    supports_drop_column: true,
    supports_rename_column: true,
    has_fts_module: false,
    has_vector_module: false,
    has_materialized_views: false,
    supports_without_rowid: false,
    supports_generated_columns: false,
    has_triggers: false,
}
```

---

### CIString

Case-insensitive string wrapper matching Turso's identifier semantics. Source: [`src/schema.rs:16-19`](src/schema.rs).

```rust
pub struct CIString {
    raw: String,    // Original spelling
    lower: String,  // Pre-computed ASCII-lowercase
}
```

`Ord`, `Eq`, and `Hash` operate on the lowercase form. `Display` and `.raw()` return the original spelling for DDL output and error messages.

**Methods:**
- `CIString::new(s)` — Construct from any `impl Into<String>`.
- `.raw()` — Original spelling.
- `.lower()` — Lowercase form.

---

## Diff Engine

The diff engine computes categorized differences between two `SchemaSnapshot` instances. Source: [`src/diff.rs`](src/diff.rs).

### SchemaDiff

```rust
pub struct SchemaDiff {
    pub tables_to_create: Vec<String>,
    pub tables_to_drop: Vec<String>,
    pub tables_to_rebuild: Vec<String>,
    pub columns_to_add: Vec<(String, ColumnInfo)>,
    pub columns_to_drop: Vec<(String, String)>,
    pub columns_to_rename: Vec<(String, String, String)>,  // (table, from, to)
    pub indexes_to_create: Vec<String>,
    pub indexes_to_drop: Vec<String>,
    pub fts_indexes_to_create: Vec<String>,
    pub fts_indexes_to_drop: Vec<String>,
    pub views_to_create: Vec<String>,
    pub views_to_drop: Vec<String>,
    pub triggers_to_create: Vec<String>,
    pub triggers_to_drop: Vec<String>,
}
```

**Methods:**
- `is_empty()` — Returns `true` if all categories are empty.
- `Display` — Human-readable output using `+` (create), `-` (drop), `~` (rebuild/rename) prefixes.

**Display format:**
```
+ TABLE users
+ TABLE posts
~ TABLE documents: REBUILD
- TABLE legacy_cache
+ COLUMN users.bio TEXT
- COLUMN users.old_field
~ COLUMN users.name -> display_name
+ INDEX idx_posts_user
+ FTS INDEX idx_docs_fts
+ VIEW mv_counts
+ TRIGGER trg_audit
```

### Diff Categories

The diff engine evaluates 15 categories across 4 object types:

| # | Category | Condition |
|---|----------|-----------|
| 1 | `tables_to_create` | Table in desired but not in actual |
| 2 | `tables_to_drop` | Table in actual but not in desired |
| 3 | `tables_to_rebuild` | Table exists in both but has incompatible column changes (type, nullability, default, PK, collation, generated status, or CHECK constraints changed) |
| 4 | `columns_to_add` | Column in desired table but not in actual, and eligible for `ALTER TABLE ADD COLUMN` |
| 5 | `columns_to_drop` | Column in actual table but not in desired, and eligible for `ALTER TABLE DROP COLUMN` |
| 6 | `columns_to_rename` | Column detected as renamed (via heuristic or explicit hint) |
| 7 | `indexes_to_create` | Standard index in desired but not in actual, or SQL changed, or parent table rebuilt |
| 8 | `indexes_to_drop` | Standard index in actual but not in desired, or being recreated |
| 9 | `fts_indexes_to_create` | FTS index in desired but not in actual, or SQL changed |
| 10 | `fts_indexes_to_drop` | FTS index in actual but not in desired, or being recreated |
| 11 | `views_to_create` | View in desired but not in actual, or SQL changed |
| 12 | `views_to_drop` | View in actual but not in desired, or being recreated |
| 13 | `triggers_to_create` | Trigger in desired but not in actual, or SQL changed |
| 14 | `triggers_to_drop` | Trigger in actual but not in desired, or being recreated |

### Column Change Classification

When a table exists in both desired and actual schemas, the diff engine classifies each column difference:

**Triggers a table rebuild:**
- CHECK constraints changed (added, removed, or modified — compared as normalized, sorted lists)
- Column type changed (case-insensitive comparison)
- Nullability changed (`NOT NULL` added or removed)
- Default value changed (compared via SQL normalization)
- Primary key status changed
- Collation changed
- Generated status changed
- New column is NOT NULL without DEFAULT (can't use ADD COLUMN)
- New column is a primary key (can't use ADD COLUMN)
- New column is GENERATED (can't use ADD COLUMN)
- Dropped column is a primary key, indexed, referenced by a foreign key, used in a view, or has a trigger on its table

**Uses `ALTER TABLE ADD COLUMN` (O(1), no rebuild):**
- New column is nullable OR has a DEFAULT value
- New column is not a primary key
- New column is not GENERATED

**Uses `ALTER TABLE DROP COLUMN` (O(1), when eligible):**
- Dropped column is not a primary key (`pk == 0`)
- Dropped column is not indexed
- Dropped column is not referenced by any foreign key (from or to)
- Dropped column's table has no triggers
- Dropped column's table is not referenced in any view

### Column Rename Detection

turso-converge detects column renames through two mechanisms:

**1. Explicit hints** (via `ColumnRenameHint`):
Hints are processed first. A hint is applied only if both old and new columns are "compatible for rename" — same type, nullability, primary key status, collation, default value, and neither is generated or hidden.

**2. Automatic heuristic** (conservative):
After processing hints, the engine attempts automatic detection on remaining unmatched add/drop pairs. A rename is detected only when:
- There is an unambiguous 1:1 match between an added and a dropped column
- Both columns are compatible for rename (same type, constraints, etc.)
- Both columns are at the same ordinal position in their respective tables
- No other column maps to the same position

If multiple candidates exist or positions don't match, the automatic heuristic does not detect a rename — use `ColumnRenameHint` for those cases.

### Index and View Diffing

**Indexes** are compared by normalized SQL. If the SQL changed OR the parent table is being rebuilt, the index is dropped and recreated. This includes **partial indexes** — a change to the `WHERE` clause (e.g., `WHERE deleted = 0` → `WHERE deleted = 0 AND active = 1`) is detected as a SQL change and triggers drop + recreate. FTS indexes are tracked separately because they require non-transactional execution.

**Views** are compared by normalized SQL. If the SQL changed, the view is dropped and recreated. When any table is rebuilt, ALL views are dropped and recreated (because views may reference rebuilt tables).

**Triggers** follow the same pattern as views — compared by normalized SQL, and triggers on rebuilt tables are dropped and recreated.

---

## Migration Planning

The plan generator produces ordered DDL statements from a schema diff. Source: [`src/plan.rs`](src/plan.rs).

### MigrationPlan

```rust
pub struct MigrationPlan {
    pub new_tables: Vec<String>,
    pub altered_tables: Vec<String>,
    pub rebuilt_tables: Vec<String>,
    pub new_indexes: Vec<String>,
    pub changed_indexes: Vec<String>,
    pub new_views: Vec<String>,
    pub changed_views: Vec<String>,
    pub transactional_stmts: Vec<String>,
    pub non_transactional_stmts: Vec<String>,
}
```

The metadata fields (`new_tables`, `altered_tables`, etc.) describe what changed. The execution fields (`transactional_stmts`, `non_transactional_stmts`) contain the actual SQL statements.

### Statement Ordering

Transactional statements are ordered as follows:

1. `DROP TRIGGER IF EXISTS ...` — Drop triggers first (they reference tables)
2. `DROP VIEW IF EXISTS ...` — Drop views (they reference tables)
3. `DROP INDEX IF EXISTS ...` — Drop standard indexes
4. `DROP TABLE IF EXISTS ...` — Drop tables (skipping protected tables)
5. `CREATE TABLE ...` — Create new tables (FK-ordered)
6. `ALTER TABLE ... RENAME COLUMN ...` — Rename columns
7. `ALTER TABLE ... ADD COLUMN ...` — Add columns
8. `ALTER TABLE ... DROP COLUMN ...` — Drop columns
9. Table rebuild sequence (per rebuilt table):
   - Drop any existing temp table
   - Save AUTOINCREMENT sequence (if applicable)
   - `CREATE TABLE "_converge_new_{table}_{seq}" ...` — Create temp table with desired schema
   - `INSERT INTO ... SELECT ...` — Copy data (generated columns excluded, defaults for new columns)
   - `DROP TABLE "{table}"` — Drop original
   - `ALTER TABLE "_converge_new_..." RENAME TO "{table}"` — Rename temp to original
   - Restore AUTOINCREMENT sequence (if applicable)
10. `CREATE INDEX ...` — Create standard indexes
11. `CREATE VIEW ...` / `CREATE MATERIALIZED VIEW ...` — Create views (dependency-ordered)
12. `CREATE TRIGGER ...` — Create triggers

Non-transactional statements (FTS indexes):
1. `DROP INDEX IF EXISTS ...` — Drop FTS indexes
2. `CREATE INDEX ... USING fts ...` — Create FTS indexes

### Foreign Key Ordering

New tables are topologically sorted by foreign key dependencies. A table is created only after all tables it references are either already in the database or have been created in the current plan. If a dependency cycle is detected, remaining tables are appended in alphabetical order.

Views are similarly sorted: a view is created only after all views it depends on have been created.

### Table Rebuild Procedure

turso-converge follows SQLite's [12-step ALTER TABLE procedure](https://www.sqlite.org/lang_altertable.html#otheralter) with safety enhancements:

| Step | turso-converge | Safety Enhancement |
|---|---|---|
| 1. Disable FK constraints | `PRAGMA defer_foreign_keys = ON` | Defers rather than disables — violations are still caught at COMMIT |
| 2. Begin transaction | `BEGIN IMMEDIATE` | Write lock prevents concurrent DDL |
| 3. Remember schema objects | `SchemaSnapshot::from_connection()` | Full introspection via `PRAGMA table_xinfo` |
| 4. Create new table | `CREATE TABLE "_converge_new_{table}_{seq}"` | Unique sequenced name prevents collisions |
| 5. Copy data | `INSERT INTO ... SELECT ...` | Generated columns excluded, defaults for new columns |
| 6. Drop old table | `DROP TABLE` | Protected tables never dropped |
| 7. Rename new to old | `ALTER TABLE ... RENAME TO` | |
| 8-9. Recreate objects | Auto-detected by diff engine | Views use fixed-point ordering for dependencies |
| 10. FK check | `PRAGMA foreign_key_check` | Returns structured `ForeignKeyViolation` error |
| 11. Commit | `COMMIT` | Atomic with hash + version update |
| 12. Restore FK mode | Automatic | `defer_foreign_keys` is per-transaction |

---

## Execution Engine

The execution engine runs migration plans in three phases. Source: [`src/execute.rs`](src/execute.rs).

### Three-Phase Execution

```
Phase 1 (transactional):
  PRAGMA defer_foreign_keys = ON → BEGIN IMMEDIATE →
  DROP triggers → DROP views → DROP indexes → DROP tables →
  CREATE tables (FK-ordered) → RENAME COLUMN → ADD COLUMN → DROP COLUMN →
  Table rebuilds → CREATE indexes → PRAGMA foreign_key_check → COMMIT

Phase 2 (non-transactional):
  DROP FTS indexes → CREATE FTS indexes
  (FTS operations use execute_batch and cannot run inside a transaction)

Phase 3 (transactional):
  CREATE views (fixed-point retry for unresolved deps) →
  CREATE triggers
```

### Phase 1: Transactional DDL

All DDL operations except FTS indexes and view/trigger creation are executed in a single `BEGIN IMMEDIATE` / `COMMIT` transaction. If any statement fails, the transaction is rolled back and a `MigrateError::Statement` is returned with the failing SQL, the underlying error, and the phase label `"DDL"`.

If the plan includes table rebuilds, `PRAGMA defer_foreign_keys = ON` is set before the transaction and `PRAGMA foreign_key_check` runs before COMMIT to catch FK violations.

### Phase 2: Non-Transactional FTS

Turso's FTS index operations (using the tantivy engine) cannot run inside a transaction. They are executed individually via `execute_batch`. Each statement is terminated with a semicolon if not already present.

### Phase 3: Views and Triggers

View and trigger creation is separated from Phase 1 because:
- Views may depend on tables that were just created/rebuilt in Phase 1
- Views may depend on other views (handled by fixed-point resolution)
- Triggers may depend on views

### View Fixed-Point Resolution

Views with inter-dependencies are resolved using a fixed-point algorithm:

1. Attempt to create all remaining views in a transaction.
2. If a view fails with "no such table" or "no such view", defer it to the next round.
3. If at least one view succeeded, commit and start a new round with the deferred views.
4. If no views succeeded in a round, abort with a `MigrateError::Schema` describing unresolvable dependencies.
5. Maximum rounds = number of views + 1.

After all views are created, triggers are created in a separate transaction.

### Lease Verification Between Phases

Between each execution phase, the migration lease is verified and refreshed via an atomic `UPDATE` statement:

```sql
UPDATE _schema_meta SET value = ?1
WHERE key = 'migration_lease_until'
  AND CAST(value AS INTEGER) > ?2
  AND EXISTS (
      SELECT 1 FROM _schema_meta owner
      WHERE owner.key = 'migration_owner' AND owner.value = ?3
  )
```

If the update affects 0 rows, the lease was lost or expired, and the migration aborts immediately to prevent concurrent DDL corruption.

---

## Safety Features

### Destructive Change Protection

`converge_with_options` with default `ConvergePolicy` blocks table drops and column drops. If an accidental schema edit would remove a table or column, you get a clear `PolicyViolation` error instead of data loss.

```rust
// Default policy blocks destructive changes:
let policy = ConvergePolicy::default();
// allow_table_drops: false
// allow_column_drops: false
// allow_table_rebuilds: true
// max_tables_affected: None
```

The policy check runs after diff computation but before any DDL executes.

### NOT NULL Column Validation

Adding a `NOT NULL` column without a `DEFAULT` value to an existing table is caught **before** any DDL executes. This applies both to `ADD COLUMN` (which Turso itself would reject) and table rebuilds (where existing rows would violate the constraint).

The error message specifies which column on which table needs a default:
```
Table 'users' rebuild would add NOT NULL column 'display_name' without DEFAULT.
Existing rows would violate the constraint.
Add a DEFAULT value to the column definition.
```

Source: [`src/plan.rs:367-393`](src/plan.rs) (`validate_rebuild_safety`)

### Foreign Key Integrity

Three mechanisms ensure FK integrity:

1. **Deferred FK checks:** `PRAGMA defer_foreign_keys = ON` before table rebuilds prevents failures with self-referential and cyclic foreign keys during the rebuild transaction.
2. **Post-rebuild validation:** `PRAGMA foreign_key_check` runs after all rebuilds but before COMMIT. Returns a structured `ForeignKeyViolation` error with the table, rowid, and referenced parent.
3. **FK dependency ordering:** New tables are topologically sorted by FK dependencies so referenced tables are created before referencing tables.

### Schema Drift Detection

Even when the desired schema hash matches the stored hash, turso-converge checks `PRAGMA schema_version` against the stored value in `_schema_meta`. This detects out-of-band changes made by:
- Manual SQL execution
- Admin operations
- Replica divergence
- Other migration tools

If drift is detected, turso-converge forces a full convergence to correct the database.

Source: [`src/converge.rs:420-444`](src/converge.rs) (`detect_drift`)

### Migration Lease

Only one migration runs at a time per database. When entering the slow path, `converge_with_options` acquires a cooperative lease:

1. **Acquire:** Inside a `BEGIN IMMEDIATE` transaction, check if an active lease exists. If not, write the lease owner ID (`{pid}_{epoch_secs}`) and expiry to `_schema_meta`.
2. **Busy check:** If another process holds the lease, return `MigrateError::MigrationBusy { owner, remaining_secs }`.
3. **TTL:** Leases expire after 300 seconds (5 minutes) for crash recovery.
4. **Phase transitions:** Between execution phases, the lease is verified and refreshed atomically. If the lease was lost, the migration aborts.
5. **Release:** On completion (success or failure), the lease owner and expiry are deleted from `_schema_meta`.

Source: [`src/converge.rs:319-373`](src/converge.rs) (`acquire_lease`, `release_lease`)

### Crash Recovery

If a migration is interrupted (process crash, kill signal, power loss):

1. The `migration_in_progress` flag remains set in `_schema_meta`.
2. On the next `converge` call, the flag is detected and a full re-convergence is forced.
3. Internal temp tables (`_converge_new_*`) are filtered from introspection to prevent crash artifacts from corrupting the diff.
4. The `migration_phase` cursor tracks which phase was in progress at the time of the crash.

The crash recovery path returns `ConvergeMode::CrashRecovery` in the report.

### Atomic State Updates

After successful execution, the schema hash, in-progress flag, and schema version are updated atomically in a single `BEGIN IMMEDIATE` / `COMMIT` transaction:

```rust
// Inside one transaction:
set_meta(conn, "schema_hash", &hash);
delete_meta(conn, "migration_in_progress");
delete_meta(conn, "migration_phase");
increment_schema_version(conn);
set_meta(conn, "sqlite_schema_version", &sv.to_string());
```

A crash between these updates is impossible because they're in a single transaction.

Source: [`src/converge.rs:473-506`](src/converge.rs) (`update_state_atomically`)

### Protected Table Namespace

The migration planner never drops tables matching these patterns:

| Pattern | Description |
|---------|-------------|
| `_schema_meta` | turso-converge's internal state table |
| `_converge_new_*` | Temporary tables from in-progress rebuilds |
| `sqlite_*` | System tables |
| `fts_dir_*` | Turso FTS internal tables |
| `__turso_internal*` | Turso internal tables |


Source: [`src/plan.rs:577-585`](src/plan.rs) (`is_protected_table`)

### AUTOINCREMENT Preservation

When rebuilding a table with `AUTOINCREMENT`, the `sqlite_sequence` value is saved before the rebuild and restored after:

```sql
-- Before rebuild:
INSERT OR REPLACE INTO _schema_meta (key, value)
SELECT 'autoincrement_seq_users', seq FROM sqlite_sequence WHERE name = 'users';

-- After rebuild:
INSERT OR REPLACE INTO sqlite_sequence (name, seq)
SELECT 'users', CAST(value AS INTEGER) FROM _schema_meta
WHERE key = 'autoincrement_seq_users';
DELETE FROM _schema_meta WHERE key = 'autoincrement_seq_users';
```

This prevents AUTOINCREMENT counters from being reset by table rebuilds.

### Busy Timeout

`PRAGMA busy_timeout` is set before migration execution to the value specified in `ConvergeOptions.busy_timeout` (default: 5 seconds). This prevents busy errors when another connection holds a write lock.

Source: [`src/execute.rs:15-28`](src/execute.rs) (`set_busy_timeout`)

### Feature Preflight Validation

Before executing any DDL, turso-converge probes the connection for supported features:

| Feature | Probe method |
|---------|-------------|
| FTS | Create and drop a test FTS index on a temp table |
| Vector columns | Create and drop a temp table with a `vector32(1)` column |
| Materialized views | Create and drop a temp materialized view |
| WITHOUT ROWID | Create and drop a temp WITHOUT ROWID table |
| GENERATED columns | Create and drop a temp table with a GENERATED column |
| Triggers | Create a temp table and trigger, clean up |

If the desired schema uses features the connection doesn't support, a `MigrateError::UnsupportedFeature` is returned with a descriptive message.

Probe artifacts (tables named `_cap_probe_*`) are cleaned up and filtered from introspection.

Source: [`src/introspect.rs:316-363`](src/introspect.rs) (`probe_fts`, `probe_vector`, `probe_materialized_views`)

#### Turso Builder Flags

| Schema Feature | Required Builder Flag | Scope |
|---|---|---|
| FTS indexes (`USING fts`) | `.experimental_index_method(true)` | Experimental |
| Materialized views | `.experimental_materialized_views(true)` | Experimental |
| Triggers | `.experimental_triggers(true)` | Experimental |
| WITHOUT ROWID tables | None — engine-level support | Engine |
| GENERATED columns | None — engine-level support | Engine |
| Vector columns | None — requires vector module | Module |

Features marked "Engine" are supported or rejected at the Turso/libSQL parser level, not via builder flags. turso-converge detects support via runtime probes regardless.

### Pre-Destructive Backup

If `ConvergeOptions.backup_before_destructive` is set and destructive changes are detected, the current schema DDL is written to the specified path before any DDL executes.

- If the path is an existing directory (or has no extension), a timestamped file is created: `turso_converge_backup_{epoch_secs}.sql`
- If the path has an extension, parent directories are created and the file is written directly.

Source: [`src/converge.rs:674-709`](src/converge.rs) (`write_schema_backup`)

### Pre-Destructive Hook

If `ConvergeOptions.pre_destructive_hook` is set, the callback is invoked with a `DestructiveChangeSet` before executing destructive changes. Return `Ok(())` to proceed or `Err(message)` to abort.

```rust
use std::sync::Arc;
use turso_converge::ConvergeOptions;

let options = ConvergeOptions {
    pre_destructive_hook: Some(Arc::new(|changes| {
        if !changes.tables_to_drop.is_empty() {
            return Err("Cannot drop tables in production".to_string());
        }
        Ok(())
    })),
    ..Default::default()
};
```

If the hook rejects, `MigrateError::PreDestructiveHookRejected` is returned with the hook's message and the list of blocked operations.

### Read-Only Guard

All convergence operations check `PRAGMA query_only` before proceeding. If the connection is read-only (e.g., a Turso replica), `MigrateError::ReadOnly` is returned immediately. Additionally, if the `_schema_meta` bootstrap fails with a read-only error, it's caught and converted to `ReadOnly`.

---

## CLI Reference

turso-converge ships a CLI binary for development and CI workflows. Source: [`src/bin/turso-converge.rs`](src/bin/turso-converge.rs).

The CLI opens local databases with all experimental flags enabled (`experimental_index_method`, `experimental_materialized_views`, `experimental_triggers`).

<a id="cli-extract"></a>
### `extract`

```bash
turso-converge extract <db-path>
```

Extracts the schema from an existing database and prints it as SQL to stdout. Tables are topologically sorted by foreign key dependencies. Internal tables (`_schema_meta`, `sqlite_*`, etc.) are excluded. Standard indexes are emitted first, then FTS indexes, then views, then triggers.

Exits with code 0 on success, or code 1 if the database has no user tables.

**Use case:** Bootstrap turso-converge on an existing database. Pipe the output to a file to create your initial schema definition:

```bash
turso-converge extract my.db > schema.sql
```

From this point forward, edit `schema.sql` and use `converge` to apply changes.

**Programmatic equivalent:**
```rust
let snapshot = SchemaSnapshot::from_connection(&conn).await?;
std::fs::write("schema.sql", snapshot.to_sql())?;
```

<a id="cli-validate"></a>
### `validate`

```bash
turso-converge validate <schema.sql>
```

Validates schema SQL by executing it against an in-memory database. Exits with code 0 and prints `"schema is valid"` on success, or exits with code 1 and prints the error on failure.

**Use case:** CI pipeline validation to catch schema syntax errors before deployment.

<a id="cli-diff"></a>
### `diff`

```bash
turso-converge diff <db-path> <schema.sql>
```

Prints a human-readable diff between the live database and the desired schema. Uses the `SchemaDiff` `Display` implementation:

```
+ TABLE users
~ TABLE documents: REBUILD
- TABLE legacy_cache
+ INDEX idx_users_email
```

<a id="cli-plan"></a>
### `plan`

```bash
turso-converge plan <db-path> <schema.sql>
```

Generates and prints the migration plan (the actual SQL statements that would be executed) without applying it. Uses dry-run mode with a permissive policy. Prints `"(no changes)"` if the schema is already converged.

<a id="cli-check"></a>
### `check`

```bash
turso-converge check <db-path> <schema.sql>
```

Checks whether the database schema matches the desired schema. If converged, prints `"schema is converged"` and exits with code 0. If not converged, prints the migration plan and exits with code 1.

**Use case:** CI checks or deployment gates to verify schema is up-to-date.

<a id="cli-apply"></a>
### `apply`

```bash
turso-converge apply <db-path> <schema.sql>
```

Applies convergence to the database using a permissive policy. Prints `"schema converged"` on success.

---

## SQL Normalization

turso-converge uses a string-literal-aware SQL normalizer for both hash computation and diff comparison. Source: [`src/diff.rs:97-179`](src/diff.rs).

The normalizer:
- **Lowercases** all characters outside of string literals (both `'single'` and `"double"` quotes).
- **Preserves case** inside string literals. Escaped quotes (`''` inside single-quoted, `""` inside double-quoted) are handled correctly.
- **Strips** SQL comments — both `--` line comments and `/* block */` comments (with nested `/* */` support).
- **Collapses** consecutive whitespace to a single space.
- **Trims** leading and trailing whitespace.

For hash computation (`normalize_for_hash`), trailing semicolons are additionally stripped.

**Implications:**
- `WHERE status = 'Active'` and `WHERE status = 'active'` are correctly treated as **different** (different string literals).
- `CREATE  TABLE   foo` and `CREATE TABLE foo` are correctly treated as **the same** (whitespace collapsed).
- Adding or changing a comment in your schema file doesn't trigger a full convergence (comments stripped before hashing).
- `INTEGER` and `integer` are treated as the same (case-insensitive type comparison outside literals).

---

## Introspection

Database introspection is implemented in [`src/introspect.rs`](src/introspect.rs).

### Database Introspection

`SchemaSnapshot::from_connection(conn)` introspects a live database using:

| Source | Data extracted |
|--------|----------------|
| `sqlite_schema` (type='table') | Table names and DDL |
| `PRAGMA table_xinfo(table)` | Column metadata (type, nullability, defaults, PK, generated/hidden) |
| `pragma_table_xinfo` table-valued function | Batched column introspection for all tables in one query |
| `PRAGMA table_info(table)` | Fallback if `table_xinfo` fails |
| `PRAGMA foreign_key_list(table)` | Foreign key constraints |
| SQL-based FK parser | Fallback FK detection from DDL when `foreign_key_list` returns nothing |
| DDL pattern matching | COLLATE clauses, AUTOINCREMENT, STRICT, WITHOUT ROWID |
| `sqlite_schema` (type='index') | Index names, tables, and DDL |
| `PRAGMA index_info(index)` | Indexed columns |
| `PRAGMA index_list(table)` | UNIQUE flag |
| `sqlite_schema` (type='view') | View names and DDL |
| `sqlite_schema` (type='trigger') | Trigger names, tables, and DDL |

**Internal object filtering:** Objects with names matching these patterns are excluded:
- `sqlite_*`, `sqlite_autoindex_*` — System objects
- `_schema_meta` — turso-converge internal state table
- `_converge_new_*` — Temporary rebuild tables
- `_cap_probe_*` — Capability probe artifacts
- `fts_dir_*`, `__turso_internal*` — Turso internal objects

### Schema SQL Introspection

`SchemaSnapshot::from_schema_sql(sql)` creates an in-memory Turso database with all experimental features enabled, executes the schema SQL, then introspects the resulting database. The snapshot is cached by BLAKE3 hash.

### Snapshot Caching

`from_schema_sql` caches results in a process-wide `OnceLock<Mutex<HashMap>>`. The cache is keyed by the BLAKE3 hash of the normalized schema SQL. When the cache exceeds 16 entries, it is cleared entirely. This prevents unbounded memory growth while providing fast repeated lookups.

### Capability Detection

`Capabilities::detect(conn)` probes the connection:

1. **Database version:** `SELECT sqlite_version()` → parsed into `(major, minor, patch)` tuple.
2. **DROP COLUMN support:** Enabled for version >= 3.35.0.
3. **RENAME COLUMN support:** Enabled for version >= 3.25.0.
4. **FTS support:** Creates a temp table, attempts a `CREATE INDEX ... USING fts`, cleans up.
5. **Vector support:** Attempts `CREATE TABLE ... (v vector32(1))`, cleans up.
6. **Materialized view support:** Creates a temp table, attempts `CREATE MATERIALIZED VIEW`, cleans up.
7. **WITHOUT ROWID support:** Attempts `CREATE TABLE ... WITHOUT ROWID`, cleans up.
8. **GENERATED column support:** Attempts `CREATE TABLE ... GENERATED ALWAYS AS ...`, cleans up.
9. **Trigger support:** Creates a temp table, attempts `CREATE TRIGGER`, cleans up.

### DDL Generation

`SchemaSnapshot::to_sql()` generates deterministic DDL from a snapshot:

1. Tables are topologically sorted by FK dependencies (tables with no FK references first).
2. If a FK cycle is detected, remaining tables are appended in alphabetical order.
3. Standard indexes are emitted after all tables.
4. FTS indexes are emitted after standard indexes.
5. Views are emitted after all indexes.
6. Triggers are emitted last.
7. Internal tables (`_schema_meta`) are excluded.

---

## Connection Abstraction

For codebases that wrap `turso::Connection` in a custom type, turso-converge provides the `ConnectionLike` trait. Source: [`src/connection.rs`](src/connection.rs).

```rust
pub trait ConnectionLike {
    fn as_turso_connection(&self) -> &turso::Connection;
}

// Blanket implementation for turso::Connection itself:
impl ConnectionLike for turso::Connection { ... }
```

**Wrapper functions:**

| Function | Equivalent |
|----------|------------|
| `converge_like(conn, sql)` | `converge(conn.as_turso_connection(), sql)` |
| `converge_like_with_options(conn, sql, opts)` | `converge_with_options(conn.as_turso_connection(), sql, opts)` |
| `schema_version_like(conn)` | `schema_version(conn.as_turso_connection())` |

**Example:**
```rust
use turso_converge::{ConnectionLike, converge_like};

struct MyConnection {
    inner: turso::Connection,
    // ... other fields
}

impl ConnectionLike for MyConnection {
    fn as_turso_connection(&self) -> &turso::Connection {
        &self.inner
    }
}

// Now you can use:
converge_like(&my_conn, SCHEMA).await?;
```

---

## Error Reference

All operations return `Result<_, MigrateError>`. Source: [`src/error.rs`](src/error.rs).

```rust
pub enum MigrateError {
    Turso(turso::Error),
    Io { path: PathBuf, source: io::Error },
    Statement { stmt: String, source: turso::Error, phase: String },
    ForeignKeyViolation { table: String, rowid: i64, parent: String },
    Schema(String),
    ReadOnly,
    MigrationBusy { owner: String, remaining_secs: u64 },
    PreDestructiveHookRejected { message: String, blocked_operations: Vec<String> },
    UnsupportedFeature(String),
    InjectedFailure { failpoint: String },
    PolicyViolation { message: String, blocked_operations: Vec<String> },
}
```

| Variant | When | Display format |
|---------|------|---------------|
| `Turso(err)` | Any underlying Turso database error | `"turso error: {err}"` |
| `Io { path, source }` | File read failure in `converge_from_path` or backup writing | `"I/O error at {path}: {source}"` |
| `Statement { stmt, source, phase }` | A SQL statement failed during execution | `"migration statement failed ({phase}): {stmt}; cause: {source}"` |
| `ForeignKeyViolation { table, rowid, parent }` | `PRAGMA foreign_key_check` found a violation after a table rebuild | `"foreign key violation: table={table}, rowid={rowid}, references={parent}"` |
| `Schema(msg)` | Schema validation error (e.g., NOT NULL without DEFAULT, empty schema SQL, empty data migration ID) | `"schema error: {msg}"` |
| `ReadOnly` | Connection is read-only (e.g., Turso replica) | `"database is read-only: migrations require write access"` |
| `MigrationBusy { owner, remaining_secs }` | Another migration holds the lease | `"migration busy: another migration is in progress (owner={owner}, expires in {remaining_secs}s)"` |
| `PreDestructiveHookRejected { message, blocked_operations }` | The `pre_destructive_hook` callback returned `Err` | `"pre-destructive hook rejected migration: {message}"` |
| `UnsupportedFeature(msg)` | Schema uses FTS, vector, or materialized views but the target connection lacks support | `"unsupported feature: {msg}"` |
| `InjectedFailure { failpoint }` | A test failpoint was triggered (never in production) | `"injected failpoint triggered: {failpoint}"` |
| `PolicyViolation { message, blocked_operations }` | The `ConvergePolicy` blocked a destructive change | `"policy violation: {message}"` |

**Phase labels** in `Statement` errors:

| Phase | Context |
|-------|---------|
| `"setup"` | `PRAGMA busy_timeout` failed |
| `"DDL"` | Phase 1 transactional DDL failed |
| `"FTS"` | Phase 2 non-transactional FTS operation failed |
| `"views"` | Phase 3 view creation failed |
| `"triggers"` | Phase 3 trigger creation failed |
| `"views_triggers"` | Phase 3 passthrough statement failed |
| `"data_migration"` | Post-DDL data migration statement failed |

---

## Internal State

turso-converge stores its state in a `_schema_meta` table that it creates automatically. This table is never dropped by the migration planner.

```sql
CREATE TABLE IF NOT EXISTS _schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)
```

| Key | Description |
|-----|-------------|
| `schema_hash` | BLAKE3 hash of the last converged schema SQL (normalized). Used for fast-path detection. |
| `previous_schema_sql` | Full DDL of the schema before the last migration. Used by `rollback_to_previous`. |
| `sqlite_schema_version` | The `PRAGMA schema_version` value after the last migration. Used for drift detection. |
| `migration_in_progress` | Set to `"1"` during migration, cleared on completion. Used for crash recovery. |
| `migration_phase` | Phase cursor: `"introspect"`, `"ddl"`, or `"complete"`. Used for crash recovery diagnostics. |
| `migration_owner` | Lease owner ID (`{pid}_{epoch_secs}`). Used for concurrency control. |
| `migration_lease_until` | Lease expiry as epoch seconds. TTL is 300 seconds (5 minutes). |
| `data_migration:{id}` | Epoch seconds when this data migration was applied. Prevents re-application. |
| `autoincrement_seq_{table}` | Temporary storage for AUTOINCREMENT sequence values during table rebuilds. Cleaned up after use. |


turso-converge also maintains a `schema_version` table for user-facing version tracking:

```sql
CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL)
```

This table contains a single row with the current version counter. It is incremented atomically each time DDL is applied.

---

## Architecture

The crate is organized into 10 modules plus a CLI binary.

```
src/
├── lib.rs          — Public re-exports
├── converge.rs     — Core convergence orchestration (fast path, slow path, lease, state)
├── diff.rs         — Schema diff computation, SQL normalization
├── plan.rs         — Migration plan generation, FK ordering, rebuild safety validation
├── execute.rs      — Three-phase DDL execution, FK checking, view fixed-point
├── introspect.rs   — Database introspection, schema SQL parsing, capability probing
├── schema.rs       — Type definitions (SchemaSnapshot, TableInfo, ColumnInfo, etc.)
├── options.rs      — Configuration types (ConvergeOptions, ConvergePolicy, ConvergeReport)
├── error.rs        — MigrateError enum
├── connection.rs   — ConnectionLike trait and wrapper functions
└── bin/
    └── turso-converge.rs  — CLI binary (validate/diff/plan/check/apply)
```

**Data flow:**

```
Schema SQL → [normalize_for_hash] → BLAKE3 hash → compare with _schema_meta
                                                        ↓ (mismatch)
Schema SQL → [from_schema_sql] → SchemaSnapshot (desired)
                                                        ↓
Connection → [from_connection] → SchemaSnapshot (actual)
                                                        ↓
         [compute_diff] → SchemaDiff
                                                        ↓
         [check_policy] → allow or reject
                                                        ↓
         [generate_plan] → MigrationPlan
                                                        ↓
         [execute_plan_with_timeout] → DDL execution
                                                        ↓
         [update_state_atomically] → hash + version stored
```

**Dependencies:**

| Crate | Version | Purpose |
|-------|---------|---------|
| `turso` | 0.6.0-pre.3 | Turso database client |
| `turso_core` | 0.6.0-pre.3 | Core database features (FTS, filesystem, raw connection API) |
| `blake3` | 1 | Fast cryptographic hashing for change detection |
| `tracing` | 0.1 | Structured logging at key decision points |
| `thiserror` | 2 | Derive macro for `MigrateError` |
| `tokio` | 1 | Async runtime (file I/O, test infrastructure) |

---

## Supported Features

| Feature | Support | Notes |
|---------|---------|-------|
| Tables (CREATE, ALTER, DROP) | Full | FK-ordered creation, policy-controlled drops |
| Standard indexes | Full | Created/dropped/recreated as needed |
| Partial indexes (`WHERE` clause) | Full | WHERE clause changes detected via normalized SQL comparison |
| FTS indexes (tantivy, `USING fts`) | Full | Non-transactional phase, `WITH` clause support |
| Vector columns (`vector32`) | Full | Diffed and preserved during rebuilds |
| Materialized views (IVM) | Full | Treated as views with `is_materialized` flag |
| Regular views | Full | Dependency-ordered creation, fixed-point resolution |
| Triggers | Full | Dropped/recreated on rebuilt tables |
| Foreign keys (PRAGMA-based detection) | Full | With SQL-based fallback parser |
| CHECK constraints | Full | Parsed from DDL; column-level, table-level, and function-based CHECKs; changes trigger table rebuild |
| COLLATE clauses | Full | Detected via DDL pattern matching |
| GENERATED columns | Full | Excluded from data copy during rebuilds |
| STRICT tables | Full | Detected from DDL |
| WITHOUT ROWID tables | Full | Detected from DDL |
| AUTOINCREMENT | Full | Sequence preserved across rebuilds |
| UNIQUE constraints | Full | Detected via `PRAGMA index_list` |
| ADD COLUMN (nullable or with DEFAULT) | Full | O(1), no rebuild required |
| DROP COLUMN (eligible columns) | Full | O(1) when not PK/indexed/FK-referenced/view-referenced |
| Feature preflight validation | Full | Runtime capability probes for FTS, vector, materialized views |
| Migration lease (concurrency) | Full | Cooperative lease in `_schema_meta` with 5-minute TTL |
| Destructive change protection | Full | `ConvergePolicy` with table/column drop blocking |
| Dry-run mode | Full | Plan without executing, SQL returned in report |
| Runtime schema validation API | Full | `validate_schema` against in-memory DB |
| Crash recovery | Full | `migration_in_progress` flag + phase cursor |
| Schema drift detection | Full | `PRAGMA schema_version` monitoring |
| Column rename detection | Full | Conservative heuristic + `ColumnRenameHint` |
| Rollback to previous schema | Full | `rollback_to_previous` (single-step) |
| Multi-file schema composition | Full | `converge_multi` / `converge_multi_with_options` |
| Idempotent data migrations | Full | `DataMigration` tracked by ID in `_schema_meta` |
| Read-only/replica guard | Full | `ReadOnly` error + `is_read_only` check |
| Pre-destructive backup snapshot | Full | `backup_before_destructive` option |
| Pre-destructive callback gate | Full | `pre_destructive_hook` option |
| Crash failpoint scaffolding | Full | `Failpoint` enum (test-only) |
| Connection abstraction wrappers | Full | `ConnectionLike` trait + `*_like` helpers |
| CLI workflows | Full | `validate` / `diff` / `plan` / `check` / `apply` |
| Case-insensitive identifiers | Full | `CIString` keys matching Turso semantics |
| String-literal-aware SQL normalization | Full | Preserves case in literals |
| Human-readable diff output | Full | `Display` impl for `SchemaDiff` |
| Structured tracing | Full | `tracing` crate at key decision points |

---

## Known Limitations

### Fundamental

**Rename detection is conservative.** Automatic rename detection only applies when there is an unambiguous 1:1 match between an added and dropped column with the same type, constraints, and ordinal position. Use `ColumnRenameHint` for non-positional or ambiguous rename scenarios.

**Table rebuilds copy all rows.** Large tables take time proportional to their row count. There is no way to avoid this for changes that require rebuilds (type changes, constraint changes, etc.).

**Rollback scope is single-step.** `rollback_to_previous` restores the most recent prior schema snapshot only, not arbitrary historical versions. For multi-step rollback, maintain your own schema version history.

### Implementation-Specific

**COLLATE detection is SQL-based.** COLLATE clauses are extracted from `CREATE TABLE` SQL using pattern matching (finding the column section, then looking for the `COLLATE` keyword). Unusual formatting or column names that are substrings of other column names could theoretically be mismatched, though the implementation handles quoted identifiers.

**FTS + triggers + materialized views require experimental Turso flags.** You must set `.experimental_index_method(true)`, `.experimental_materialized_views(true)`, and `.experimental_triggers(true)` on your `turso::Builder`. Without these flags, `MigrateError::UnsupportedFeature` is returned with a message specifying which flag is needed.

**WITHOUT ROWID tables are not supported by Turso.** turso-converge detects this via a runtime probe and returns `MigrateError::UnsupportedFeature` with a clear message before attempting to parse the schema. When Turso adds support, the probe will detect it automatically.

**GENERATED columns are not supported by Turso.** turso-converge detects this via a runtime probe and returns `MigrateError::UnsupportedFeature` with a clear message before attempting to parse the schema. When Turso adds support, the probe will detect it automatically.

**Snapshot cache is process-wide.** The `from_schema_sql` cache uses a `OnceLock<Mutex<HashMap>>` and is cleared when it exceeds 16 entries. In long-running processes with many distinct schemas, cache churn may occur.

**FK parser fallback is approximate.** The SQL-based foreign key parser (used when `PRAGMA foreign_key_list` returns nothing) parses `REFERENCES table_name` tokens. It correctly skips string literals and comments, but extracts only the referenced table name — not the referenced columns or ON DELETE/UPDATE actions.

---

## Testing

```bash
cargo test
```

186 tests covering: convergence, diff (including rename hints), plan generation, execution (3 phases + rename path + view retry), introspection (table_xinfo + TVF batching fallback), schema round-trip, policy enforcement (all four policy fields), dry-run, drift detection, rollback, backup hook, idempotent data migrations, read-only guards, failpoint crash scaffolding, deterministic fuzzing, SQL normalization, triggers, connection abstraction wrappers, unsupported feature detection, NoOp mode, migration lease contention, protected table namespace, data integrity verification, index functionality verification, CHECK constraint tracking and diffing, and partial index WHERE clause handling.

All tests use in-memory Turso databases — no external services, no network, no test fixtures to set up.

**Test files:**

| File | Tests | Covers |
|------|-------|--------|
| `tests/converge.rs` | 25 | Core convergence, fast path, drift, crash recovery |
| `tests/coverage.rs` | 43 | Policy edge cases, data migration errors, lease contention, AUTOINCREMENT, COLLATE, DROP COLUMN, STRICT tables, CIString, UnsupportedFeature, NoOp mode, data integrity, index verification, capabilities validation (WITHOUT ROWID, GENERATED, triggers) |
| `tests/execute.rs` | 21 | Three-phase execution, rebuild, FK checks, views |
| `tests/diff.rs` | 23 | Diff computation, rename detection, SQL normalization |
| `tests/new_api.rs` | 16 | `converge_with_options`, policy, dry-run, backup, hooks |
| `tests/introspect.rs` | 12 | `table_xinfo`, batched introspection, snapshot caching |
| `tests/check_constraints.rs` | 11 | CHECK constraint tracking: same/modified/added/removed, column-level, table-level, function-based, end-to-end convergence, whitespace normalization |
| `tests/partial_indexes.rs` | 9 | Partial index WHERE clause: same/changed/added/removed WHERE, end-to-end convergence, plan verification, idempotency, whitespace normalization |
| `tests/triggers.rs` | 6 | Trigger creation, rebuild, drop |
| `tests/migrator.rs` | 4 | End-to-end migration scenarios |
| `tests/fuzz.rs` | 1 | Deterministic fuzzing of schema round-trips |
| `src/bin/turso-converge.rs` | 1 | CLI trigger DDL support |
| `src/introspect.rs` | 9 | FK parser edge cases, CHECK constraint extraction (7 parser unit tests) |
| `src/execute.rs` | 5 | Statement classification (view/trigger detection) |

**CI:** GitHub Actions runs `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` on every push and pull request using Rust 1.88. See [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

---

## License

MIT — see [LICENSE](LICENSE).
