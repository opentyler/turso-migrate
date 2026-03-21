# turso-converge vs turso vs SQLx

A comparison of three Rust libraries for working with SQLite-compatible databases, each solving a different layer of the stack.

| | turso-converge | turso (crate) | SQLx (SQLite) |
|---|---|---|---|
| **What it is** | Declarative schema convergence engine | Database driver (Rust-native SQLite rewrite) | Async database toolkit with compile-time checked SQL |
| **Primary job** | Manage schema evolution | Execute queries | Execute queries + manage migrations |
| **Approach to schema** | Declarative (desired-state) | None (manual DDL) | Imperative (sequential migrations) |
| **Current version** | 0.1.0 | 0.6.0-pre.3 | 0.8.x |

---

## The Three Layers

These are not competing libraries. They solve different problems:

```
┌─────────────────────────────────────────────────┐
│  Your Application Code                          │
├─────────────────────────────────────────────────┤
│  turso-converge    │  SQLx migrations           │  ← Schema management
│  (declarative)     │  (imperative)              │
├────────────────────┼────────────────────────────┤
│  turso (driver)    │  SQLx (driver + macros)    │  ← Query execution
├────────────────────┼────────────────────────────┤
│  Turso engine      │  libsqlite3 (C)            │  ← Database engine
│  (Rust-native)     │                            │
└────────────────────┴────────────────────────────┘
```

turso-converge sits on top of the `turso` driver. SQLx bundles its own driver and migration system. You would never choose between turso-converge and SQLx — you'd choose between the Turso ecosystem and the SQLx ecosystem.

---

## Schema Management

The biggest philosophical difference.

### turso-converge: Declarative

You define what the schema **should look like**. The library figures out the DDL.

```rust
const SCHEMA: &str = r#"
    CREATE TABLE users (
        id TEXT PRIMARY KEY,
        email TEXT NOT NULL UNIQUE,
        display_name TEXT  -- was "name" last week; was added this week
    );
    CREATE INDEX idx_users_email ON users(email);
"#;

// One call. Every time. It just works.
converge(&conn, SCHEMA).await?;
```

No migration files. No version numbers. No "what order do these run in?" The schema file is the single source of truth. turso-converge introspects the live database, computes a diff across 15 categories, generates an FK-ordered migration plan, and executes it — or returns in <1ms if nothing changed.

### SQLx: Imperative (Sequential Migrations)

You write migration scripts that describe **how to transform** from one state to the next.

```
migrations/
├── 001_create_users.sql
├── 002_add_email_column.sql
├── 003_rename_name_to_display_name.sql
└── 004_add_email_index.sql
```

Each file runs once, in order. The `_sqlx_migrations` table tracks which have been applied. Reversible migrations are supported via `.up.sql` / `.down.sql` pairs.

```rust
sqlx::migrate!("./migrations").run(&pool).await?;
```

### turso (driver): None

The `turso` crate is a raw database driver. It has no migration system, no schema management, no DDL generation. You execute `CREATE TABLE` statements yourself, or use a library like turso-converge or turso-migrate on top of it.

### When Each Approach Wins

| Scenario | Best fit |
|---|---|
| Rapid prototyping, schema changes multiple times per day | turso-converge |
| Team of 1-3 devs, schema is the product | turso-converge |
| Large team with strict review process for each migration | SQLx migrations |
| Need to run arbitrary data transformations between schema versions | SQLx migrations |
| Embedded/edge databases (one per user, thousands of instances) | turso-converge |
| Schema defined by an external system or config | turso-converge |
| PostgreSQL/MySQL/SQLite support needed in one codebase | SQLx |

---

## Query Execution

### turso (driver)

Raw SQL, async, parameter binding. Minimal API surface.

```rust
let db = turso::Builder::new_local("my.db").build().await?;
let conn = db.connect()?;

conn.execute("INSERT INTO users (id, name) VALUES (?1, ?2)", ("u1", "alice")).await?;

let mut rows = conn.query("SELECT id, name FROM users WHERE id = ?1", ("u1",)).await?;
while let Some(row) = rows.next().await? {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
}
```

No compile-time SQL checking. No connection pooling (single connection per `db.connect()`). Errors are runtime `turso::Error` values.

### SQLx (SQLite)

Raw SQL with optional compile-time verification via macros.

```rust
let pool = SqlitePool::connect("sqlite:my.db").await?;

// Runtime SQL — same as turso, but with connection pooling
sqlx::query("INSERT INTO users (id, name) VALUES (?1, ?2)")
    .bind("u1").bind("alice")
    .execute(&pool).await?;

// Compile-time checked SQL — the macro validates against a real database at build time
let user = sqlx::query_as!(User, "SELECT id, name FROM users WHERE id = ?1", "u1")
    .fetch_one(&pool).await?;
```

The `query!` macro connects to the database during compilation, validates column names, types, and nullability. Typos in SQL are caught before the code ships.

### turso-converge

Not a query execution library. It only manages schema. You use the `turso` driver for queries.

### Comparison

| Capability | turso | SQLx | turso-converge |
|---|---|---|---|
| Raw SQL execution | ✓ | ✓ | — |
| Parameter binding | `?1`, `?2` positional | `?1` or `$1` positional | — |
| Compile-time SQL checking | — | ✓ (via macro) | — |
| Connection pooling | — | ✓ | — |
| Transactions | ✓ | ✓ | — |
| Streaming rows | ✓ | ✓ | — |
| Multiple database backends | — | ✓ (Postgres, MySQL, SQLite) | — |

---

## Type Safety

| Aspect | turso | SQLx | turso-converge |
|---|---|---|---|
| Query parameter types | Runtime checked | **Compile-time checked** (via macro) | N/A |
| Result column types | Manual extraction by index | **Compile-time mapped** (via macro) | N/A |
| Schema correctness | No validation | Compile-time SQL validation | **Runtime diff engine** — detects schema drift, validates constraints |
| NULL safety | Runtime `Option` extraction | **Compile-time nullability** inference | N/A |

SQLx's compile-time checking is its standout feature. The `query!` macro catches:
- Misspelled column names
- Wrong parameter types
- Missing or extra bind parameters
- Nullability mismatches

Neither turso nor turso-converge offers compile-time query validation. turso-converge provides a different kind of safety: it validates that your schema is internally consistent (no NOT NULL columns without defaults, no FK violations, no unsupported features) and prevents accidental destructive changes via policy enforcement.

---

## Migration Features

| Feature | turso-converge | SQLx | turso |
|---|---|---|---|
| **Migration model** | Declarative (desired-state) | Imperative (sequential) | None |
| **Migration files** | 1 file (schema.sql) | N files (001_...sql, 002_...sql) | — |
| **Automatic diff** | ✓ (15 categories) | — | — |
| **Automatic plan generation** | ✓ (FK-ordered, 12-step rebuilds) | — | — |
| **Reversible migrations** | ✓ (`rollback_to_previous`) | ✓ (.up.sql / .down.sql pairs) | — |
| **Dry-run mode** | ✓ (returns plan SQL) | — | — |
| **Table rebuild (ALTER TABLE workaround)** | ✓ (automatic 12-step procedure) | Manual (you write the SQL) | Manual |
| **CHECK constraint diffing** | ✓ | — | — |
| **Partial index diffing** | ✓ | — | — |
| **Column rename detection** | ✓ (heuristic + hints) | — | — |
| **Destructive change protection** | ✓ (ConvergePolicy) | — | — |
| **Crash recovery** | ✓ (in-progress flag + re-convergence) | — | — |
| **Concurrent migration prevention** | ✓ (cooperative lease) | ✓ (lock row) | — |
| **Schema drift detection** | ✓ (PRAGMA schema_version) | — | — |
| **Fast-path (no-change detection)** | ✓ (<1ms via BLAKE3 hash) | — | — |
| **Data migrations** | ✓ (idempotent, tracked by ID) | Via migration files | — |
| **FTS index support** | ✓ (3-phase execution) | — | — |
| **Pre-destructive hooks** | ✓ (callback or backup) | — | — |
| **Human-readable diff** | ✓ (`Display` for `SchemaDiff`) | — | — |
| **CLI** | ✓ (validate/diff/plan/check/apply) | ✓ (sqlx-cli: migrate, prepare) | — |

---

## Database Engine

| Aspect | turso / turso-converge | SQLx (SQLite) |
|---|---|---|
| **Engine** | Turso — full Rust rewrite of SQLite | libsqlite3 — C library via `libsqlite3-sys` |
| **FTS** | Tantivy-powered (Rust) | FTS5 (C) |
| **Vector search** | Native `vector32` columns | Via extensions |
| **Materialized views** | Native (IVM) | Not supported |
| **Embedded replicas** | Native (local + remote sync) | Not supported |
| **Encryption** | Optional (feature flag) | Via SQLCipher extension |
| **Async model** | Native async | `spawn_blocking` wrapper around C calls |
| **Maturity** | Beta (0.6.x pre-release) | Stable (production-ready) |

---

## Ecosystem and Maturity

| | turso-converge | turso | SQLx |
|---|---|---|---|
| **crates.io** | Git dependency | Pre-release on crates.io | Stable on crates.io |
| **GitHub stars** | New project | ~1K | ~17K |
| **Contributors** | Small team | Turso team | 440+ |
| **Production readiness** | Early (186 tests, comprehensive safety) | Beta | Production-ready |
| **Documentation** | README + DOCUMENTATION.md | Docs.rs | Docs.rs + The Book |
| **Multi-database** | Turso only | Turso only | PostgreSQL, MySQL, SQLite |

---

## Decision Guide

**Choose the Turso stack (turso + turso-converge) when:**
- You're building on Turso (edge databases, embedded replicas, per-user DBs)
- You want declarative schema management — define it once, converge everywhere
- You need FTS, vector search, or materialized views
- Schema changes are frequent and you don't want to manage migration files
- You're running thousands of database instances with the same schema

**Choose SQLx when:**
- You need PostgreSQL, MySQL, or standard SQLite support
- Compile-time SQL checking is important to your team
- You prefer explicit, reviewable migration files
- You need connection pooling built into the driver
- You want a battle-tested, production-proven ecosystem
- You need to run complex data transformations between schema versions

**Use both patterns when:**
- turso-converge for schema management + turso for queries (Turso ecosystem)
- SQLx for queries + SQLx migrations for schema (SQLx ecosystem)

These are stack choices, not library choices. Pick the ecosystem that fits your database engine and workflow preferences.
