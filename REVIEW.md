# turso-migrate Production Readiness Review

**Date**: 2026-03-17
**Reviewer**: Automated multi-agent review (Sisyphus + Oracle)
**Codebase**: turso-migrate v0.1.0 @ main
**Test Baseline**: 61 tests, all passing

---

## Executive Summary

turso-migrate is a well-architected declarative schema convergence library with a solid core algorithm. The BLAKE3 fast-path, 3-phase execution model, and 12-step ALTER TABLE procedure are correctly implemented. However, several gaps exist that should be addressed before production hardening.

**Severity Legend**: P0 = must fix before production, P1 = should fix soon, P2 = improvement, P3 = nice to have

---

## Findings

### P0: Critical Issues

#### 1. Error paths leave `migration_in_progress` flag stuck permanently

**File**: `src/converge.rs:21-40`
**Issue**: The `migration_in_progress` flag is set to `"1"` on line 21, but is only cleared on the **success path** (line 34). Any error after line 21 - including invalid schema SQL in `from_schema_sql()`, introspection failure, or plan execution error - leaves the flag permanently stuck at `"1"`.

While a stuck flag doesn't cause incorrect behavior (it just forces the slow path on every subsequent call), it means:
- Every future `converge()` call does full introspection instead of fast-pathing
- The flag stays stuck until someone manually clears it
- There's no timeout/TTL-based recovery

**Impact**: Performance degradation after any migration error; requires manual intervention
**Fix**: Use RAII guard pattern or ensure `migration_in_progress` is cleared in all error paths. Move flag-set after `from_schema_sql()` succeeds to avoid wedging on invalid input.

#### 2. `from_schema_sql` missing `experimental_triggers` flag (FIXED)

**File**: `src/introspect.rs:179-183`
**Issue**: `SchemaSnapshot::from_schema_sql()` creates an in-memory DB without `.experimental_triggers(true)`. Any schema SQL containing `CREATE TRIGGER` will fail with "CREATE TRIGGER is an experimental feature". Triggers are listed as supported in the README but cannot work through `converge()`.

**Impact**: Triggers completely broken via `converge()` API
**Status**: FIXED - added `.experimental_triggers(true)` to the builder

#### 3. `Migrator::migrate()` bypasses crash recovery and hash tracking

**File**: `src/migrator.rs:37-55`
**Issue**: `Migrator::migrate()` does NOT:
- Set `migration_in_progress = "1"` before running DDL
- Clear `migration_in_progress` after completion
- Update `schema_hash` after successful migration

This means:
- If `Migrator::migrate()` crashes mid-execution, the next `converge()` call has no way to know a migration was interrupted
- After `Migrator::migrate()` succeeds, the next `converge()` call with the same schema will run the **full slow-path** again (hash mismatch), finding an empty diff but wasting time on introspection

**Impact**: Crash recovery gap + performance degradation on subsequent calls
**Fix**: Add hash/progress tracking to `Migrator::migrate()`, or document that `Migrator` is for one-shot use only

#### 2. Temp table collision on crash recovery

**File**: `src/plan.rs:138`
**Issue**: Table rebuilds create `_converge_new_{table}` but never check if this table already exists. If a previous migration crashed after creating the temp table but before the rename, the next attempt will fail with "table already exists".

**Impact**: Crash recovery path may fail instead of recovering
**Fix**: Add `DROP TABLE IF EXISTS _converge_new_{table}` before the CREATE

#### 4. `build_copy_data_stmt` generates invalid SQL with no shared columns (FIXED)

**File**: `src/plan.rs:329-358`
**Issue**: When a table rebuild has zero shared columns between old and new schema, the function generated `INSERT INTO "temp" () SELECT  FROM "old"` which is syntactically invalid SQL, crashing the migration.

**Impact**: Any table rebuild where ALL columns change (complete schema redesign) crashes
**Status**: FIXED - function now returns `Option<String>` and caller skips INSERT when None

#### 5. `schema_version()` fails if table doesn't exist

**File**: `src/converge.rs:55-66`
**Issue**: `schema_version()` queries `SELECT version FROM schema_version LIMIT 1`. If the user's schema doesn't include a `schema_version` table (it's not required by the library), this query returns a Turso error, not 0.

Called from `increment_schema_version()` with `unwrap_or(0)` which masks the real error, but the subsequent `DELETE FROM schema_version` and `INSERT INTO schema_version` will also fail.

**Impact**: `converge()` will fail on any schema that doesn't include a `schema_version` table
**Fix**: Either auto-create `schema_version` in `bootstrap_schema_meta()`, or handle the "table doesn't exist" case gracefully

#### 4. BLAKE3 fast-path can skip real needed work (out-of-band changes)

**File**: `src/converge.rs:17-19`
**Issue**: If the database schema is modified out-of-band (manual SQL, another tool, direct DDL), but `schema_sql` hasn't changed, the BLAKE3 hash matches and `converge()` returns `Ok(())` without introspecting. The database silently drifts from the desired schema.

**Impact**: For a "convergence" tool, this is a correctness landmine. The database may be missing tables/indexes that the application expects.
**Mitigation**: Document this clearly as a design constraint (already partially done), or add optional DB-side fingerprint check (e.g., hash of `sqlite_master` rows)

---

### P1: Should Fix

#### 5. Blocking I/O in async context

**File**: `src/converge.rs:48`
**Issue**: `std::fs::read_to_string(path)` is blocking I/O inside an async function. On a multi-threaded Tokio runtime this blocks a worker thread; on a single-threaded runtime it blocks the entire executor.

**Fix**: Use `tokio::fs::read_to_string` instead

#### 6. `ForeignKeyViolation` error variant is dead code

**File**: `src/error.rs:13`
**Issue**: `MigrateError::ForeignKeyViolation` is declared but never constructed anywhere in the codebase. The README mentions "No FK pragma handling" and the migration process skips FK checks (steps 1, 10, 12 of the 12-step procedure).

**Impact**: Dead code. Either implement FK checking or remove the variant.
**Fix**: Remove unused variant, or implement optional FK validation

#### 7. `allow_deletions` only protects tables

**File**: `src/migrator.rs:42-48`
**Issue**: `Migrator::allow_deletions(false)` only prevents dropping tables. It does NOT prevent dropping indexes, views, or triggers that exist in the DB but not in the desired schema.

**Impact**: Users expecting `allow_deletions(false)` to be a comprehensive safety net will be surprised when views/triggers are dropped.
**Fix**: Either extend protection to all object types, or rename to `allow_table_deletions` for clarity

#### 8. `sanitize_view_sql` is incomplete and fragile

**File**: `src/plan.rs:418-427`
**Issue**: Only handles space-before-paren for COUNT, AVG, MIN, MAX. Missing: SUM, TOTAL, GROUP_CONCAT, COALESCE, IFNULL, etc. Also only handles specific casing patterns (uppercase and lowercase, but not mixed case).

**Impact**: View recreation may fail if SQLite normalizes function call spacing differently
**Fix**: Use a regex or more comprehensive normalization approach

---

### P2: Improvements

#### 9. `referenced_tables` parser doesn't handle edge cases

**File**: `src/plan.rs:261-312`
**Issue**: Byte-level parser for `REFERENCES table_name` doesn't handle:
- Backtick-quoted identifiers: `` REFERENCES `my_table` ``
- String literals containing "references"
- SQL comments containing "references"
- The word appearing as part of a column name (e.g., `cross_references`)

**Impact**: Low in practice (FK references are typically simple), but could cause incorrect topological ordering
**Mitigation**: Document limitation clearly (already done in README)

#### 10. `is_safe_identifier` silently drops tables with special chars

**File**: `src/introspect.rs:270-275`
**Issue**: Rejects any identifier containing characters other than `[a-zA-Z0-9_]`. Tables with hyphens, dots, or any Unicode characters are silently ignored during introspection.

**Impact**: If a user has a table named `my-table` or `user.data`, it will be invisible to the migration engine
**Fix**: Accept quoted identifiers or at minimum log a warning

#### 11. View `is_materialized` detection is fragile

**File**: `src/introspect.rs:128`
**Issue**: Detects materialized views by `sql.to_lowercase().contains("materialized")`. A regular view with "materialized" in a string literal or comment would be misclassified.

**Impact**: Low probability but could cause incorrect view handling
**Fix**: Check for `"create materialized view"` prefix specifically

#### 12. `normalize_sql` for diff comparison is too aggressive

**File**: `src/diff.rs:36-41`
**Issue**: Lowercases entire SQL and collapses all whitespace. This means `INTEGER` vs `INT` or any casing differences trigger unnecessary rebuilds. While rebuilds are idempotent, they're expensive for large tables.

**Impact**: Performance - unnecessary rebuilds on cosmetic SQL differences
**Mitigation**: Already documented in README as known limitation

#### 13. No logging/tracing during migration

**File**: Various
**Issue**: The convergence pipeline has minimal tracing. Only `bridge_legacy` and `Migrator::migrate` (for allow_deletions warning) log anything. The actual DDL execution, plan generation, and diff computation are silent.

**Impact**: Debugging production migration issues requires code-level debugging
**Fix**: Add `tracing::info!` for plan summary, `tracing::debug!` for individual statements

---

### P3: Nice to Have

#### 14. `data_migrations.rs` is essentially dead code
- `DATA_VERSION` is 0, `converge_data()` is a no-op
- Framework exists but does nothing

#### 15. Duplicated test helpers across test files
- `empty_db()`, `test_schema()`, `get_meta()` are copy-pasted across 7 test files
- Should extract to a shared `tests/common/mod.rs`

#### 16. Inconsistent test runtime annotations
- Some tests use `#[tokio::test]`, others `#[tokio::test(flavor = "multi_thread")]`
- Should standardize on `multi_thread` since Turso requires it

#### 17. No doc comments on public API
- Public functions and types lack `///` documentation
- `README.md` covers usage well but rustdoc is empty

---

## Test Coverage Gaps

### Missing Test Categories

| Category | Current Coverage | Gap |
|----------|-----------------|-----|
| **Trigger diff/execution** | 0 tests | No tests for trigger create/drop/change |
| **View dependency cascade** | 1 test (materialized view rebuild) | No test for regular view cascade on table rebuild |
| **`referenced_tables` parser** | 0 direct tests | Edge cases: quoted names, self-references, circular FKs |
| **`rewrite_create_table_name`** | 0 direct tests | Quoted table names, IF NOT EXISTS |
| **`view_depends_on_table`** | 0 direct tests | False positive/negative scenarios |
| **`is_safe_identifier`** | 0 tests | Boundary cases |
| **`quote_ident`** | 0 tests | Double-quote escaping |
| **`build_add_column_stmt`** | 0 direct tests | Output format verification |
| **`build_copy_data_stmt` edge cases** | 0 tests | No shared columns, all columns shared |
| **Concurrent migration** | 0 tests | Two connections converging simultaneously |
| **Schema without `schema_version` table** | 0 tests | P0 bug #3 |
| **`Migrator` + `converge()` interaction** | 0 tests | Hash state after `Migrator::migrate()` |
| **Multiple table rebuilds in one migration** | 0 tests | FK ordering during multi-rebuild |
| **Table rebuild with no shared columns** | 0 tests | Complete schema change for existing table |
| **Drop + recreate in same migration** | 0 tests | Table renamed (treated as drop+create) |
| **`converge_from_path` with unicode** | 0 tests | Non-ASCII path characters |
| **Empty diff produces empty plan** | 1 test | But doesn't verify plan fields individually |
| **`sanitize_view_sql` edge cases** | 0 tests | Mixed case, other functions |
| **Whitespace-only schema SQL** | 0 tests | `"   \n  "` should fail same as empty |
| **Very large number of tables** | 0 tests | Performance/correctness at scale |

---

## Recommendations

### Immediate (before any production traffic)
1. Fix P0 #1 (migration_in_progress stuck on error paths)
2. Fix P0 #2 (Migrator hash/crash-recovery bypass)
3. Fix P0 #3 (temp table collision on crash recovery)
4. Fix P0 #4 (schema_version table absence)
5. Add tests for trigger handling (diff + execution)
6. Add tests for crash recovery edge cases
7. Add tests for Migrator/converge interaction

### Short-term
8. Fix P1 #5-8
9. Add comprehensive parser edge case tests
10. Add migration tracing/logging
11. Extract shared test helpers

### Medium-term
12. Address P2 improvements
13. Add concurrent migration tests
14. Add large-schema performance benchmarks
15. Consider adding `PRAGMA foreign_key_check` as optional post-migration step
16. Consider DB-side fingerprint to detect out-of-band schema changes (P0 #5)

---

## Second Pass Review (Oracle)

### Verified Safe

| Concern | Status | Analysis |
|---------|--------|----------|
| **Infinite loops in `to_sql()`/`order_new_tables()`** | SAFE | Both loops have `if !progressed { break; }` that drains all remaining items. Circular FKs are handled correctly. |
| **SQL injection via `quote_ident()`** | SAFE | Double-quote escaping (`"` → `""`) is the standard SQLite identifier defense. Sufficient as long as output is used only in identifier context. |
| **Partial migration retry correctness** | SAFE (accidental) | If phase2 (views/triggers) fails after phase1+FTS succeed, `migration_in_progress` stays stuck → next `converge()` forces slow path → introspects → finds missing views/triggers → recreates them. The stuck flag bug is actually accidental crash recovery for this scenario. |
| **Table rebuild data safety** | SAFE | Uses plain `INSERT INTO ... SELECT`, not `INSERT OR REPLACE`. No silent data loss from uniqueness constraint changes. |
| **Integer overflow (`u32` schema version)** | NEGLIGIBLE RISK | 4 billion migrations is beyond any plausible deployment. Could harden with `checked_add` but not a production risk. |

### New Findings

#### `is_create_view_or_trigger` false negatives (P2)

**File**: `src/execute.rs:80-85`
**Issue**: Checks `starts_with("create view")` etc. Leading whitespace is handled (`trim_start`), but SQL comments, `CREATE TEMP VIEW`, or `CREATE VIEW IF NOT EXISTS` could be mis-partitioned into phase1 instead of phase2.
**Risk**: Low - SQL comes from `sqlite_schema` introspection which has consistent formatting.
**Fix**: Replace with a more robust token scanner if untrusted SQL is ever supported.

#### Non-transactional FTS partial failure (P2)

**Issue**: If FTS index creation fails mid-batch in `run_non_transactional`, some FTS indexes exist and some don't. The next diff correctly detects missing ones and creates them, so retries are safe. However, partial FTS *content* (auxiliary tables) may not be detected by schema diff.
**Risk**: Low - FTS index creation is per-statement atomic in Turso.

---

## Bugs Fixed During This Review

| Bug | Severity | Fix |
|-----|----------|-----|
| `from_schema_sql()` missing `.experimental_triggers(true)` | P0 | Added flag to builder - triggers now work through `converge()` |
| `build_copy_data_stmt` invalid SQL with no shared columns | P0 | Returns `Option<String>`, caller skips INSERT when `None` |

## Test Summary

| Category | Before | After | Delta |
|----------|--------|-------|-------|
| Unit tests (lib) | 0 | 54 | +54 |
| Bridge tests | 4 | 4 | 0 |
| Converge tests | 9 | 9 | 0 |
| Data migration tests | 3 | 3 | 0 |
| Diff tests | 12 | 12 | 0 |
| Edge case tests | 0 | 16 | +16 |
| Execute tests | 11 | 11 | 0 |
| Introspect tests | 8 | 8 | 0 |
| Migrator tests | 4 | 4 | 0 |
| New API tests | 10 | 10 | 0 |
| Trigger tests | 0 | 8 | +8 |
| **Total** | **61** | **139** | **+78** |
