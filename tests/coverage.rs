//! Coverage gap tests — behaviors that must survive optimization.
//!
//! Each test targets a specific untested code path identified during
//! the coverage audit. Grouped by subsystem.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use turso_converge::diff::normalize_for_hash;
use turso_converge::execute::execute_plan;
use turso_converge::plan::generate_plan;
use turso_converge::{
    CIString, Capabilities, ConnectionLike, ConvergeMode, ConvergeOptions, ConvergePolicy,
    DataMigration, DestructiveChangeSet, Failpoint, MigrateError, SchemaSnapshot, compute_diff,
    converge, converge_like_with_options, converge_with_options, schema_version,
};

// ── ConvergePolicy edge cases ──────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn policy_max_tables_affected_blocks_when_exceeded() {
    let (_db, conn) = common::empty_db().await;
    let options = ConvergeOptions {
        policy: ConvergePolicy {
            max_tables_affected: Some(1),
            ..ConvergePolicy::permissive()
        },
        ..Default::default()
    };

    let err = converge_with_options(&conn, common::test_schema(), &options)
        .await
        .unwrap_err();
    match err {
        MigrateError::PolicyViolation { message, .. } => {
            assert!(
                message.to_lowercase().contains("table") && message.contains("max_tables_affected"),
                "Expected tables-affected message, got: {message}"
            );
        }
        other => panic!("expected PolicyViolation, got: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_max_tables_affected_allows_when_within_limit() {
    let (_db, conn) = common::empty_db().await;
    let schema = "CREATE TABLE only_one (id TEXT PRIMARY KEY);";
    let options = ConvergeOptions {
        policy: ConvergePolicy {
            max_tables_affected: Some(5),
            ..ConvergePolicy::permissive()
        },
        ..Default::default()
    };

    let report = converge_with_options(&conn, schema, &options)
        .await
        .unwrap();
    assert_eq!(report.tables_created, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_blocks_column_drops() {
    let (_db, conn) = common::empty_db().await;
    let schema_v1 = "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT, extra TEXT);";
    converge(&conn, schema_v1).await.unwrap();

    let schema_v2 = "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT);";
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    let options = ConvergeOptions {
        policy: ConvergePolicy {
            allow_column_drops: false,
            ..ConvergePolicy::permissive()
        },
        ..Default::default()
    };
    let err = converge_with_options(&conn, schema_v2, &options)
        .await
        .unwrap_err();
    match err {
        MigrateError::PolicyViolation {
            blocked_operations, ..
        } => {
            assert!(
                blocked_operations
                    .iter()
                    .any(|op| op.contains("DROP COLUMN")),
                "Should mention DROP COLUMN, got: {blocked_operations:?}"
            );
        }
        other => panic!("expected PolicyViolation, got: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_blocks_table_rebuilds() {
    let (_db, conn) = common::empty_db().await;
    let schema_v1 = "CREATE TABLE foo (id TEXT PRIMARY KEY, val TEXT);";
    converge(&conn, schema_v1).await.unwrap();

    let schema_v2 = "CREATE TABLE foo (id TEXT PRIMARY KEY, val INTEGER);";
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    let options = ConvergeOptions {
        policy: ConvergePolicy {
            allow_table_rebuilds: false,
            ..ConvergePolicy::permissive()
        },
        ..Default::default()
    };
    let err = converge_with_options(&conn, schema_v2, &options)
        .await
        .unwrap_err();
    match err {
        MigrateError::PolicyViolation {
            blocked_operations, ..
        } => {
            assert!(
                blocked_operations.iter().any(|op| op.contains("REBUILD")),
                "Should mention REBUILD, got: {blocked_operations:?}"
            );
        }
        other => panic!("expected PolicyViolation, got: {other:?}"),
    }
}

// ── DataMigration edge cases ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn data_migration_empty_id_errors() {
    let (_db, conn) = common::empty_db().await;
    let schema = "CREATE TABLE users (id TEXT PRIMARY KEY);";
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        data_migrations: vec![DataMigration {
            id: "".to_string(),
            statements: vec!["INSERT INTO users VALUES ('x')".to_string()],
        }],
        ..Default::default()
    };

    let err = converge_with_options(&conn, schema, &options)
        .await
        .unwrap_err();
    match err {
        MigrateError::Schema(msg) => {
            assert!(
                msg.to_lowercase().contains("empty") || msg.to_lowercase().contains("id"),
                "Expected empty-ID error, got: {msg}"
            );
        }
        other => panic!("expected Schema error for empty ID, got: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn data_migration_statement_failure_rolls_back() {
    let (_db, conn) = common::empty_db().await;
    let schema = "CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT NOT NULL);";
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        data_migrations: vec![DataMigration {
            id: "bad-migration".to_string(),
            statements: vec!["INSERT INTO users (id) VALUES ('no-name')".to_string()],
        }],
        ..Default::default()
    };

    let err = converge_with_options(&conn, schema, &options)
        .await
        .unwrap_err();
    assert!(
        matches!(err, MigrateError::Statement { .. }),
        "expected Statement error, got: {err:?}"
    );

    let marker = common::get_meta(&conn, "data_migration:bad-migration").await;
    assert!(
        marker.is_none(),
        "Failed data migration should not be recorded"
    );
}

// ── Failpoint::BeforeIntrospect ────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn failpoint_before_introspect_aborts_cleanly() {
    let (_db, conn) = common::empty_db().await;
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        failpoint: Some(Failpoint::BeforeIntrospect),
        ..Default::default()
    };

    let err = converge_with_options(&conn, common::test_schema(), &options)
        .await
        .unwrap_err();
    assert!(
        matches!(err, MigrateError::InjectedFailure { ref failpoint } if failpoint == "before_introspect"),
        "got: {err:?}"
    );

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(snap.tables.is_empty(), "No tables should be created");
}

// ── AUTOINCREMENT preservation ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn autoincrement_rebuild_plan_includes_sequence_save_restore() {
    let (_db, conn) = common::empty_db().await;
    conn.execute(
        "CREATE TABLE items (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, old_col TEXT)",
        (),
    )
    .await
    .unwrap();
    conn.execute("CREATE INDEX idx_items_old ON items(old_col)", ())
        .await
        .unwrap();

    let desired_sql = "CREATE TABLE items (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);";
    let desired = SchemaSnapshot::from_schema_sql(desired_sql).await.unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();

    let actual_items = actual.get_table("items").unwrap();
    assert!(
        actual_items.has_autoincrement,
        "Should detect AUTOINCREMENT on actual table"
    );

    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"items".to_string()),
        "Indexed column removal should trigger rebuild"
    );

    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    let all_sql = plan.transactional_stmts.join("\n").to_lowercase();
    assert!(
        all_sql.contains("autoincrement_seq_items"),
        "Rebuild plan should save/restore AUTOINCREMENT sequence"
    );
    assert!(
        all_sql.contains("sqlite_sequence"),
        "Rebuild plan should reference sqlite_sequence"
    );
}

// ── COLLATE clause detection ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn collate_change_triggers_rebuild() {
    let (_db, conn) = common::empty_db().await;
    conn.execute(
        "CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT COLLATE NOCASE)",
        (),
    )
    .await
    .unwrap();
    conn.execute("INSERT INTO users VALUES ('1', 'Alice@Example.com')", ())
        .await
        .unwrap();

    let desired_sql = "CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT COLLATE BINARY);";
    let desired = SchemaSnapshot::from_schema_sql(desired_sql).await.unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"users".to_string()),
        "COLLATE change should trigger rebuild: {diff}"
    );

    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    execute_plan(&conn, &plan).await.unwrap();

    let mut rows = conn
        .query("SELECT email FROM users WHERE id = '1'", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let email: String = row.get(0).unwrap();
    assert_eq!(email, "Alice@Example.com");
}

// ── DROP COLUMN execution (integration) ────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn drop_column_execution_removes_column() {
    let (_db, conn) = common::empty_db().await;
    conn.execute(
        "CREATE TABLE foo (id TEXT PRIMARY KEY, keep TEXT, remove_me TEXT)",
        (),
    )
    .await
    .unwrap();
    conn.execute("INSERT INTO foo VALUES ('1', 'kept', 'gone')", ())
        .await
        .unwrap();

    let desired_sql = "CREATE TABLE foo (id TEXT PRIMARY KEY, keep TEXT);";
    let desired = SchemaSnapshot::from_schema_sql(desired_sql).await.unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.columns_to_drop
            .iter()
            .any(|(t, c)| t == "foo" && c == "remove_me"),
        "Should detect column drop: {diff}"
    );
    assert!(
        diff.tables_to_rebuild.is_empty(),
        "Eligible column should use DROP COLUMN, not rebuild"
    );

    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    execute_plan(&conn, &plan).await.unwrap();

    let after = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let foo = after.get_table("foo").unwrap();
    assert!(!foo.columns.iter().any(|c| c.name == "remove_me"));
    assert!(foo.columns.iter().any(|c| c.name == "keep"));

    let mut rows = conn
        .query("SELECT keep FROM foo WHERE id = '1'", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "kept");
}

// ── Migration lease contention ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn migration_busy_when_lease_held() {
    let (_db, conn) = common::empty_db().await;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS _schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        (),
    )
    .await
    .unwrap();
    let future_expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 300;
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('migration_owner', 'other_process_999')",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('migration_lease_until', '{future_expiry}')"
        ),
        (),
    )
    .await
    .unwrap();

    let schema = "CREATE TABLE foo (id TEXT PRIMARY KEY);";
    let err = converge(&conn, schema).await.unwrap_err();
    match err {
        MigrateError::MigrationBusy {
            owner,
            remaining_secs,
        } => {
            assert!(
                owner.contains("other_process"),
                "Should report owner, got: {owner}"
            );
            assert!(remaining_secs > 0, "Should have time remaining");
        }
        other => panic!("expected MigrationBusy, got: {other:?}"),
    }
}

// ── Protected table namespace ──────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn schema_meta_table_never_dropped() {
    let (_db, conn) = common::empty_db().await;
    converge(&conn, common::test_schema()).await.unwrap();

    let minimal = "CREATE TABLE only_one (id TEXT PRIMARY KEY);";
    converge(&conn, minimal).await.unwrap();

    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='_schema_meta'",
            (),
        )
        .await
        .unwrap();
    assert!(
        rows.next().await.unwrap().is_some(),
        "_schema_meta must never be dropped"
    );
}

// ── ConnectionLike wrappers ────────────────────────────────────────

struct TestWrapper<'a> {
    inner: &'a turso::Connection,
}

impl ConnectionLike for TestWrapper<'_> {
    fn as_turso_connection(&self) -> &turso::Connection {
        self.inner
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_like_with_options_works() {
    let (_db, conn) = common::empty_db().await;
    let wrapped = TestWrapper { inner: &conn };
    let schema = "CREATE TABLE items (id TEXT PRIMARY KEY);";

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        dry_run: true,
        ..Default::default()
    };
    let report = converge_like_with_options(&wrapped, schema, &options)
        .await
        .unwrap();
    assert_eq!(report.mode, ConvergeMode::DryRun);
    assert!(!report.plan_sql.is_empty());
}

// ── STRICT and WITHOUT ROWID tables ────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn strict_table_detected_and_preserved() {
    let (_db, conn) = common::empty_db().await;
    let schema = "CREATE TABLE strict_tbl (id TEXT PRIMARY KEY, val INTEGER) STRICT;";
    converge(&conn, schema).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let tbl = snap.get_table("strict_tbl").unwrap();
    assert!(tbl.is_strict, "Should detect STRICT table");

    converge(&conn, schema).await.unwrap();
}

// ── SQL normalization edge cases ───────────────────────────────────

#[test]
fn normalize_preserves_double_quoted_string_case() {
    let sql = r#"CREATE TABLE "MyTable" ("Col" TEXT)"#;
    let normalized = normalize_for_hash(sql);
    assert!(
        normalized.contains(r#""MyTable""#),
        "Double-quoted identifiers should preserve case: {normalized}"
    );
    assert!(
        normalized.contains(r#""Col""#),
        "Double-quoted column name should preserve case: {normalized}"
    );
}

#[test]
fn normalize_collapses_whitespace() {
    let sql = "CREATE   TABLE   foo  (  id   TEXT  )";
    let normalized = normalize_for_hash(sql);
    assert!(
        !normalized.contains("  "),
        "Should collapse multiple spaces: {normalized}"
    );
}

// ── CIString case-insensitive behavior ─────────────────────────────

#[test]
fn cistring_equality_is_case_insensitive() {
    let a = CIString::new("Users");
    let b = CIString::new("users");
    let c = CIString::new("USERS");
    assert_eq!(a, b);
    assert_eq!(b, c);
    assert_eq!(a, c);
}

#[test]
fn cistring_preserves_original_case() {
    let s = CIString::new("MyTable");
    assert_eq!(s.raw(), "MyTable");
    assert_eq!(s.lower(), "mytable");
}

#[test]
fn cistring_btreemap_lookup_is_case_insensitive() {
    let mut map = BTreeMap::new();
    map.insert(CIString::new("Users"), "found");
    assert_eq!(map.get(&CIString::new("users")), Some(&"found"));
    assert_eq!(map.get(&CIString::new("USERS")), Some(&"found"));
}

// ── extract CLI command ────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn dump_output_round_trips_through_converge() {
    let (_db, conn) = common::empty_db().await;
    let schema = "\
        CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT NOT NULL);\n\
        CREATE TABLE posts (id TEXT PRIMARY KEY, user_id TEXT REFERENCES users(id), title TEXT);\n\
        CREATE INDEX idx_posts_user ON posts(user_id);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);";

    converge(&conn, schema).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let dumped = snap.to_sql();

    let (_db2, conn2) = common::empty_db().await;
    converge(&conn2, &dumped).await.unwrap();

    let snap2 = SchemaSnapshot::from_connection(&conn2).await.unwrap();
    assert!(snap2.has_table("users"));
    assert!(snap2.has_table("posts"));
    assert!(snap2.has_index("idx_posts_user"));

    let desired = SchemaSnapshot::from_schema_sql(schema).await.unwrap();
    let diff = compute_diff(&desired, &snap2);
    assert!(
        diff.is_empty(),
        "Dump → converge should produce identical schema, got diff: {diff}"
    );
}

// ── Multiple operations in single convergence ──────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn single_convergence_handles_create_drop_rebuild() {
    let (_db, conn) = common::empty_db().await;
    let schema_v1 = "\
        CREATE TABLE keep (id TEXT PRIMARY KEY, val TEXT);\n\
        CREATE TABLE drop_me (id TEXT PRIMARY KEY);\n\
        CREATE TABLE rebuild_me (id TEXT PRIMARY KEY, name TEXT);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);";
    converge(&conn, schema_v1).await.unwrap();
    conn.execute("INSERT INTO keep VALUES ('1', 'a')", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO rebuild_me VALUES ('r1', 'alice')", ())
        .await
        .unwrap();

    let schema_v2 = "\
        CREATE TABLE keep (id TEXT PRIMARY KEY, val TEXT);\n\
        CREATE TABLE new_tbl (id TEXT PRIMARY KEY, data TEXT);\n\
        CREATE TABLE rebuild_me (id TEXT PRIMARY KEY, name INTEGER);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);";
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    let report = converge_with_options(&conn, schema_v2, &options)
        .await
        .unwrap();

    assert_eq!(report.tables_created, 1, "new_tbl created");
    assert_eq!(report.tables_dropped, 1, "drop_me dropped");
    assert_eq!(report.tables_rebuilt, 1, "rebuild_me rebuilt");

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(snap.has_table("keep"));
    assert!(snap.has_table("new_tbl"));
    assert!(snap.has_table("rebuild_me"));
    assert!(!snap.has_table("drop_me"));
}

// ── Pre-destructive hook with column drops ─────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn pre_destructive_hook_receives_column_drops() {
    let (_db, conn) = common::empty_db().await;
    let schema_v1 = "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT, extra TEXT);";
    converge(&conn, schema_v1).await.unwrap();

    let schema_v2 = "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT);";
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    let hook_called = Arc::new(std::sync::Mutex::new(false));
    let hook_called_clone = hook_called.clone();

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        pre_destructive_hook: Some(Arc::new(move |changes| {
            *hook_called_clone.lock().unwrap() = true;
            assert!(
                changes
                    .columns_to_drop
                    .iter()
                    .any(|(t, c)| t == "foo" && c == "extra"),
                "Hook should see column drop: {:?}",
                changes.columns_to_drop
            );
            Ok(())
        })),
        ..Default::default()
    };

    converge_with_options(&conn, schema_v2, &options)
        .await
        .unwrap();
    assert!(*hook_called.lock().unwrap(), "Hook should have been called");
}

// ── Capability detection basics ────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_detect_fts_and_materialized_views() {
    use turso_converge::Capabilities;

    let (_db, conn) = common::empty_db().await;
    let caps = Capabilities::detect(&conn).await.unwrap();
    assert!(caps.has_fts_module, "Test DB should have FTS support");
    assert!(
        caps.has_materialized_views,
        "Test DB should have materialized view support"
    );
    assert!(caps.supports_drop_column, "Should support DROP COLUMN");
    assert!(caps.supports_rename_column, "Should support RENAME COLUMN");
}

// ── UnsupportedFeature error path ──────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_fts_returns_error() {
    let db = turso::Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();

    let schema = "\
        CREATE TABLE docs (id TEXT PRIMARY KEY, title TEXT);\n\
        CREATE INDEX idx_fts ON docs USING fts (title);";

    let err = converge(&conn, schema).await.unwrap_err();
    match err {
        MigrateError::UnsupportedFeature(msg) => {
            assert!(msg.contains("FTS"), "Should mention FTS: {msg}");
        }
        other => panic!("expected UnsupportedFeature, got: {other:?}"),
    }
}

// ── NoOp mode (hash mismatch, no actual changes) ──────────────────

#[tokio::test(flavor = "multi_thread")]
async fn noop_mode_when_hash_mismatches_but_schema_matches() {
    let (_db, conn) = common::empty_db().await;
    let schema = "CREATE TABLE foo (id TEXT PRIMARY KEY, val TEXT);\n\
                  CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);";
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    converge_with_options(&conn, schema, &options)
        .await
        .unwrap();

    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'stale')",
        (),
    )
    .await
    .unwrap();

    let report = converge_with_options(&conn, schema, &options)
        .await
        .unwrap();
    assert_eq!(
        report.mode,
        ConvergeMode::NoOp,
        "Hash mismatch but no actual changes should produce NoOp"
    );
    assert!(!report.had_changes());
}

// ── validate_schema with empty string ──────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn validate_schema_empty_string_errors() {
    use turso_converge::validate_schema;
    let err = validate_schema("").await.unwrap_err();
    match err {
        MigrateError::Schema(msg) => assert!(msg.contains("empty")),
        other => panic!("expected Schema error, got: {other:?}"),
    }
}

// ── Backup to explicit file path ───────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn backup_to_explicit_file_path() {
    let (_db, conn) = common::empty_db().await;
    converge(&conn, common::test_schema()).await.unwrap();

    conn.execute("CREATE TABLE extra (id TEXT PRIMARY KEY)", ())
        .await
        .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let explicit_path = dir.path().join("my_backup.sql");

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        backup_before_destructive: Some(explicit_path.clone()),
        ..Default::default()
    };
    converge_with_options(&conn, common::test_schema(), &options)
        .await
        .unwrap();

    assert!(
        explicit_path.exists(),
        "Backup file should exist at exact path"
    );
    let contents = std::fs::read_to_string(&explicit_path).unwrap();
    assert!(contents.contains("CREATE TABLE"));
}

// ── Lease cleanup after convergence ────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn lease_cleaned_up_after_convergence() {
    let (_db, conn) = common::empty_db().await;
    let schema = "CREATE TABLE foo (id TEXT PRIMARY KEY);\n\
                  CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);";
    converge(&conn, schema).await.unwrap();

    let owner = common::get_meta(&conn, "migration_owner").await;
    let lease_until = common::get_meta(&conn, "migration_lease_until").await;
    assert!(
        owner.is_none(),
        "migration_owner should be cleared: {owner:?}"
    );
    assert!(
        lease_until.is_none(),
        "migration_lease_until should be cleared: {lease_until:?}"
    );
}

// ── Multiple data migrations with mixed state ──────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn multiple_data_migrations_partially_applied() {
    let (_db, conn) = common::empty_db().await;
    let schema = "CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT NOT NULL);";

    let first_run = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        data_migrations: vec![DataMigration {
            id: "seed-alice".to_string(),
            statements: vec!["INSERT INTO users VALUES ('a', 'alice')".to_string()],
        }],
        ..Default::default()
    };
    let r1 = converge_with_options(&conn, schema, &first_run)
        .await
        .unwrap();
    assert_eq!(r1.data_migrations_applied, 1);

    let second_run = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        data_migrations: vec![
            DataMigration {
                id: "seed-alice".to_string(),
                statements: vec!["INSERT INTO users VALUES ('a', 'alice')".to_string()],
            },
            DataMigration {
                id: "seed-bob".to_string(),
                statements: vec!["INSERT INTO users VALUES ('b', 'bob')".to_string()],
            },
        ],
        ..Default::default()
    };
    let r2 = converge_with_options(&conn, schema, &second_run)
        .await
        .unwrap();
    assert_eq!(
        r2.data_migrations_applied, 1,
        "Only seed-bob should be applied"
    );

    let mut rows = conn.query("SELECT COUNT(*) FROM users", ()).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let count: i64 = row.get(0).unwrap();
    assert_eq!(count, 2);
}

// ── schema_version on fresh DB ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn schema_version_returns_error_on_fresh_db() {
    let (_db, conn) = common::empty_db().await;
    let result = schema_version(&conn).await;
    assert!(
        result.is_err(),
        "schema_version on fresh DB (no table) should error"
    );
}

// ── plan_sql empty for non-dry-run ─────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn slow_path_report_has_empty_plan_sql() {
    let (_db, conn) = common::empty_db().await;
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    let report = converge_with_options(&conn, common::test_schema(), &options)
        .await
        .unwrap();
    assert_eq!(report.mode, ConvergeMode::SlowPath);
    assert!(
        report.plan_sql.is_empty(),
        "Non-dry-run should have empty plan_sql"
    );
}

// ── DestructiveChangeSet methods ───────────────────────────────────

#[test]
fn destructive_change_set_has_changes() {
    let empty = DestructiveChangeSet {
        tables_to_drop: vec![],
        columns_to_drop: vec![],
        tables_to_rebuild: vec![],
    };
    assert!(!empty.has_changes());

    let with_drop = DestructiveChangeSet {
        tables_to_drop: vec!["foo".to_string()],
        columns_to_drop: vec![],
        tables_to_rebuild: vec![],
    };
    assert!(with_drop.has_changes());
}

#[test]
fn destructive_change_set_blocked_operations() {
    let changes = DestructiveChangeSet {
        tables_to_drop: vec!["users".to_string()],
        columns_to_drop: vec![("posts".to_string(), "legacy".to_string())],
        tables_to_rebuild: vec!["items".to_string()],
    };
    let ops = changes.blocked_operations();
    assert!(ops.iter().any(|o| o.contains("DROP TABLE")));
    assert!(ops.iter().any(|o| o.contains("DROP COLUMN")));
    assert!(ops.iter().any(|o| o.contains("REBUILD")));
}

// ── SchemaSnapshot lookup methods edge cases ───────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_get_methods_return_none_for_missing() {
    let snap = SchemaSnapshot::from_schema_sql("CREATE TABLE foo (id TEXT PRIMARY KEY);")
        .await
        .unwrap();
    assert!(snap.get_table("nonexistent").is_none());
    assert!(snap.get_index("nonexistent").is_none());
    assert!(snap.get_view("nonexistent").is_none());
    assert!(snap.get_trigger("nonexistent").is_none());
    assert!(!snap.has_table("nonexistent"));
    assert!(!snap.has_index("nonexistent"));
    assert!(!snap.has_view("nonexistent"));
    assert!(!snap.has_trigger("nonexistent"));
}

// ── Functional verification: data survives full convergence cycle ──

#[tokio::test(flavor = "multi_thread")]
async fn full_convergence_cycle_preserves_data_integrity() {
    let (_db, conn) = common::empty_db().await;
    let schema = "\
        CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT NOT NULL, email TEXT);\n\
        CREATE TABLE posts (id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id), title TEXT NOT NULL);\n\
        CREATE INDEX idx_posts_user ON posts(user_id);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);";

    converge(&conn, schema).await.unwrap();

    conn.execute(
        "INSERT INTO users VALUES ('u1', 'Alice', 'alice@test.com')",
        (),
    )
    .await
    .unwrap();
    conn.execute("INSERT INTO users VALUES ('u2', 'Bob', 'bob@test.com')", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO posts VALUES ('p1', 'u1', 'Hello World')", ())
        .await
        .unwrap();

    let schema_v2 = "\
        CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT NOT NULL, email TEXT, bio TEXT);\n\
        CREATE TABLE posts (id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id), title TEXT NOT NULL);\n\
        CREATE INDEX idx_posts_user ON posts(user_id);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);";

    converge(&conn, schema_v2).await.unwrap();

    let mut user_rows = conn
        .query("SELECT id, name, email FROM users ORDER BY id", ())
        .await
        .unwrap();
    let u1 = user_rows.next().await.unwrap().unwrap();
    assert_eq!(u1.get::<String>(0).unwrap(), "u1");
    assert_eq!(u1.get::<String>(1).unwrap(), "Alice");
    assert_eq!(u1.get::<String>(2).unwrap(), "alice@test.com");
    let u2 = user_rows.next().await.unwrap().unwrap();
    assert_eq!(u2.get::<String>(0).unwrap(), "u2");

    let mut post_rows = conn
        .query("SELECT id, user_id, title FROM posts", ())
        .await
        .unwrap();
    let p1 = post_rows.next().await.unwrap().unwrap();
    assert_eq!(p1.get::<String>(0).unwrap(), "p1");
    assert_eq!(p1.get::<String>(1).unwrap(), "u1");
    assert_eq!(p1.get::<String>(2).unwrap(), "Hello World");

    let v = schema_version(&conn).await.unwrap();
    assert_eq!(v, 2, "Two DDL convergences = version 2");
}

// ── Index functionality after convergence ──────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn index_actually_works_after_convergence() {
    let (_db, conn) = common::empty_db().await;
    let schema = "\
        CREATE TABLE items (id TEXT PRIMARY KEY, category TEXT NOT NULL);\n\
        CREATE INDEX idx_items_cat ON items(category);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);";

    converge(&conn, schema).await.unwrap();

    for i in 0..100 {
        conn.execute(
            &format!("INSERT INTO items VALUES ('id_{i}', 'cat_{}')", i % 5),
            (),
        )
        .await
        .unwrap();
    }

    let mut rows = conn
        .query(
            "EXPLAIN QUERY PLAN SELECT * FROM items WHERE category = 'cat_0'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let detail: String = row.get(3).unwrap();
    assert!(
        detail.to_lowercase().contains("index") || detail.to_lowercase().contains("idx_items_cat"),
        "Query should use index: {detail}"
    );
}

// ── Capability validation: WITHOUT ROWID ───────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_without_rowid_returns_error() {
    let (_db, conn) = common::empty_db().await;
    let schema = "CREATE TABLE wr (id INTEGER PRIMARY KEY) WITHOUT ROWID;";
    let mut caps = Capabilities::detect(&conn).await.unwrap();
    caps.supports_without_rowid = false;
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        capabilities: Some(caps),
        ..Default::default()
    };
    let err = converge_with_options(&conn, schema, &options)
        .await
        .unwrap_err();
    match err {
        MigrateError::UnsupportedFeature(msg) => {
            assert!(
                msg.contains("WITHOUT ROWID"),
                "Should mention WITHOUT ROWID: {msg}"
            );
        }
        other => panic!("expected UnsupportedFeature, got: {other:?}"),
    }
}

// ── Capability validation: GENERATED columns ───────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_generated_returns_error() {
    let (_db, conn) = common::empty_db().await;
    let schema = "CREATE TABLE gen (x INTEGER, y INTEGER GENERATED ALWAYS AS (x * 2) STORED);";
    let mut caps = Capabilities::detect(&conn).await.unwrap();
    caps.supports_generated_columns = false;
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        capabilities: Some(caps),
        ..Default::default()
    };
    let err = converge_with_options(&conn, schema, &options)
        .await
        .unwrap_err();
    match err {
        MigrateError::UnsupportedFeature(msg) => {
            assert!(msg.contains("GENERATED"), "Should mention GENERATED: {msg}");
        }
        other => panic!("expected UnsupportedFeature, got: {other:?}"),
    }
}

// ── Capability validation: triggers ────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_triggers_returns_error() {
    let db = turso::Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();

    let schema = "\
        CREATE TABLE users (id TEXT PRIMARY KEY);\n\
        CREATE TRIGGER trg_ins AFTER INSERT ON users BEGIN SELECT 1; END;";

    let err = converge(&conn, schema).await.unwrap_err();
    match err {
        MigrateError::UnsupportedFeature(msg) => {
            assert!(
                msg.to_lowercase().contains("trigger"),
                "Should mention triggers: {msg}"
            );
        }
        other => panic!("expected UnsupportedFeature, got: {other:?}"),
    }
}

// ── Capability detection: new fields ───────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_detect_triggers_and_engine_features() {
    let (_db, conn) = common::empty_db().await;
    let caps = Capabilities::detect(&conn).await.unwrap();
    assert!(
        caps.has_triggers,
        "Test DB (all experimental flags) should have trigger support"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_detect_triggers_disabled_without_flag() {
    let db = turso::Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    let caps = Capabilities::detect(&conn).await.unwrap();
    assert!(
        !caps.has_triggers,
        "DB without experimental_triggers flag should not have trigger support"
    );
}

// ── Capabilities override in ConvergeOptions ───────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_override_skips_detection() {
    let (_db, conn) = common::empty_db().await;
    let schema = "\
        CREATE TABLE docs (id TEXT PRIMARY KEY, title TEXT);\n\
        CREATE INDEX idx_fts ON docs USING fts (title);";

    let caps = Capabilities {
        has_fts_module: false,
        ..Capabilities::default()
    };
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        capabilities: Some(caps),
        ..Default::default()
    };

    let err = converge_with_options(&conn, schema, &options)
        .await
        .unwrap_err();
    match err {
        MigrateError::UnsupportedFeature(msg) => {
            assert!(msg.contains("FTS"), "Should mention FTS: {msg}");
        }
        other => panic!("Override should block FTS even on capable DB, got: {other:?}"),
    }
}

// ── Table rebuild row count logging ────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_with_data_succeeds() {
    let (_db, conn) = common::empty_db().await;
    let v1 = "CREATE TABLE items (id TEXT PRIMARY KEY, val TEXT);";
    converge(&conn, v1).await.unwrap();
    conn.execute("INSERT INTO items VALUES ('1', 'a')", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO items VALUES ('2', 'b')", ())
        .await
        .unwrap();

    let v2 = "CREATE TABLE items (id TEXT PRIMARY KEY, val INTEGER);";
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    let report = converge_with_options(&conn, v2, &options).await.unwrap();
    assert_eq!(report.tables_rebuilt, 1);
}
