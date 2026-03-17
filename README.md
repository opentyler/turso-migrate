# turso-migrate

Declarative schema convergence for [Turso](https://turso.tech/) databases.

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

## Comparison with the Original

turso-migrate is a Rust implementation of the approach described in David Rothlis and William Manley's [Simple declarative schema migration for SQLite](https://david.rothlis.net/declarative-schema-migration-for-sqlite/) (2022). That article describes a Python migrator used in production at [stb-tester.com](https://stb-tester.com) since 2019. The core idea is identical: define desired schema in one SQL file, create an in-memory pristine database from it, introspect both databases via `sqlite_schema` and `PRAGMA table_info`, diff, and converge.

turso-migrate extends the original design in several areas and inherits some of the same fundamental limitations.

### What turso-migrate adds beyond the original

| Area | Original (Python) | turso-migrate |
|------|-------------------|---------------|
| **Triggers & views** | Explicitly not supported ("not for any fundamental reason, it's just that we don't use them") | Full support: CREATE/DROP/ALTER for both, including Turso materialized views (IVM) |
| **FTS indexes** | Not applicable (standard SQLite) | Full support for Turso's tantivy-powered FTS indexes (`CREATE INDEX ... USING fts`), handled in a separate non-transactional execution phase because Turso FTS cannot run inside transactions |
| **Vector columns** | Not applicable | `vector32(N)` columns are diffed and preserved during table rebuilds |
| **Schema change detection** | None — always runs full introspection | BLAKE3 hash fast-path: stores hash of schema SQL in `_schema_meta`, skips everything on match (<1ms) |
| **Crash recovery** | None | Sets `migration_in_progress` flag before convergence, clears after. On next startup, a set flag forces slow-path re-convergence regardless of hash match |
| **FK-aware table creation** | Tables created in schema-file order | Topological sort by REFERENCES: tables are created in FK dependency order so constraints are satisfied |
| **Execution model** | Single transaction for all DDL | Three-phase execution: (1) transactional DDL, (2) non-transactional FTS, (3) transactional views/triggers — because FTS index creation cannot run inside a transaction |
| **Schema generation** | Not included (mentions Graphviz ER diagrams) | `SchemaSnapshot::to_sql()` generates deterministic DDL from a live database, useful for bootstrapping |
| **Dry-run mode** | Not mentioned | `Migrator::plan()` returns the migration plan without executing it |
| **Schema versioning** | Uses SQLite's `PRAGMA user_version` | Maintains `schema_version` table with version counter and timestamp, incremented on each DDL change |

### Limitations shared with the original

These are fundamental to the declarative approach, not implementation bugs:

**No column rename detection.** If you rename a column from `name` to `display_name`, the migrator sees "column `name` removed, column `display_name` added." Data in the old column is lost. The original article handles this the same way — the migrator treats it as a table rebuild where only shared column names are copied.

*Workaround:* Run a manual `ALTER TABLE ... RENAME COLUMN` before calling `converge()`.

**No data migrations.** The migrator handles schema (DDL) only. If you need to transform existing data — populate a new column from old columns, backfill a NOT NULL column, migrate data between tables — you must do that separately.

*Workaround:* Run data migration SQL after `converge()`, guarded by idempotent `WHERE` clauses (same pattern the original article recommends).

**Table rebuilds are slow with large data.** When a column changes type, gains/loses NOT NULL, or changes its default, the migrator follows SQLite's [12-step ALTER TABLE procedure](https://www.sqlite.org/lang_altertable.html#otheralter): create temp table → INSERT INTO ... SELECT → DROP original → RENAME temp. This copies every row. On a million-row table, this takes real time.

**Table rebuilds may change rowid values.** The original article notes this too. If your code depends on SQLite's internal rowid staying stable across migrations, table rebuilds will break that assumption.

**Destructive changes are one-way.** There is no "undo" or "rollback to previous schema." If you drop a column or table, the data is gone. The `Migrator` API has `allow_deletions(false)` (same as the original) to prevent accidental drops, but `converge()` allows deletions by default.

### Limitations specific to turso-migrate

These are implementation limitations that could be improved:

**FK reference extraction is manual byte-by-byte parsing.** The `referenced_tables()` function in `plan.rs` parses `REFERENCES table_name` by scanning bytes rather than using a SQL parser. It handles quoted identifiers (`"table"`) and unquoted names correctly for simple cases, but complex FK syntax (multi-column references, inline constraints mixed with comments) could confuse it. This affects FK-aware table creation ordering.

*Risk:* Low in practice — FK references in schema SQL are typically straightforward. A SQL parser would be more robust but adds a dependency.

**View dependency detection is token-based.** `view_depends_on_table()` checks if a table name appears as a word token in the view SQL. This can produce false positives (a table name that appears in a string literal or comment would match) and false negatives (aliased or qualified references might not match the simple token check).

*Risk:* Medium — false positives cause unnecessary view recreation (harmless but wasteful). False negatives could leave a view referencing a stale table after rebuild.

**SQL comparison uses whitespace normalization.** The diff engine compares `CREATE` statements by collapsing whitespace and lowercasing. Two statements that are semantically identical but syntactically different in ways beyond whitespace (e.g., `INTEGER` vs `INT`, extra parentheses) will be seen as different, triggering unnecessary rebuilds.

*Risk:* Low — unnecessary rebuilds are harmless (idempotent), just slower.

**Temp table names are predictable.** Table rebuilds use `_converge_new_{table_name}` as the temporary table name. If your schema has a table with that name, the rebuild will conflict.

*Risk:* Negligible — don't name your tables `_converge_new_*`.

**No VACUUM after migrations.** The original article runs VACUUM to repack the database file after migrations. turso-migrate does not. After dropping tables or rebuilding large tables, the database file may contain free pages that aren't reclaimed.

*Workaround:* Run `VACUUM` manually after convergence if file size matters.

**FTS indexes require experimental Turso features.** FTS (`USING fts`) and materialized views are gated behind `turso::Builder` flags (`.experimental_index_method(true)`, `.experimental_materialized_views(true)`). If these flags aren't set on the target database connection, convergence will fail when trying to create FTS indexes. The error message from Turso (`"unknown module name 'fts'"`) doesn't indicate the fix.

*Workaround:* Always set both experimental flags on the `turso::Builder` if your schema uses FTS or materialized views. The Quick Start section of this README shows the correct setup.

**No `allow_deletions` flag on `converge()`.** The `Migrator` builder API has `.allow_deletions(false)` to prevent accidental table/column drops. The simpler `converge()` function always allows deletions. If you need deletion protection, use `Migrator` instead.

**Hash sensitivity to whitespace.** The BLAKE3 fast-path hashes the raw `schema_sql` bytes. If you reformat the schema file (add a trailing newline, change indentation), the hash changes and triggers a full convergence — even though the schema is semantically identical. The full convergence will then find no diff and complete quickly, but it's slower than the fast-path.

*Workaround:* None needed — the slow path correctly detects "no changes" and finishes fast. It's just not sub-millisecond like a hash match.

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

- [Simple declarative schema migration for SQLite (Rothlis & Manley, 2022)](https://david.rothlis.net/declarative-schema-migration-for-sqlite/) — **The direct inspiration for turso-migrate.** Python implementation of the same core algorithm: pristine DB from schema SQL → introspect both → diff → converge. turso-migrate extends this with BLAKE3 fast-path, crash recovery, FTS/vector/view/trigger support, and FK-aware ordering. See "Comparison with the Original" above for a detailed breakdown.
- [Declarative vs Versioned Migrations (Atlas)](https://atlasgo.io/concepts/declarative-vs-versioned) — The declarative migration paradigm that turso-migrate implements
- [Deep Dive into Declarative Migrations (Atlas)](https://atlasgo.io/blog/2024/10/31/declarative-migrations-deepdive) — Design philosophy and challenges
- [Database per User (Turso)](https://turso.tech/blog/give-each-of-your-users-their-own-sqlite-database-b74445f4) — The per-user database architecture that motivated constant-cost convergence
- [SQLite as an Application File Format](https://www.sqlite.org/appfileformat.html) — Why SQLite (and Turso) for embedded databases
- [All-In on Server-Side SQLite (Fly.io)](https://fly.io/blog/all-in-on-sqlite-litestream/) — The server-side SQLite movement
- [BLAKE3 Hash Function](https://github.com/BLAKE3-team/BLAKE3) — Fast content-addressable hashing for schema change detection
- [SQLx — Compile-time Checked SQL (Launchbadge)](https://github.com/launchbadge/sqlx) — Inspiration for compile-time SQL tooling in Rust

## License

MIT
