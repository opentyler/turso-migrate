# turso-converge

Declarative schema convergence for [Turso](https://turso.tech/) databases.

Define your desired schema once. turso-converge diffs it against the live database and applies the minimal set of DDL operations to converge. A BLAKE3 hash fast-path makes subsequent checks sub-millisecond.

```
Traditional migrations:           turso-converge:

001_create_users.sql             turso_schema.sql  ← single source of truth
002_add_email.sql                       ↓
003_create_posts.sql             converge(&conn, SCHEMA)
004_add_index.sql                       ↓
005_rename_col.sql               automatic diff → plan → execute
```

## Install

```toml
[dependencies]
turso-converge = { git = "https://github.com/opentyler/turso-converge" }
turso = "0.5.0-pre.13"
turso_core = { version = "0.5.0-pre.13", features = ["fts", "fs"] }
tokio = { version = "1", features = ["full"] }
```

Requires **Rust 1.85+** (edition 2024) and a **Tokio multi-thread** runtime.

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

    converge(&conn, SCHEMA).await?;
    // First run: creates tables + indexes.
    // Every run after: <1ms hash check, returns immediately.

    Ok(())
}
```

## How It Works

```
converge() called
    │
    ├─ Normalize SQL → BLAKE3 hash → compare with stored hash
    ├─ Check PRAGMA schema_version for out-of-band drift
    │
    ├─ Both match? → return (<1ms)                          ← fast path
    │
    └─ Mismatch → full convergence:                         ← slow path
         1. Acquire migration lease
         2. Build desired snapshot (in-memory DB from SQL)
         3. Introspect actual database
         4. Compute diff (14 categories)
         5. Check policy → validate safety → generate plan
         6. Execute in 3 phases (DDL → FTS → views/triggers)
         7. Atomically update hash + version
         8. Release lease
```

## API

### Simple — `converge`

Uses a permissive policy (allows all changes). Good for development.

```rust
turso_converge::converge(&conn, include_str!("../schema.sql")).await?;
```

### Production — `converge_with_options`

Returns a `ConvergeReport`. Supports policy enforcement, dry-run, backup hooks, data migrations, and more.

```rust
use turso_converge::{converge_with_options, ConvergeOptions, ConvergePolicy, ConvergeMode};

let options = ConvergeOptions::default(); // safe: blocks table/column drops

let report = converge_with_options(&conn, SCHEMA, &options).await?;
match report.mode {
    ConvergeMode::FastPath      => println!("unchanged (<1ms)"),
    ConvergeMode::SlowPath      => println!("applied changes"),
    ConvergeMode::DryRun        => println!("plan: {:?}", report.plan_sql),
    ConvergeMode::CrashRecovery => println!("recovered from crash"),
    ConvergeMode::NoOp          => println!("no changes needed"),
}
```

### Dry Run — Preview Without Executing

```rust
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

### Additional Entry Points

| Function | Description |
|----------|-------------|
| `converge_from_path(&conn, "schema.sql")` | Read schema from file, converge with permissive policy |
| `converge_multi(&conn, &[part1, part2])` | Compose schema from multiple SQL fragments |
| `converge_multi_with_options(...)` | Multi-fragment with full options |
| `validate_schema(sql)` | Validate schema against in-memory DB (no side effects) |
| `schema_version(&conn)` | Read the schema version counter |
| `rollback_to_previous(&conn)` | Re-converge to the previous schema snapshot |
| `is_read_only(&conn)` | Check if connection is read-only |
| `compute_diff(&desired, &actual)` | Compute diff between two `SchemaSnapshot`s |


## Policy — Destructive Change Protection

Default policy blocks accidental data loss. Customize per environment:

```rust
use turso_converge::ConvergePolicy;

// Production — safe defaults:
let policy = ConvergePolicy::default();
// allow_table_drops: false    ← blocks DROP TABLE
// allow_column_drops: false   ← blocks DROP COLUMN
// allow_table_rebuilds: true
// max_tables_affected: None

// Development — allow everything:
let policy = ConvergePolicy::permissive();

// Custom — fine-grained control:
let policy = ConvergePolicy {
    allow_table_drops: true,
    allow_column_drops: false,
    allow_table_rebuilds: true,
    max_tables_affected: Some(5),
};
```

Violations return `MigrateError::PolicyViolation` with details about what was blocked and why.

## Data Migrations

Idempotent post-DDL data steps, tracked by ID so each applies exactly once:

```rust
use turso_converge::{ConvergeOptions, DataMigration};

let options = ConvergeOptions {
    data_migrations: vec![DataMigration {
        id: "seed-admin".to_string(),
        statements: vec![
            "INSERT OR IGNORE INTO users (id, email) VALUES ('admin', 'admin@example.com')".into(),
        ],
    }],
    ..Default::default()
};
```

## Column Rename Detection

turso-converge detects renames automatically when there's an unambiguous 1:1 match (same type, constraints, ordinal position). For ambiguous cases, provide explicit hints:

```rust
use turso_converge::{ConvergeOptions, ColumnRenameHint};

let options = ConvergeOptions {
    rename_hints: vec![ColumnRenameHint {
        table: "users".into(),
        from: "name".into(),
        to: "display_name".into(),
    }],
    ..Default::default()
};
```

## Pre-Destructive Hooks

Gate destructive changes with a callback or automatic backup:

```rust
use std::sync::Arc;

let options = ConvergeOptions {
    // Automatic SQL backup before destructive changes:
    backup_before_destructive: Some("backups/".into()),

    // Or a programmatic gate:
    pre_destructive_hook: Some(Arc::new(|changes| {
        if !changes.tables_to_drop.is_empty() {
            Err("refusing to drop tables in production".into())
        } else {
            Ok(())
        }
    })),
    ..Default::default()
};
```

## Human-Readable Diff

```rust
use turso_converge::{SchemaSnapshot, compute_diff};

let desired = SchemaSnapshot::from_schema_sql(SCHEMA).await?;
let actual = SchemaSnapshot::from_connection(&conn).await?;
println!("{}", compute_diff(&desired, &actual));
```

```
+ TABLE users
+ TABLE posts
~ TABLE documents: REBUILD
- TABLE legacy_cache
+ INDEX idx_posts_user
+ FTS INDEX idx_docs_fts
```

## ConnectionLike — Wrapper Support

For codebases that wrap `turso::Connection`:

```rust
use turso_converge::{ConnectionLike, converge_like, converge_like_with_options, schema_version_like};

struct MyConn { inner: turso::Connection }

impl ConnectionLike for MyConn {
    fn as_turso_connection(&self) -> &turso::Connection { &self.inner }
}

converge_like(&my_conn, SCHEMA).await?;
```

## Safety

turso-converge is designed to be safe by default:

| Feature | How |
|---------|-----|
| **Destructive change protection** | `ConvergePolicy` blocks table/column drops by default |
| **NOT NULL validation** | Caught before DDL — error names the exact column and table |
| **Foreign key integrity** | Deferred FK checks + post-rebuild `PRAGMA foreign_key_check` + FK-ordered creation |
| **Schema drift detection** | `PRAGMA schema_version` monitoring catches out-of-band changes |
| **Migration lease** | Cooperative lease prevents concurrent migrations (5-min TTL with phase-guarded refresh) |
| **Crash recovery** | `migration_in_progress` flag forces re-convergence; temp tables filtered from introspection |
| **Atomic state** | Hash + version + flags updated in a single transaction |
| **Protected namespace** | `_schema_meta`, `_converge_new_*`, `sqlite_*`, etc. never dropped |
| **AUTOINCREMENT preservation** | `sqlite_sequence` values saved and restored across rebuilds |
| **Busy timeout** | `PRAGMA busy_timeout` (default 5s) prevents busy errors |
| **Feature preflight** | Runtime probes detect FTS/vector/materialized view support before execution |
| **Read-only guard** | `MigrateError::ReadOnly` returned immediately on replica connections |

## Supported Schema Features

| Feature | Notes |
|---------|-------|
| Tables (CREATE, ALTER, DROP) | FK-ordered creation, policy-controlled drops |
| Standard indexes | |
| FTS indexes (`USING fts`) | Turso tantivy engine, 3-phase execution |
| Vector columns (`vector32`) | Diffed and preserved during rebuilds |
| Materialized views | Turso IVM |
| Regular views | Dependency-ordered, fixed-point resolution |
| Triggers | |
| Foreign keys | PRAGMA-based + SQL fallback detection |
| COLLATE clauses | Detected via DDL parsing |
| GENERATED columns | Excluded from data copy |
| STRICT / WITHOUT ROWID | Detected from DDL |
| AUTOINCREMENT | Sequence preserved across rebuilds |
| UNIQUE constraints | Via `PRAGMA index_list` |
| ADD COLUMN | O(1), nullable or with DEFAULT |
| DROP COLUMN | O(1) when eligible |

## CLI

Development and CI workflow commands:

```bash
turso-converge dump     my.db                # Extract schema SQL from existing DB
turso-converge validate schema.sql           # Validate syntax against in-memory DB
turso-converge diff     my.db schema.sql     # Human-readable diff
turso-converge plan     my.db schema.sql     # Show migration SQL (dry-run)
turso-converge check    my.db schema.sql     # Exit 0 if converged, 1 if not
turso-converge apply    my.db schema.sql     # Apply convergence
```

`dump` is the bootstrapping entry point — extract your existing database's schema to start using turso-converge. `check` is designed for CI gates — exits non-zero if the schema is not converged.

## Error Types

All operations return `Result<_, MigrateError>`:

| Error | When |
|-------|------|
| `PolicyViolation` | Policy blocked a destructive change |
| `Schema(msg)` | Validation error (NOT NULL without DEFAULT, empty schema, etc.) |
| `MigrationBusy` | Another migration holds the lease |
| `ReadOnly` | Connection is read-only |
| `UnsupportedFeature` | Schema uses FTS/vector/materialized views without builder flags |
| `ForeignKeyViolation` | FK check failed after table rebuild |
| `Statement` | SQL execution failed (includes the SQL, error, and phase) |
| `PreDestructiveHookRejected` | User hook rejected destructive changes |
| `Turso(err)` | Underlying database error |
| `Io` | File I/O error (converge_from_path, backup) |
| `InjectedFailure` | Test-only failpoint (never in production) |

## Tests

```bash
cargo test
```

152 tests. In-memory Turso databases, no external services.

## Full Documentation

See **[DOCUMENTATION.md](DOCUMENTATION.md)** for the comprehensive reference, including:

- Complete API reference with exact signatures and source links
- Configuration reference (all fields, defaults, types)
- Schema type system (SchemaSnapshot, TableInfo, ColumnInfo, etc.)
- Diff engine internals (14 categories, column classification rules, rename algorithm)
- Migration plan generation (statement ordering, FK topological sort, rebuild procedure)
- Execution engine (3-phase execution, view fixed-point resolution, lease verification)
- Safety feature details (each mechanism explained in depth)
- SQL normalization algorithm
- Introspection details (PRAGMAs, capability probing, snapshot caching)
- Internal state table (`_schema_meta` keys)
- Architecture and module map

## Background

Rust implementation of the approach from David Rothlis and William Manley's [Simple declarative schema migration for SQLite](https://david.rothlis.net/declarative-schema-migration-for-sqlite/) (2022), extended with:

- Triggers and views (with dependency ordering)
- Turso tantivy FTS with 3-phase execution
- Vector columns (`vector32`)
- BLAKE3 hash fast-path + drift detection
- Crash recovery with phase cursor
- Policy enforcement and NOT NULL validation
- Cooperative migration lease with atomic phase-guarded refresh
- Structured tracing and detailed reporting

## License

MIT
