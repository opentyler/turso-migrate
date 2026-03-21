# Improving the turso Crate

An analysis of gaps in the `turso` Rust crate (v0.6.0-pre.3) and concrete proposals for improvement — informed by building turso-converge on top of it.

This document is organized by severity of impact, with evidence from our own codebase, comparisons to mature alternatives (rusqlite, SQLx, diesel), and references to open GitHub issues.

---

## Table of Contents

- [Evidence From This Codebase](#evidence-from-this-codebase)
- [Tier 1: Fundamental API Gaps](#tier-1-fundamental-api-gaps)
- [Tier 2: Missing Primitives](#tier-2-missing-primitives)
- [Tier 3: Ecosystem Integration](#tier-3-ecosystem-integration)
- [Tier 4: Performance & Observability](#tier-4-performance--observability)
- [Tier 5: Drastic Structural Changes](#tier-5-drastic-structural-changes)
- [Summary Matrix](#summary-matrix)

---

## Evidence From This Codebase

turso-converge has 70+ query/execute calls across 4 source files. The patterns we were forced to write reveal what the driver should provide natively.

| Pattern | Count | Files | What it reveals |
|---|---|---|---|
| `row.get(N)` positional extraction | 50 | introspect.rs, converge.rs, execute.rs | No row-to-struct mapping, no column-name access |
| `conn.execute("BEGIN IMMEDIATE", ())` | 20 | execute.rs, converge.rs | No transaction API |
| `format!("PRAGMA ...", val)` | 18 | introspect.rs, execute.rs, converge.rs | No typed PRAGMA interface |
| `execute_batch` with manual semicolons | 5 | execute.rs, introspect.rs | Batch API requires workarounds |
| Custom `normalize_sql` (80 lines) | 1 | diff.rs | No SQL normalization utility |
| Custom `quote_ident` | 1 | plan.rs | No identifier quoting helper |
| Custom `is_missing_dependency_error` via string matching | 1 | execute.rs | No structured error types |
| 6 runtime capability probe functions | 6 | introspect.rs | No feature detection API |
| `ConnectionLike` wrapper trait | 1 | connection.rs | No standard connection abstraction |

---

## Tier 1: Fundamental API Gaps

These are things every database driver should have. Their absence forces every consumer to reimplement the same patterns.

### 1.1 Transaction API

**The problem**: turso has no `Transaction` type. You execute `BEGIN`, `COMMIT`, `ROLLBACK` as raw SQL strings and manage rollback-on-error yourself.

**What we write today** (execute.rs):
```rust
conn.execute("BEGIN IMMEDIATE", ()).await?;
for stmt in stmts {
    if let Err(err) = conn.execute(stmt, ()).await {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(err.into());
    }
}
conn.execute("COMMIT", ()).await?;
```

**What it should look like**:
```rust
let tx = conn.begin_immediate().await?;
for stmt in stmts {
    tx.execute(stmt, ()).await?; // auto-rollback on drop
}
tx.commit().await?;
```

**What rusqlite provides**: `Transaction` type with `Drop`-based rollback, `SAVEPOINT` support, `TransactionBehavior` enum (Deferred, Immediate, Exclusive).

**What SQLx provides**: `pool.begin()` returning a `Transaction<'_, Sqlite>` that implements `Executor` and rolls back on drop.

**Impact**: We have 20 manual BEGIN/COMMIT/ROLLBACK sites. A `Transaction` type with drop-based rollback would eliminate an entire class of bugs where rollback is forgotten on error paths.

**Proposal**: Add `conn.begin()`, `conn.begin_immediate()`, `conn.begin_exclusive()` returning a `Transaction` that rolls back on drop and commits on `.commit()`.

---

### 1.2 Row-to-Struct Mapping

**The problem**: Every query result requires manual positional extraction — `row.get(0)`, `row.get(1)`, etc. Column indices are fragile; reordering a SELECT breaks everything silently.

**What we write today** (introspect.rs — one of many):
```rust
let name: String = row.get(1)?;
let col_type: String = row.get(2)?;
let notnull: i64 = row.get(3)?;
let default_value: Option<String> = row.get(4)?;
let pk: i64 = row.get(5)?;
let hidden: i64 = row.get(6).unwrap_or(0);
```

**What it should look like**:
```rust
#[derive(FromRow)]
struct ColumnXInfo {
    name: String,
    col_type: String,
    notnull: i64,
    default_value: Option<String>,
    pk: i64,
    #[turso(default)]
    hidden: i64,
}

let col: ColumnXInfo = row.into()?;
```

**What rusqlite provides**: Column-by-name access via `row.get::<_, String>("name")?` and community `FromRow` derive macros.

**What SQLx provides**: `#[derive(sqlx::FromRow)]` with automatic column-name mapping and `query_as!` macro.

**Impact**: 50 `row.get(N)` calls. Every one is a positional landmine. Column-name access alone would be a significant improvement; a derive macro would be transformative.

**Proposal (incremental)**:
1. Add `row.get_by_name::<T>("column_name")` — minimal, high-value
2. Add `#[derive(turso::FromRow)]` — ideal but more work
3. Add `row.column_count()` and `row.column_name(idx)` — already done in v0.5 per Issue #3142

---

### 1.3 Structured Error Types

**The problem**: `turso::Error` is an opaque type. To determine _why_ a query failed, you must parse the error message string.

**What we write today** (execute.rs):
```rust
fn is_missing_dependency_error(err: &turso::Error) -> bool {
    let lower = err.to_string().to_ascii_lowercase();
    lower.contains("no such table") || lower.contains("no such view")
}
```

**What it should look like**:
```rust
match err {
    turso::Error::NoSuchTable(name) => { /* handle */ }
    turso::Error::NoSuchView(name) => { /* handle */ }
    turso::Error::DatabaseLocked => { /* retry */ }
    turso::Error::ReadOnly => { /* report */ }
    _ => { /* generic */ }
}
```

**What rusqlite provides**: `ErrorCode` enum mapping to SQLite error codes (`SQLITE_BUSY`, `SQLITE_READONLY`, `SQLITE_CONSTRAINT`, etc.) accessible via `err.sqlite_error_code()`.

**Impact**: String-matching errors is fragile and locale-dependent. Any error message change in a turso release silently breaks our detection. Structured errors would make consumer code robust.

**Proposal**: Expose at minimum the SQLite-compatible error code as an enum variant or `.error_code()` method on `turso::Error`.

---

### 1.4 Typed PRAGMA Interface

**The problem**: PRAGMAs are executed as raw `format!` strings. No type safety, no autocomplete, easy to typo.

**What we write today** (scattered across 3 files):
```rust
conn.execute(&format!("PRAGMA busy_timeout = {ms}"), ()).await?;
conn.execute("PRAGMA defer_foreign_keys = ON", ()).await?;
let mut rows = conn.query("PRAGMA schema_version", ()).await?;
let mut rows = conn.query(&format!("PRAGMA table_xinfo('{}')", name.replace('\'', "''")), ()).await?;
```

**What it should look like**:
```rust
conn.pragma_busy_timeout(Duration::from_secs(5)).await?;
conn.pragma_defer_foreign_keys(true).await?;
let version: i64 = conn.pragma_schema_version().await?;
let columns: Vec<ColumnXInfo> = conn.pragma_table_xinfo("users").await?;
```

**What rusqlite provides**: `pragma_update()`, `pragma_query()`, `pragma_query_value()`, and dedicated methods like `busy_timeout()`.

**Impact**: 18 PRAGMA calls in our codebase, each constructing SQL by hand. Type-safe PRAGMA methods would prevent injection, improve readability, and enable IDE autocomplete.

**Proposal**: Add typed methods for the 10 most common PRAGMAs: `busy_timeout`, `foreign_keys`, `defer_foreign_keys`, `foreign_key_check`, `schema_version`, `query_only`, `table_info`, `table_xinfo`, `index_list`, `index_info`, `foreign_key_list`.

---

## Tier 2: Missing Primitives

Features that are standard in database drivers but absent from turso.

### 2.1 Connection Pooling

**Status**: Missing. Explicitly requested in [Issue #5721](https://github.com/tursodatabase/turso/issues/5721).

SQLite is single-writer, so a pool needs special handling — typically 1 writer + N readers. SQLx solves this with `SqlitePoolOptions::max_connections()`. turso consumers currently must build their own pooling or manage a single connection.

**Proposal**: Add a `Pool` type with writer/reader separation, configurable pool size, idle timeout, and health checking.

---

### 2.2 Prepared Statement Caching

**Status**: Limited. `prepare()` exists but there's no automatic cache.

**What rusqlite provides**: `prepare_cached()` — returns a cached statement if the SQL matches, or prepares and caches a new one. Statement cache size is configurable.

**Proposal**: Add `conn.prepare_cached(sql)` that returns statements from a per-connection LRU cache.

---

### 2.3 Hooks (Update / Commit / Rollback)

**Status**: Missing. [Issue #1418](https://github.com/tursodatabase/libsql/issues/1418) open since 2024.

**What rusqlite provides**:
```rust
conn.update_hook(Some(|action, db_name, table, rowid| {
    println!("Changed: {table} row {rowid}");
}));
conn.commit_hook(Some(|| { /* ... */ }));
conn.rollback_hook(Some(|| { /* ... */ }));
```

These enable reactive queries, change data capture, and cache invalidation — patterns that are impossible without driver support.

**Proposal**: Add `conn.on_update(callback)`, `conn.on_commit(callback)`, `conn.on_rollback(callback)`.

---

### 2.4 Backup API

**Status**: Missing.

**What rusqlite provides**: Incremental online backup via `Backup::new(&src, &mut dst)` with progress callbacks and step-based execution. Essential for point-in-time recovery.

**Proposal**: Add `turso::backup(source, dest)` with async progress reporting.

---

### 2.5 Blob I/O

**Status**: No `Read`/`Write` interface for BLOB columns.

**What rusqlite provides**: `Blob` type implementing `std::io::Read`, `std::io::Write`, and `std::io::Seek` for incremental BLOB I/O without loading the entire value into memory.

**Proposal**: Add `conn.blob_open(table, column, rowid)` returning an async reader/writer.

---

### 2.6 Custom Functions and Collations

**Status**: Available via the separate `turso_ext` crate, but not in the main `turso` crate.

Having to depend on a second crate for custom scalar functions is friction. rusqlite integrates this behind a feature flag.

**Proposal**: Add a `functions` feature to the `turso` crate with `conn.create_scalar_function()` and `conn.create_collation()`.

---

### 2.7 Feature Detection API

**Status**: Missing. turso-converge implements 6 runtime probe functions that create and drop temporary tables to test if features like FTS, vector columns, materialized views, WITHOUT ROWID, GENERATED columns, and triggers are supported.

**What it should look like**:
```rust
let caps = conn.capabilities().await?;
if caps.supports_fts { /* ... */ }
if caps.supports_triggers { /* ... */ }
```

**Proposal**: Add `conn.capabilities()` or `db.capabilities()` returning a struct with boolean fields for each feature.

---

## Tier 3: Ecosystem Integration

These would expand turso's reach beyond its current niche.

### 3.1 ORM Compatibility Layer

No major Rust ORM supports turso natively:
- **SeaORM**: [Issue #2763](https://github.com/SeaQL/sea-orm/issues/2763) — open, requesting turso backend
- **Diesel**: No turso backend
- **SQLx**: [Issue #2674](https://github.com/launchbadge/sqlx/issues/2674) — open, requesting turso support

The `rbdc-turso` crate exists as a Rbatis driver, but adoption is minimal (23 reverse dependencies on the turso crate total).

**Proposal**: Implement the SQLx `Database` + `Connection` + `Executor` traits for turso, which would unlock the entire SQLx ecosystem (compile-time checking, migrations, pool) without reimplementing it.

---

### 3.2 WASM Support

**Status**: Problematic. [Issue #5049](https://github.com/tursodatabase/turso/issues/5049) — partially addressed but not guaranteed.

Rust-to-WASM is a major deployment target (edge functions, browser apps). Full WASM compilation support would open turso to Cloudflare Workers, Deno Deploy, and browser-based applications.

---

### 3.3 Connection Abstraction Trait

**Status**: Missing. turso-converge defines its own `ConnectionLike` trait.

A standard `trait Connection` in the turso crate would let libraries like turso-converge accept any connection type (pooled, raw, wrapped) without defining their own abstraction.

**Proposal**: Add a `turso::ConnectionExt` or `turso::Executor` trait that `turso::Connection` implements, so downstream crates can be generic over it.

---

## Tier 4: Performance & Observability

### 4.1 Performance Parity with SQLite

Benchmarks show turso can be significantly slower than SQLite/rusqlite for local workloads:
- Reports of 200x slowdown vs rusqlite in local mode ([libsql Issue #1458](https://github.com/tursodatabase/libsql/issues/1458))
- Missing `PRAGMA temp_store=MEMORY` optimization (acknowledged by Turso team)

**Specific gaps**:
- No `PRAGMA temp_store` builder option
- No `PRAGMA mmap_size` builder option
- No `PRAGMA cache_size` builder option
- Statement caching missing (see 2.2)

**Proposal**: Add performance-relevant PRAGMAs as `Builder` options with sensible defaults for local workloads.

---

### 4.2 Trace / Profile Callbacks

**Status**: Missing.

**What rusqlite provides**: `conn.trace(callback)` and `conn.profile(callback)` for SQL tracing and query timing.

turso-converge uses the `tracing` crate for observability but cannot instrument the driver layer itself. Profile callbacks would enable query-level performance monitoring.

**Proposal**: Add `conn.on_trace(callback)` and `conn.on_profile(callback)`.

---

### 4.3 Panic-Free Guarantees

Multiple turso GitHub issues report panics instead of errors:
- [#5281](https://github.com/tursodatabase/turso/issues/5281): Panic on unresolved table reference in UPSERT
- [#5227](https://github.com/tursodatabase/turso/issues/5227): Panic on duplicate ORDER BY expressions
- [#5124](https://github.com/tursodatabase/turso/issues/5124): `PRAGMA integrity_check` panics

A database driver should never panic. Every error path should return `Result`.

**Proposal**: Audit all `unwrap()` and `panic!()` paths and convert to `Result` returns. Consider adding `#![deny(clippy::unwrap_used)]` to the crate.

---

## Tier 5: Drastic Structural Changes

These are larger architectural shifts that could fundamentally improve the developer experience.

### 5.1 Implement the SQLx Trait Family

Rather than building a parallel ecosystem, turso could implement SQLx's trait family (`Database`, `Connection`, `Executor`, `Row`, `Column`, `TypeInfo`). This would:

- Unlock `sqlx::query!()` compile-time checking for turso
- Unlock SQLx's migration system
- Unlock SQLx's connection pooling
- Unlock every SQLx-based ORM (SeaORM, etc.)
- Maintain turso's unique features (FTS, vectors, replicas) as extensions

This is the single highest-leverage change possible. Instead of building pool, migrations, compile-time checking, and ORM support from scratch, implementing ~6 traits gives turso access to the entire SQLx ecosystem.

**Effort**: High (months of work). **Impact**: Transformative.

---

### 5.2 Split Sync and Async APIs

turso is async-only, requiring `tokio` for everything — even single-query CLI tools. rusqlite thrives partly because synchronous SQLite access is often the right choice for:
- CLI tools
- Build scripts
- Tests (tokio overhead is unnecessary)
- Embedded systems without an async runtime

**Proposal**: Offer `turso::sync::Connection` alongside `turso::Connection` (async). The `sync` feature already exists in turso's Cargo features — expand it to a fully synchronous API.

---

### 5.3 Schema Introspection as a Driver Feature

turso-converge reimplements significant schema introspection logic:
- Reading `sqlite_schema` and parsing types
- Running `PRAGMA table_xinfo` / `foreign_key_list` / `index_list` / `index_info`
- Parsing COLLATE, GENERATED, STRICT, WITHOUT ROWID from DDL
- Extracting CHECK constraints from DDL

This is generic functionality that every schema tool needs. If turso provided `conn.schema()` returning structured table/column/index metadata, multiple projects wouldn't need to reimplement it.

**Proposal**: Add `conn.schema_snapshot()` returning structured `Table`, `Column`, `Index`, `View`, `Trigger` types — essentially a driver-level version of what turso-converge's `SchemaSnapshot::from_connection()` does.

---

### 5.4 First-Class Migration Primitives

Instead of leaving schema management entirely to external tools, turso could provide low-level primitives that tools build on:
- `conn.schema_hash()` — BLAKE3 hash of the current schema DDL
- `conn.diff_schema(desired_sql)` — structured diff
- `conn.table_rebuild(table, new_ddl)` — safe 12-step rebuild
- `conn.schema_version()` — typed PRAGMA wrapper

These wouldn't be a migration system — they'd be building blocks that make migration systems like turso-converge simpler and more reliable.

---

## Summary Matrix

| Improvement | Effort | Impact | Unblocks |
|---|---|---|---|
| **Transaction API** | Low | High | Every turso consumer |
| **Row-to-struct (`FromRow`)** | Medium | High | Every turso consumer |
| **Structured error types** | Medium | High | Robust error handling |
| **Typed PRAGMA methods** | Low | Medium | Cleaner code, fewer bugs |
| **Connection pooling** | Medium | High | Web applications |
| **Statement caching** | Low | Medium | Performance |
| **Hooks (update/commit/rollback)** | Medium | High | Reactive apps, CDC |
| **Backup API** | Medium | Medium | Data safety |
| **Blob I/O** | Low | Low | Niche use cases |
| **Custom functions in main crate** | Low | Medium | Extension developers |
| **Feature detection API** | Low | Medium | turso-converge, migration tools |
| **SQLx trait implementation** | High | Transformative | Entire Rust database ecosystem |
| **Sync API expansion** | Medium | Medium | CLI tools, tests, embedded |
| **Schema introspection API** | Medium | High | Schema tools, migration tools |
| **Panic-free guarantees** | Medium | High | Production reliability |
| **Trace/profile callbacks** | Low | Medium | Observability |
| **WASM support** | High | Medium | Edge deployments |
| **Performance PRAGMA defaults** | Low | Medium | Local-first apps |

---

## References

- turso crate: [crates.io](https://crates.io/crates/turso) · [GitHub](https://github.com/tursodatabase/turso)
- Connection pool request: [Issue #5721](https://github.com/tursodatabase/turso/issues/5721)
- Hooks request: [Issue #1418](https://github.com/tursodatabase/libsql/issues/1418)
- WASM request: [Issue #5049](https://github.com/tursodatabase/turso/issues/5049)
- SQLx integration request: [Issue #2674](https://github.com/launchbadge/sqlx/issues/2674)
- SeaORM turso backend request: [SeaQL/sea-orm#2763](https://github.com/SeaQL/sea-orm/issues/2763)
- Panic issues: [#5281](https://github.com/tursodatabase/turso/issues/5281), [#5227](https://github.com/tursodatabase/turso/issues/5227), [#5124](https://github.com/tursodatabase/turso/issues/5124)
- Performance reports: [libsql#1458](https://github.com/tursodatabase/libsql/issues/1458)
- rusqlite: [docs.rs](https://docs.rs/rusqlite/latest/rusqlite/)
- SQLx: [docs.rs](https://docs.rs/sqlx/latest/sqlx/)
