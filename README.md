# turso-migrate

Declarative schema convergence for [Turso](https://turso.tech/) databases.

Instead of numbered migration files (`001_up.sql`, `002_up.sql`, ...), you define your desired schema in a single SQL file and turso-migrate automatically diffs and converges any database to match it. A BLAKE3 hash fast-path makes subsequent checks near-instant when the schema hasn't changed.

## Quick Start

**Requirements:** Rust 1.85+ (edition 2024), Tokio multi-thread runtime, turso crate

```toml
# Cargo.toml
[dependencies]
turso-migrate = { git = "https://github.com/opentyler/turso-migrate" }
turso = "0.5.0-pre.13"
turso_core = { version = "0.5.0-pre.13", features = ["fts", "fs"] }
tokio = { version = "1", features = ["full"] }
```

```rust
use turso_migrate::converge;

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

## API Reference

### `converge(conn, schema_sql)` — Backward-Compatible Entry Point

The simplest API. Uses a permissive policy (allows all changes including table drops) for backward compatibility.

```rust
const SCHEMA: &str = include_str!("../my_schema.sql");
turso_migrate::converge(&conn, SCHEMA).await?;
```

### `converge_with_options(conn, schema_sql, options)` — Full-Featured API

Returns a detailed `ConvergeReport` and supports policy enforcement, dry-run mode, and configurable behavior.

```rust
use turso_migrate::{converge_with_options, ConvergeOptions, ConvergePolicy, ConvergeMode};

// Safe defaults: blocks destructive changes (table drops, column drops)
let options = ConvergeOptions::default();

// Or permissive: allows all changes
let options = ConvergeOptions {
    policy: ConvergePolicy::permissive(),
    ..Default::default()
};

// Or dry-run: preview changes without executing
let options = ConvergeOptions {
    policy: ConvergePolicy::permissive(),
    dry_run: true,
    ..Default::default()
};

let report = converge_with_options(&conn, SCHEMA, &options).await?;

match report.mode {
    ConvergeMode::FastPath => println!("Schema unchanged (<1ms)"),
    ConvergeMode::SlowPath => println!("Applied {} changes", report.tables_created + report.tables_rebuilt),
    ConvergeMode::DryRun => println!("Would apply: {:?}", report.plan_sql),
    ConvergeMode::CrashRecovery => println!("Recovered from interrupted migration"),
    ConvergeMode::NoOp => println!("Slow path ran but no changes needed"),
}
```

### `ConvergePolicy` — Destructive Change Protection

Controls what the migration is allowed to do. Default policy blocks destructive changes.

```rust
use turso_migrate::ConvergePolicy;

// Safe defaults (recommended for production):
let policy = ConvergePolicy::default();
// allow_table_drops: false
// allow_column_drops: false
// allow_table_rebuilds: true
// max_tables_affected: None

// Permissive (for development or when you know what you're doing):
let policy = ConvergePolicy::permissive();

// Custom:
let policy = ConvergePolicy {
    allow_table_drops: true,
    allow_column_drops: false,
    allow_table_rebuilds: true,
    max_tables_affected: Some(5),  // Safety limit
};
```

When a policy violation is detected, `converge_with_options` returns `Err(MigrateError::PolicyViolation { .. })` with details about what was blocked and why.

### `ConvergeOptions` — Full Configuration

```rust
use std::time::Duration;
use turso_migrate::ConvergeOptions;

let options = ConvergeOptions {
    policy: ConvergePolicy::default(),      // Safe: blocks destructive changes
    dry_run: false,                         // Set true to preview without executing
    busy_timeout: Duration::from_secs(5),   // PRAGMA busy_timeout for SQLITE_BUSY
    max_retries: 3,
    backup_before_destructive: None,        // Optional file/dir path for pre-DDL SQL backup
    data_migrations: vec![],                // Optional idempotent data migration steps
    rename_hints: vec![],                   // Optional explicit column rename hints
    pre_destructive_hook: None,             // Optional callback gate before destructive changes
    failpoint: None,                        // Test-only crash injection selector
};
```

The `busy_timeout` is applied as `PRAGMA busy_timeout` before executing the migration plan. This prevents `SQLITE_BUSY` errors when another connection holds a write lock.

### `ConvergeReport` — What Happened

Returned by `converge_with_options`. Includes the execution mode, statistics about what changed, and timing.

```rust
pub struct ConvergeReport {
    pub mode: ConvergeMode,        // FastPath | SlowPath | DryRun | CrashRecovery | NoOp
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
    pub plan_sql: Vec<String>,     // Populated in dry-run mode
}
```

### `converge_from_path(conn, path)`

Reads a schema file from disk, then converges using the permissive policy.

```rust
turso_migrate::converge_from_path(&conn, "schemas/turso_schema.sql").await?;
```

### `converge_multi(conn, &[...])` and `converge_multi_with_options(...)`

Compose schema from multiple SQL fragments/files (concatenated in order), then converge as one schema:

```rust
let parts = [
    include_str!("schemas/core.sql"),
    include_str!("schemas/indexes.sql"),
    include_str!("schemas/views.sql"),
];

turso_migrate::converge_multi(&conn, &parts).await?;
```

### `rollback_to_previous(conn)`

Re-converges to the previous stored schema snapshot (`previous_schema_sql` in `_schema_meta`):

```rust
turso_migrate::rollback_to_previous(&conn).await?;
```

### `validate_schema(schema_sql)`

Validates schema SQL by executing it against an in-memory Turso database:

```rust
turso_migrate::validate_schema(include_str!("../turso_schema.sql")).await?;
```

### `ConnectionLike` wrappers

For codebases that wrap `turso::Connection`, use:

```rust
use turso_migrate::{ConnectionLike, converge_like, schema_version_like};
```

### `DataMigration` — Idempotent Post-DDL Data Steps

Data migrations run after schema convergence and are tracked in `_schema_meta` so each migration ID applies once:

```rust
use turso_migrate::{ConvergeOptions, ConvergePolicy, DataMigration, converge_with_options};

let options = ConvergeOptions {
    policy: ConvergePolicy::permissive(),
    data_migrations: vec![DataMigration {
        id: "seed-users".to_string(),
        statements: vec![
            "INSERT INTO users (id, name) VALUES ('u1', 'alice')".to_string(),
        ],
    }],
    ..Default::default()
};

let report = converge_with_options(&conn, SCHEMA, &options).await?;
println!("data migrations applied: {}", report.data_migrations_applied);
```

### `SchemaDiff` — Human-Readable Diff

The diff engine produces a `SchemaDiff` with a `Display` implementation for human-readable output:

```rust
use turso_migrate::{SchemaSnapshot, compute_diff};

let desired = SchemaSnapshot::from_schema_sql(schema_sql).await?;
let actual = SchemaSnapshot::from_connection(&conn).await?;
let diff = compute_diff(&desired, &actual);

println!("{diff}");
// + TABLE users
// + TABLE posts
// ~ TABLE documents: REBUILD
// - TABLE legacy_cache
// + INDEX idx_posts_user
// + FTS INDEX idx_docs_fts
```

### `SchemaSnapshot::to_sql()`

Introspect a live database and generate deterministic DDL. Tables are topologically sorted by foreign key dependencies.

```rust
let snapshot = SchemaSnapshot::from_connection(&conn).await?;
std::fs::write("turso_schema.sql", snapshot.to_sql())?;
```

### `schema_version(conn)`

Returns the schema version counter (incremented atomically each time DDL is applied).

```rust
let version = turso_migrate::schema_version(&conn).await?;
```

### `MigrateError` — Error Types

All operations return `Result<_, MigrateError>`:

| Variant | When | Fields |
|---------|------|--------|
| `Turso(turso::Error)` | Underlying database error | Source error |
| `Io { path, source }` | File read failure (converge_from_path) | Path + IO error |
| `Statement { stmt, source, phase }` | SQL execution failed | The SQL, error, and phase (`DDL`, `FTS`, or `views_triggers`) |
| `ForeignKeyViolation { table, rowid, parent }` | FK check failed after rebuild | Table, row, and referenced parent |
| `Schema(String)` | Schema validation error (e.g., NOT NULL without DEFAULT) | Descriptive message |
| `PolicyViolation { message, blocked_operations }` | Policy blocked a destructive change | What was blocked and why |
| `MigrationBusy { owner, remaining_secs }` | Another migration holds the lease | Owner ID and seconds until expiry |
| `UnsupportedFeature(String)` | Schema uses FTS/vector/materialized views but target lacks support | Descriptive message |
| `ReadOnly` | Connection is read-only / replica role | Migration requires write access |
| `PreDestructiveHookRejected { .. }` | User hook rejected destructive operations | Hook message + blocked ops |
| `InjectedFailure { failpoint }` | Test failpoint intentionally aborted convergence | Failpoint identifier |

## How It Works

```
Developer edits turso_schema.sql (desired end-state)
    ↓
Consumer embeds: include_str!("turso_schema.sql")
    ↓
On each database connection:
    1. Normalize schema SQL → BLAKE3 hash → compare against stored hash
    2. Check PRAGMA schema_version for out-of-band drift
    3. Both match → return (<1ms, fast-path)
    4. Mismatch → full convergence:
       a. Build pristine snapshot (in-memory Turso DB from schema SQL)
       b. Introspect actual database (sqlite_schema + PRAGMA table_xinfo)
       c. Compute diff (12 categories)
       d. Check policy (block destructive changes if configured)
       e. Validate rebuild safety (NOT NULL columns require DEFAULT)
       f. Generate migration plan (FK-ordered, capability-aware)
       g. Execute plan (deferred FK checks, FK validation after rebuild)
       h. Atomically update hash + version + clear in-progress flag
```

## Safety Features

### Destructive Change Protection
`converge_with_options` with default `ConvergePolicy` blocks table drops and column drops. An accidental schema edit won't destroy data — you get a clear `PolicyViolation` error instead.

### NOT NULL Column Validation
Adding a `NOT NULL` column without a `DEFAULT` value to an existing table is caught **before** any DDL executes. The error message tells you exactly which column on which table needs a default.

### Foreign Key Integrity
- `PRAGMA defer_foreign_keys = ON` before table rebuilds prevents failures with self-referential and cyclic foreign keys
- `PRAGMA foreign_key_check` runs after rebuilds to catch FK violations before COMMIT
- FK dependency ordering ensures tables are created in the right order

### Schema Drift Detection
Even when the desired schema hash matches, turso-migrate checks `PRAGMA schema_version` to detect out-of-band changes (manual SQL, admin operations, replica divergence). If drift is detected, it forces a full convergence to correct the database.

### Migration Lease (Concurrency Protection)
Only one migration runs at a time per database. `converge_with_options` acquires a cooperative lease in `_schema_meta` (owner + TTL) before entering the slow path. If another process holds the lease, the caller gets `MigrateError::MigrationBusy` with the owner and time remaining. The lease expires automatically after 5 minutes (crash recovery). Before each execution phase transition, turso-migrate performs an atomic guarded lease refresh (owner must still match and lease must still be unexpired), and aborts immediately if the lease is lost.

### Crash Recovery
If a migration is interrupted, the `migration_in_progress` flag and phase cursor force a full re-convergence on the next connection. Internal temp tables (`_converge_new_*`) are filtered from introspection to prevent crash artifacts from corrupting the diff.

### Atomic State Updates
Hash, schema version, and the in-progress flag are updated in a single `BEGIN IMMEDIATE` / `COMMIT` transaction. A crash between these updates is impossible.

### Protected Table Namespace
Internal tables (`_schema_meta`, `_converge_new_*`, `_cap_probe_*`, `sqlite_*`, etc.) are never dropped by the migration planner, even if they appear in the diff.

### AUTOINCREMENT Preservation
Table rebuilds save and restore `sqlite_sequence` values so that AUTOINCREMENT counters aren't reset.

### Busy Timeout
`PRAGMA busy_timeout` is set before migration execution (configurable via `ConvergeOptions.busy_timeout`, default 5 seconds). This prevents `SQLITE_BUSY` errors when another connection holds a write lock.

## Supported Features

| Feature | Support |
|---------|---------|
| Tables (CREATE, ALTER, DROP) | ✅ |
| Standard indexes | ✅ |
| FTS indexes (tantivy, `USING fts`) | ✅ |
| Vector columns (`vector32`) | ✅ |
| Materialized views (IVM) | ✅ |
| Regular views (with dependency ordering) | ✅ |
| Triggers | ✅ |
| Foreign keys (PRAGMA-based detection) | ✅ |
| COLLATE clauses | ✅ (detected via SQL parsing) |
| GENERATED columns | ✅ (excluded from data copy) |
| STRICT tables | ✅ (detected) |
| WITHOUT ROWID tables | ✅ (detected) |
| AUTOINCREMENT | ✅ (sequence preserved across rebuilds) |
| UNIQUE constraints | ✅ (via PRAGMA index_list) |
| ADD COLUMN (nullable or with DEFAULT) | ✅ (O(1), no rebuild) |
| DROP COLUMN (eligible columns) | ✅ (O(1) when not PK/indexed/FK-referenced/view-referenced) |
| Feature preflight validation | ✅ (runtime capability probes for FTS, vector, materialized view) |
| Migration lease (concurrency) | ✅ (cooperative lease in _schema_meta) |
| Destructive change protection | ✅ (ConvergePolicy) |
| Dry-run mode | ✅ (plan without executing) |
| Runtime schema validation API | ✅ (`validate_schema`) |
| Crash recovery | ✅ (`migration_in_progress` flag) |
| Schema drift detection | ✅ (PRAGMA schema_version) |
| Column rename detection | ✅ (conservative heuristic + `ColumnRenameHint`) |
| Rollback to previous schema | ✅ (`rollback_to_previous`) |
| Multi-file schema composition | ✅ (`converge_multi*`) |
| Idempotent data migrations | ✅ (`DataMigration` + `ConvergeOptions.data_migrations`) |
| Read-only/replica guard | ✅ (`ReadOnly` + `is_read_only`) |
| Pre-destructive backup snapshot | ✅ (`backup_before_destructive`) |
| Pre-destructive callback gate | ✅ (`pre_destructive_hook`) |
| Crash failpoint scaffolding | ✅ (`Failpoint` in `ConvergeOptions`) |
| Connection abstraction wrappers | ✅ (`ConnectionLike` + `*_like` helpers) |
| CLI workflows | ✅ (`turso-migrate diff/plan/check/apply/validate`) |
| Case-insensitive identifiers | ✅ (CIString keys) |
| String-literal-aware SQL normalization | ✅ (preserves case in literals) |
| Human-readable diff output | ✅ (Display for SchemaDiff) |
| Structured tracing | ✅ (tracing crate at key decision points) |

## Schema Type System

turso-migrate uses a rich internal schema representation:

- **`SchemaSnapshot`** — Complete database schema with case-insensitive `CIString` keys
- **`TableInfo`** — Columns, foreign keys, STRICT/WITHOUT ROWID/AUTOINCREMENT flags
- **`ColumnInfo`** — Type, nullability, default, primary key, collation, generated/hidden status
- **`IndexInfo`** — Table, SQL, FTS flag, UNIQUE flag, indexed columns
- **`ForeignKey`** — From/to columns, referenced table, ON DELETE/UPDATE actions
- **`Capabilities`** — Detected SQLite version and available features

## The 12-Step ALTER TABLE Procedure

SQLite has limited native ALTER TABLE support. For changes beyond ADD/DROP COLUMN, SQLite prescribes a [12-step procedure](https://www.sqlite.org/lang_altertable.html#otheralter). turso-migrate follows it with safety enhancements:

| SQLite Step | turso-migrate | Safety Enhancement |
|-------------|---------------|-------------------|
| 1. Disable FK constraints | `PRAGMA defer_foreign_keys = ON` | Defers rather than disables — violations still caught |
| 2. Begin transaction | `BEGIN IMMEDIATE` | Write lock prevents concurrent migration |
| 3. Remember schema objects | `SchemaSnapshot::from_connection()` | Full introspection via `PRAGMA table_xinfo` |
| 4. Create new table | `CREATE TABLE "_converge_new_{table}"` | Correct create-copy-drop-rename order |
| 5. Copy data | `INSERT INTO ... SELECT ...` | Generated columns excluded, defaults for new columns |
| 6. Drop old table | `DROP TABLE` | Protected tables never dropped |
| 7. Rename new to old | `ALTER TABLE ... RENAME TO` | |
| 8-9. Recreate objects | Auto-detected by diff engine | Views use fixed-point ordering for dependencies |
| 10. FK check | `PRAGMA foreign_key_check` | Returns structured `ForeignKeyViolation` error |
| 11. Commit | `COMMIT` | Atomic with hash + version update |
| 12. Restore FK mode | Automatic | `defer_foreign_keys` is per-transaction |

### Execution Phases

```
Phase 1 (transactional):
  PRAGMA defer_foreign_keys = ON → BEGIN IMMEDIATE →
  DROP triggers → DROP views → DROP indexes → DROP tables →
  CREATE tables (FK-ordered) → ADD COLUMN → Table rebuilds →
  CREATE indexes → PRAGMA foreign_key_check → COMMIT

Phase 2 (non-transactional):
  DROP FTS indexes → CREATE FTS indexes

Phase 3 (transactional):
  CREATE views (fixed-point retry for unresolved deps) → CREATE triggers
```

## SQL Normalization

turso-migrate uses a string-literal-aware SQL normalizer that:
- **Lowercases** keywords and identifiers outside of string literals
- **Preserves case** inside `'single'` and `"double"` quoted strings
- **Strips** SQL comments (both `--` line and `/* block */` styles)
- **Collapses** whitespace to single spaces

This means:
- `WHERE status = 'Active'` and `WHERE status = 'active'` are correctly treated as **different**
- `CREATE  TABLE   foo` and `CREATE TABLE foo` are correctly treated as **the same**
- Adding a comment to your schema file doesn't trigger a full convergence
- `INTEGER` and `integer` are correctly treated as **the same** (case-insensitive type comparison)

## Known Limitations

### Fundamental

**Rename detection is conservative.** Automatic rename detection only applies when there is an unambiguous 1:1 match (same type/constraints/position). Use `ColumnRenameHint` for non-positional or ambiguous rename scenarios.

**Table rebuilds copy all rows.** Large tables take proportional time.

**Rollback scope is single-step.** `rollback_to_previous` restores the most recent prior schema snapshot, not arbitrary historical versions.

### Implementation-Specific

**COLLATE detection is SQL-based.** Extracts COLLATE clauses from CREATE TABLE SQL using pattern matching, not a full SQL parser. Unusual formatting could be missed.

**FTS + triggers require experimental Turso flags.** Set `.experimental_index_method(true)`, `.experimental_materialized_views(true)`, and `.experimental_triggers(true)` on your `turso::Builder`.

## Running Tests

```bash
cargo test
```

117 tests covering: convergence, diff (including rename hints), plan generation, execution (3 phases + rename path + view retry), introspection (table_xinfo + TVF batching fallback), schema round-trip, policy enforcement, dry-run, drift detection, rollback, backup hook, idempotent data migrations, read-only guards, failpoint crash scaffolding, deterministic fuzzing, SQL normalization, connection abstraction wrappers, and the legacy bridge. In-memory Turso databases, no external services.

## CLI

The crate now ships a small CLI for development/CI workflows:

```bash
turso-migrate validate schemas/turso_schema.sql
turso-migrate diff data/user.db schemas/turso_schema.sql
turso-migrate plan data/user.db schemas/turso_schema.sql
turso-migrate check data/user.db schemas/turso_schema.sql
turso-migrate apply data/user.db schemas/turso_schema.sql
```

## Background

turso-migrate is a Rust implementation of the approach described in David Rothlis and William Manley's [Simple declarative schema migration for SQLite](https://david.rothlis.net/declarative-schema-migration-for-sqlite/) (2022). turso-migrate extends the original with:

| Area | Original (Python) | turso-migrate |
|------|-------------------|---------------|
| Triggers & views | Not supported | Full support with dependency ordering |
| FTS indexes | N/A | Turso tantivy FTS with 3-phase execution |
| Vector columns | N/A | `vector32(N)` diffed and preserved |
| Change detection | Always full introspection | BLAKE3 hash + drift detection (<1ms) |
| Crash recovery | None | `migration_in_progress` flag + phase cursor + internal temp table filtering |
| FK handling | Basic | PRAGMA-based detection, deferred checks, post-rebuild validation |
| Introspection | `PRAGMA table_info` | `PRAGMA table_xinfo` + `index_list` + `foreign_key_list` |
| Schema model | Basic columns | COLLATE, GENERATED, UNIQUE, STRICT, WITHOUT ROWID, AUTOINCREMENT |
| Safety | None | Policy enforcement, NOT NULL validation, protected namespaces |
| API | Single function | `converge` + `converge_with_options` + `SchemaDiff` Display |
| Observability | None | Structured tracing + ConvergeReport |

## License

MIT
