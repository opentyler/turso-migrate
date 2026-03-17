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
        .experimental_index_method(true)        // Required for FTS indexes
        .experimental_materialized_views(true)  // Required for materialized views
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

### `converge(conn, schema_sql)`

The primary API. Pass your schema SQL as a string. Computes a BLAKE3 hash, checks it against the hash stored in the database, and skips everything if they match. On mismatch, runs the full convergence pipeline.

```rust
const SCHEMA: &str = include_str!("../my_schema.sql");
turso_migrate::converge(&conn, SCHEMA).await?;
```

### `converge_from_path(conn, path)`

Reads a schema file from disk, then converges. Returns `MigrateError::Io` if the file can't be read.

```rust
turso_migrate::converge_from_path(&conn, "schemas/turso_schema.sql").await?;
```

### `SchemaSnapshot::to_sql()`

Introspect a live database and generate deterministic DDL. Tables are topologically sorted by foreign key dependencies. Useful for bootstrapping — reverse-engineer an existing database into a schema file.

```rust
use turso_migrate::SchemaSnapshot;

let snapshot = SchemaSnapshot::from_connection(&conn).await?;
std::fs::write("turso_schema.sql", snapshot.to_sql())?;
```

### `Migrator`

Builder API for dry-runs and deletion protection.

```rust
use turso_migrate::Migrator;

let migrator = Migrator::new(schema_sql)
    .allow_deletions(false);  // Don't drop unknown tables

let plan = migrator.plan(&conn).await?;   // Preview without applying
let plan = migrator.migrate(&conn).await?; // Apply
```

### `schema_version(conn)`

Returns the schema version counter (incremented each time DDL is applied).

```rust
let version = turso_migrate::schema_version(&conn).await?;
```

## How It Works

```
Developer edits turso_schema.sql (desired end-state)
    ↓
Consumer embeds: include_str!("turso_schema.sql")
    ↓
On each database connection:
    1. BLAKE3(schema_sql) → compare against stored hash
    2. Match → return (<1ms, two SELECT queries)
    3. Mismatch → full convergence:
       a. Build pristine snapshot (in-memory Turso DB from schema SQL)
       b. Introspect actual database (sqlite_schema + PRAGMA table_info)
       c. Compute diff (12 categories)
       d. Generate migration plan (FK-ordered, 3-phase)
       e. Execute plan
       f. Store new hash, increment schema version
```

### Supported Features

| Feature | Support |
|---------|---------|
| Tables (CREATE, ALTER, DROP) | ✅ |
| Standard indexes | ✅ |
| FTS indexes (tantivy, `USING fts`) | ✅ |
| Vector columns (`vector32`) | ✅ |
| Materialized views (IVM) | ✅ |
| Regular views | ✅ |
| Triggers | ✅ |
| Foreign keys | ✅ (dependency-ordered) |
| ADD COLUMN (nullable or with DEFAULT) | ✅ (O(1), no rebuild) |
| Crash recovery | ✅ (`migration_in_progress` flag) |

### The 12-Step ALTER TABLE Procedure

SQLite has limited native ALTER TABLE support. For changes beyond ADD/DROP COLUMN — changing types, modifying NOT NULL, altering primary keys — SQLite prescribes a [12-step procedure](https://www.sqlite.org/lang_altertable.html#otheralter). turso-migrate follows it for all table rebuilds:

| SQLite Step | turso-migrate | Notes |
|-------------|---------------|-------|
| 1. Disable FK constraints | **Skipped** | Turso defaults to `foreign_keys=OFF`. See [Known Limitations](#known-limitations). |
| 2. Begin transaction | `BEGIN IMMEDIATE` | Acquires write lock upfront to prevent `SQLITE_BUSY`. |
| 3. Remember indexes/triggers/views | Introspection phase | `SchemaSnapshot::from_connection()` captures everything from `sqlite_schema` before DDL runs. |
| 4. CREATE TABLE "new_X" | `CREATE TABLE "_converge_new_{table}" (...)` | Uses the **correct** order (create new → copy → drop old → rename), not the incorrect order SQLite warns against. |
| 5. Copy data | `INSERT INTO ... SELECT shared_cols` | Only copies columns present in both schemas. New columns get DEFAULT values. |
| 6. Drop old table | `DROP TABLE "{table}"` | |
| 7. Rename new to old | `ALTER TABLE ... RENAME TO` | |
| 8. Recreate indexes/triggers/views | Auto-detected by diff engine | Objects on rebuilt tables are automatically dropped and recreated. |
| 9. Recreate affected views | Token-based dependency check | Views referencing rebuilt tables are dropped and recreated. |
| 10. FK check | **Skipped** | See [Known Limitations](#known-limitations). |
| 11. Commit | `COMMIT` | |
| 12. Re-enable FK constraints | **Skipped** | See [Known Limitations](#known-limitations). |

### Execution Phases

All DDL runs in three phases to handle Turso-specific constraints:

```
Phase 1 (transactional):
  DROP triggers → DROP views → DROP indexes → DROP tables →
  CREATE tables (FK-ordered) → ADD COLUMN → Table rebuilds → CREATE indexes

Phase 2 (non-transactional):
  DROP FTS indexes → CREATE FTS indexes

Phase 3 (transactional):
  CREATE views → CREATE triggers
```

FTS indexes cannot be created inside transactions (Turso/tantivy limitation). Views and triggers run last because they may reference FTS-indexed tables.

## Known Limitations

### Fundamental (shared with the declarative approach)

**No column rename detection.** Renaming `name` to `display_name` is seen as "drop `name`, add `display_name`" — data in the old column is lost.
*Workaround:* Run `ALTER TABLE ... RENAME COLUMN` manually before `converge()`.

**No data migrations.** Schema (DDL) only. Transforming existing data, backfilling columns, or moving data between tables must be done separately.
*Workaround:* Run idempotent data migration SQL after `converge()`.

**Table rebuilds are slow with large data.** The 12-step procedure copies every row. A million-row table takes real time.

**Table rebuilds may change rowid values.** If your code depends on SQLite's internal rowid staying stable, table rebuilds will break that.

**Destructive changes are one-way.** No undo. Dropped columns and tables lose data. Use `Migrator::new(sql).allow_deletions(false)` for protection. Note: `converge()` allows deletions by default.

### Implementation-specific

**No FK pragma handling (12-step deviations).** turso-migrate does not disable FK constraints before migration or run `PRAGMA foreign_key_check` after. This is safe under Turso's default `foreign_keys=OFF`. If your app explicitly enables FK enforcement, intermediate rebuild states could theoretically trigger violations within the transaction.

**FK reference extraction is byte-level parsing.** The `referenced_tables()` function parses `REFERENCES table_name` by scanning bytes, not a SQL parser. Handles quoted and unquoted identifiers for simple cases. Complex FK syntax with comments could confuse it.
*Risk:* Low — FK references in schema SQL are typically straightforward.

**View dependency detection is token-based.** Checks if a table name appears as a word token in view SQL. False positives (table name in a string literal) cause unnecessary recreation. False negatives (aliased references) could leave stale views.
*Risk:* Medium — false positives are harmless; false negatives are the real concern.

**SQL comparison is whitespace-normalized.** `INTEGER` vs `INT` or extra parentheses trigger unnecessary rebuilds. Rebuilds are idempotent, just slower.

**No VACUUM after migrations.** Free pages from drops and rebuilds aren't reclaimed. Run `VACUUM` manually if file size matters.

**FTS requires experimental Turso flags.** If `.experimental_index_method(true)` isn't set on your `turso::Builder`, FTS schema convergence fails with `"unknown module name 'fts'"` — a confusing error. Always set both experimental flags if your schema uses FTS or materialized views.

**Hash sensitivity to whitespace.** Reformatting the schema file (changing indentation, trailing newlines) changes the BLAKE3 hash and triggers a full convergence. The slow path correctly finds no diff and completes fast, but it's not sub-millisecond like a hash match.

**Temp table names are predictable.** Rebuilds use `_converge_new_{table}`. Don't name your tables that.

## Background

turso-migrate is a Rust implementation of the approach described in David Rothlis and William Manley's [Simple declarative schema migration for SQLite](https://david.rothlis.net/declarative-schema-migration-for-sqlite/) (2022), a Python migrator used in production at [stb-tester.com](https://stb-tester.com) since 2019. The core algorithm is identical: desired schema in one SQL file → pristine in-memory database → introspect both via `sqlite_schema` and `PRAGMA table_info` → diff → converge.

turso-migrate extends the original with:

| Area | Original (Python) | turso-migrate |
|------|-------------------|---------------|
| Triggers & views | Not supported | Full support, including materialized views |
| FTS indexes | N/A (standard SQLite) | Turso tantivy FTS with 3-phase execution |
| Vector columns | N/A | `vector32(N)` diffed and preserved |
| Change detection | None (always full introspection) | BLAKE3 hash fast-path (<1ms) |
| Crash recovery | None | `migration_in_progress` flag |
| FK ordering | Schema-file order | Topological sort by REFERENCES |
| Schema generation | Not included | `SchemaSnapshot::to_sql()` |
| Dry-run | Not mentioned | `Migrator::plan()` |

## Running Tests

```bash
cargo test
```

In-memory Turso databases, no external services. Covers convergence, diff (12 categories), plan generation, execution (3 phases), introspection, schema round-trip, legacy bridge, and error handling.

## Inspiration

- [Simple declarative schema migration for SQLite (Rothlis & Manley, 2022)](https://david.rothlis.net/declarative-schema-migration-for-sqlite/) — The direct inspiration. See [Background](#background) for a detailed comparison.

## License

MIT
