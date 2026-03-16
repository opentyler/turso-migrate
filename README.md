# turso-migrate

Declarative schema convergence for [Turso](https://turso.tech/) and libSQL databases.

Instead of numbered migration files (`001_up.sql`, `002_up.sql`, ...), you define your desired schema in a single SQL file and turso-migrate automatically diffs and converges any database to match it. A BLAKE3 hash fast-path makes subsequent checks near-instant when the schema hasn't changed.

## Requirements

- **Rust** 1.85+ (edition 2024)
- **Tokio** multi-thread runtime (`#[tokio::main(flavor = "multi_thread")]`)
- **turso** 0.5.0-pre.13 with `turso_core` FTS features enabled

## Quick Start

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

// Your schema — define the desired end-state
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
    // If using FTS or materialized views, enable experimental features:
    let db = turso::Builder::new_local("my.db")
        .experimental_index_method(true)        // Required for FTS indexes
        .experimental_materialized_views(true)  // Required for materialized views
        .build()
        .await?;
    let conn = db.connect()?;

    // Converge: first call creates tables, subsequent calls are <1ms (hash match)
    converge(&conn, SCHEMA).await?;

    Ok(())
}
```

## API Reference

### `converge(conn, schema_sql)` — Runtime Schema Input

The primary API. Pass your schema SQL as a string. turso-migrate will:

1. Compute a BLAKE3 hash of `schema_sql`
2. Compare against the hash stored in the database's `_schema_meta` table
3. **Fast-path**: If hashes match, return immediately (<1ms)
4. **Slow-path**: Build a pristine snapshot from your SQL, introspect the actual database, compute a diff, generate a migration plan, and execute it

```rust
// Embed schema at compile time
const SCHEMA: &str = include_str!("../my_schema.sql");

turso_migrate::converge(&conn, SCHEMA).await?;
```

### `converge_from_path(conn, path)` — Path-Based

Reads a schema file from disk, then converges. Useful for CLI tools or dynamic schema loading.

```rust
turso_migrate::converge_from_path(&conn, "schemas/turso_schema.sql").await?;
```

Returns `MigrateError::Io` if the file doesn't exist or can't be read.

### `SchemaSnapshot::to_sql()` — Generate Schema from Existing Database

Introspect a live database and generate deterministic DDL output. Useful for bootstrapping turso-migrate from an existing database.

```rust
use turso_migrate::SchemaSnapshot;

let snapshot = SchemaSnapshot::from_connection(&conn).await?;
let ddl = snapshot.to_sql();
std::fs::write("turso_schema.sql", ddl)?;

// The generated SQL is directly executable:
// conn.execute_batch(&ddl).await?;
```

Tables are topologically sorted by foreign key dependencies — the output is safe to execute against a fresh database.

### `Migrator` — Builder API with Dry-Run

For advanced use cases: preview changes before applying, or control deletion behavior.

```rust
use turso_migrate::Migrator;

let migrator = Migrator::new(schema_sql)
    .allow_deletions(false);  // Don't drop tables not in desired schema

// Dry-run: see what would change without applying
let plan = migrator.plan(&conn).await?;
println!("Planned {} statements", plan.len());

// Apply changes
let plan = migrator.migrate(&conn).await?;
```

### `schema_version(conn)` — Read Schema Version

Returns the current schema version counter (incremented each time DDL is applied).

```rust
let version = turso_migrate::schema_version(&conn).await?;
```

## How It Works

```
Developer edits turso_schema.sql (desired end-state)
    ↓
Consumer embeds SQL: include_str!("turso_schema.sql")
    ↓
On database connection:
    1. BLAKE3 hash of schema SQL
    2. Compare against stored hash in _schema_meta
    3. Hash match → skip (< 1ms, two SELECT queries)
    4. Hash mismatch → full convergence:
       a. Build pristine snapshot (in-memory Turso DB)
       b. Introspect actual database schema
       c. Compute diff (12 categories: tables, columns, indexes, FTS, views, triggers)
       d. Generate migration plan (FK-dependency-ordered DDL)
       e. Execute plan (transactional DDL → non-transactional FTS → views/triggers)
       f. Update stored hash
```

### What turso-migrate handles automatically

- Table creation, deletion, and column-level rebuilds
- `ADD COLUMN` for nullable columns with defaults
- Full table rebuild for column type/constraint changes (SQLite's ALTER TABLE limitations)
- Standard and FTS indexes (tantivy-powered via Turso)
- Regular and materialized views
- Triggers
- Foreign key dependency ordering
- Crash recovery via `migration_in_progress` flag

## Supported Turso/SQLite Features

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

## Running Tests

```bash
cargo test
```

Tests use in-memory Turso databases — no external services needed. The test suite covers:
- Schema convergence (idempotency, crash recovery, fast-path)
- Diff algorithm (all 12 change categories)
- Plan generation (FK ordering, table rebuilds)
- Execution (transactional + non-transactional phases)
- Introspection (tables, columns, indexes, FTS, views)
- Schema generation round-trip (`to_sql()`)
- Legacy bridge (sequential migration table detection)
- Error handling (empty SQL, invalid SQL, missing files)

## Inspiration & References

- [Declarative vs Versioned Migrations (Atlas)](https://atlasgo.io/concepts/declarative-vs-versioned) — The declarative migration paradigm that turso-migrate implements
- [Deep Dive into Declarative Migrations (Atlas)](https://atlasgo.io/blog/2024/10/31/declarative-migrations-deepdive) — Design philosophy and challenges
- [Database per User (Turso)](https://turso.tech/blog/give-each-of-your-users-their-own-sqlite-database-b74445f4) — The per-user database architecture that motivated constant-cost convergence
- [SQLite as an Application File Format](https://www.sqlite.org/appfileformat.html) — Why SQLite (and Turso) for embedded databases
- [All-In on Server-Side SQLite (Fly.io)](https://fly.io/blog/all-in-on-sqlite-litestream/) — The server-side SQLite movement
- [BLAKE3 Hash Function](https://github.com/BLAKE3-team/BLAKE3) — Fast content-addressable hashing for schema change detection
- [SQLx — Compile-time Checked SQL (Launchbadge)](https://github.com/launchbadge/sqlx) — Inspiration for compile-time SQL tooling in Rust

## License

MIT
