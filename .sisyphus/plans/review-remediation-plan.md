# turso-migrate Review Remediation Plan

**Goal:** Resolve remaining production-readiness issues from `REVIEW.md` that are still relevant to the current codebase, with safety-first behavior and regression-proof tests.

**Architecture:** Keep current lease/crash-recovery model, but tighten controlled-error cleanup and state updates. Remove avoidable async runtime blocking. Harden SQL statement classification and CLI behavior parity for trigger support.

**Tech Stack:** Rust, Tokio, Turso, existing test suite (`cargo test`).

---

## Scope (Current Codebase)

### In scope
1. Controlled-error cleanup for migration state markers.
2. Schema version increment hardening (`unwrap_or` removal + overflow guard).
3. Async-safe file reads/writes in async migration paths.
4. Stronger CREATE view/trigger classification in execution phase partitioning.
5. CLI trigger capability parity with library behavior.
6. Documentation refresh in `REVIEW.md` and `README.md`.

### Out of scope
- Large refactors of diff/plan model.
- API-breaking policy changes to `converge()` defaults.

---

## Task Breakdown

### Task 1: Controlled-error lease/state cleanup

**Files:**
- Modify: `src/converge.rs`
- Test: `tests/converge.rs`

**Change:**
- After acquiring a lease and running slow path, if result is `Err`, perform best-effort cleanup of `migration_phase` and `migration_in_progress` only when lease owner still matches current lease id.
- Keep crash semantics intact (cleanup only on handled error paths in current process).

**QA:**
- Add/extend failpoint test to assert `migration_in_progress` and `migration_phase` are cleared after injected failure (controlled error).
- Existing crash-recovery behavior test remains passing.

---

### Task 2: Schema version hardening

**Files:**
- Modify: `src/converge.rs`
- Test: `tests/converge.rs`

**Change:**
- In `increment_schema_version`, replace `schema_version(conn).await.unwrap_or(0)` with explicit `?` propagation.
- Use `checked_add(1)` and return a schema error on overflow.

**QA:**
- Add test that seeds `schema_version` to `u32::MAX` and verifies convergence fails with overflow error.
- Existing version increment tests still pass.

---

### Task 3: Async-safe file I/O in async migration paths

**Files:**
- Modify: `src/converge.rs`
- Test: `tests/new_api.rs`, `tests/converge.rs`

**Change:**
- `converge_from_path`: switch to `tokio::fs::read_to_string(...).await` with same `MigrateError::Io` mapping.
- `write_schema_backup`: convert to async-safe path (`tokio::fs::{metadata,create_dir_all,write}`) and await at call site.

**QA:**
- Existing `converge_from_path` success/missing-file tests remain passing.
- Existing backup creation test remains passing.

---

### Task 4: Harden create-view/trigger classifier

**Files:**
- Modify: `src/execute.rs`
- Test: `tests/execute.rs`

**Change:**
- Replace plain `starts_with("create view")`/`starts_with("create trigger")` checks with a small normalized classifier that handles:
  - leading whitespace/comments
  - `CREATE TEMP VIEW`
  - `CREATE TEMPORARY VIEW`
  - `CREATE [TEMP|TEMPORARY] TRIGGER`
  - optional `IF NOT EXISTS`

**QA:**
- Add tests for classifier behavior through execution planning path or unit tests in module.
- Ensure existing execution tests pass unchanged.

---

### Task 5: CLI trigger parity

**Files:**
- Modify: `src/bin/turso-migrate.rs`
- Test: `tests/new_api.rs` (if needed)

**Change:**
- In `open_local_connection`, add `.experimental_triggers(true)` for parity with in-memory schema validation/introspection support.

**QA:**
- Existing CLI-oriented integration tests remain passing.

---

### Task 6: Documentation updates

**Files:**
- Modify: `REVIEW.md`
- Modify: `README.md`

**Change:**
- Mark newly fixed items as resolved with specific file references.
- Update test-count summary and mention trigger capability requirement consistently (index/materialized/triggers).
- Keep unresolved items explicitly listed with rationale.

**QA:**
- Doc content matches implemented behavior and current test totals.

---

## Verification Plan

1. `cargo fmt`
2. `cargo test --jobs 1`
3. `lsp_diagnostics` on modified files (severity: error)
4. Confirm git clean state except intentional changes

Success criteria:
- All tests pass
- No LSP errors in changed files
- Docs reflect implemented behavior

---

## Branch Cleanup Plan

After implementation/verification and before final push:
- Identify branches merged into `main`.
- Delete stale local branches (excluding `main`).
- Delete stale remote branches merged into `origin/main` (excluding `origin/main`).
- Verify only active branches remain.
