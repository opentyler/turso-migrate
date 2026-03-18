use tempfile::tempdir;
use turso_converge::diff::normalize_for_hash;
use turso_converge::{
    ConvergeMode, ConvergeOptions, ConvergePolicy, DataMigration, Failpoint, MigrateError,
    SchemaSnapshot, converge, converge_with_options, rollback_to_previous, schema_version,
};

fn test_schema() -> &'static str {
    include_str!("fixtures/schema.sql")
}

async fn empty_db() -> (turso::Database, turso::Connection) {
    let db = turso::Builder::new_local(":memory:")
        .experimental_index_method(true)
        .experimental_materialized_views(true)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    (db, conn)
}

async fn get_meta(conn: &turso::Connection, key: &str) -> Option<String> {
    let mut rows = conn
        .query("SELECT value FROM _schema_meta WHERE key = ?1", [key])
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    row.get::<String>(0).ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_fresh_db() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);

    let normalized = normalize_for_hash(test_schema());
    let expected_hash = blake3::hash(normalized.as_bytes()).to_hex().to_string();
    let hash = get_meta(&conn, "schema_hash").await;
    assert_eq!(hash.as_deref(), Some(expected_hash.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn fresh_db_has_schema_version_1() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();
    let version = schema_version(&conn).await.unwrap();
    assert_eq!(
        version, 1,
        "fresh database should start at schema version 1"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_op_convergence_does_not_increment_version() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();
    let v1 = schema_version(&conn).await.unwrap();
    converge(&conn, test_schema()).await.unwrap();
    let v2 = schema_version(&conn).await.unwrap();
    assert_eq!(v1, v2, "no-op convergence should not increment version");
}

#[tokio::test(flavor = "multi_thread")]
async fn ddl_convergence_increments_version() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();
    let v1 = schema_version(&conn).await.unwrap();
    assert_eq!(v1, 1);

    conn.execute("DROP TABLE settings", ()).await.unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force_reconverge')",
        (),
    )
    .await
    .unwrap();

    converge(&conn, test_schema()).await.unwrap();
    let v2 = schema_version(&conn).await.unwrap();
    assert_eq!(
        v2, 2,
        "convergence with DDL changes should increment version"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_is_idempotent() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();
    converge(&conn, test_schema()).await.unwrap();
    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn fast_path_skips_diff() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();
    converge(&conn, test_schema()).await.unwrap();

    let normalized = normalize_for_hash(test_schema());
    let expected_hash = blake3::hash(normalized.as_bytes()).to_hex().to_string();
    let hash = get_meta(&conn, "schema_hash").await;
    assert_eq!(hash.as_deref(), Some(expected_hash.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_recovery_forces_slow_path() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();

    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('migration_in_progress', '1')",
        (),
    )
    .await
    .unwrap();

    converge(&conn, test_schema()).await.unwrap();
    let in_progress = get_meta(&conn, "migration_in_progress").await;
    assert!(in_progress.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn schema_meta_table_bootstrapped() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = '_schema_meta'",
            (),
        )
        .await
        .unwrap();
    assert!(
        rows.next().await.unwrap().is_some(),
        "_schema_meta table should exist"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn hash_mismatch_triggers_convergence() {
    let (_db, conn) = empty_db().await;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS _schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO _schema_meta (key, value) VALUES ('schema_hash', 'fake_old_hash')",
        (),
    )
    .await
    .unwrap();

    converge(&conn, test_schema()).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);

    let normalized = normalize_for_hash(test_schema());
    let expected_hash = blake3::hash(normalized.as_bytes()).to_hex().to_string();
    let hash = get_meta(&conn, "schema_hash").await;
    assert_eq!(hash.as_deref(), Some(expected_hash.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_blocks_table_drops() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();

    conn.execute("CREATE TABLE extra_table (id TEXT PRIMARY KEY)", ())
        .await
        .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    let options = ConvergeOptions {
        policy: ConvergePolicy::default(),
        ..Default::default()
    };
    let result = converge_with_options(&conn, test_schema(), &options).await;
    assert!(result.is_err(), "Safe policy should block table drops");
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("policy violation"), "Got: {msg}");
}

#[tokio::test(flavor = "multi_thread")]
async fn permissive_policy_allows_drops() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();

    conn.execute("CREATE TABLE extra_table (id TEXT PRIMARY KEY)", ())
        .await
        .unwrap();
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
    let report = converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap();
    assert_eq!(report.tables_dropped, 1);
    assert_eq!(report.mode, ConvergeMode::SlowPath);
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_returns_plan_without_executing() {
    let (_db, conn) = empty_db().await;

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        dry_run: true,
        ..Default::default()
    };
    let report = converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap();
    assert_eq!(report.mode, ConvergeMode::DryRun);
    assert!(report.tables_created > 0);
    assert!(
        !report.plan_sql.is_empty(),
        "Dry-run should return plan SQL"
    );

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(snap.tables.is_empty(), "Dry-run should NOT execute DDL");

    let in_progress = get_meta(&conn, "migration_in_progress").await;
    let phase = get_meta(&conn, "migration_phase").await;
    assert!(
        in_progress.is_none(),
        "Dry-run should not leave migration_in_progress set"
    );
    assert!(
        phase.is_none(),
        "Dry-run should not leave migration_phase set"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_with_options_returns_report() {
    let (_db, conn) = empty_db().await;
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    let report = converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap();
    assert_eq!(report.mode, ConvergeMode::SlowPath);
    assert_eq!(report.tables_created, 10);
    assert!(report.had_changes());

    let report2 = converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap();
    assert_eq!(report2.mode, ConvergeMode::FastPath);
    assert!(!report2.had_changes());
}

#[tokio::test(flavor = "multi_thread")]
async fn state_update_is_atomic() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();

    let hash = get_meta(&conn, "schema_hash").await;
    assert!(hash.is_some(), "Hash should be stored");
    let in_progress = get_meta(&conn, "migration_in_progress").await;
    assert!(
        in_progress.is_none(),
        "migration_in_progress should be cleared"
    );
    let version = schema_version(&conn).await.unwrap();
    assert_eq!(version, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn drift_detection_forces_slow_path() {
    let (_db, conn) = empty_db().await;
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    let r1 = converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap();
    assert_eq!(r1.mode, ConvergeMode::SlowPath);

    conn.execute("CREATE TABLE drift_table (id TEXT PRIMARY KEY)", ())
        .await
        .unwrap();

    let r2 = converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap();
    assert!(
        r2.mode == ConvergeMode::SlowPath,
        "Out-of-band DDL should trigger slow-path via drift detection, got {:?}",
        r2.mode
    );

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(
        !snap.has_table("drift_table"),
        "Drift table should be dropped by convergence"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_to_previous_restores_prior_schema() {
    let (_db, conn) = empty_db().await;

    let schema_v1 = "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT NOT NULL);";
    let schema_v2 = "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT NOT NULL, email TEXT);";

    converge(&conn, schema_v1).await.unwrap();
    converge(&conn, schema_v2).await.unwrap();

    rollback_to_previous(&conn).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let foo = snap.get_table("foo").unwrap();
    assert!(foo.columns.iter().any(|c| c.name == "name"));
    assert!(!foo.columns.iter().any(|c| c.name == "email"));
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_without_snapshot_errors() {
    let (_db, conn) = empty_db().await;
    let err = rollback_to_previous(&conn).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("No previous schema stored"), "Got: {msg}");
}

#[tokio::test(flavor = "multi_thread")]
async fn pre_destructive_hook_can_block_migration() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();

    conn.execute("CREATE TABLE extra_table (id TEXT PRIMARY KEY)", ())
        .await
        .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        pre_destructive_hook: Some(std::sync::Arc::new(|changes| {
            if !changes.tables_to_drop.is_empty() {
                Err("table drops blocked by hook".to_string())
            } else {
                Ok(())
            }
        })),
        ..Default::default()
    };

    let err = converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap_err();
    match err {
        MigrateError::PreDestructiveHookRejected {
            message,
            blocked_operations,
        } => {
            assert!(message.contains("blocked"));
            assert!(
                blocked_operations
                    .iter()
                    .any(|op| op.contains("DROP TABLE"))
            );
        }
        other => panic!("expected hook rejection, got: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn query_only_mode_returns_read_only_error() {
    let (_db, conn) = empty_db().await;
    conn.execute("PRAGMA query_only = 1", ()).await.unwrap();

    let err = converge(&conn, test_schema()).await.unwrap_err();
    assert!(matches!(err, MigrateError::ReadOnly));

    conn.execute("PRAGMA query_only = 0", ()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn failpoint_injection_aborts_then_recovery_succeeds() {
    let (_db, conn) = empty_db().await;

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        failpoint: Some(Failpoint::BeforeExecute),
        ..Default::default()
    };

    let err = converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        MigrateError::InjectedFailure { failpoint } if failpoint == "before_execute"
    ));

    let in_progress = get_meta(&conn, "migration_in_progress").await;
    let phase = get_meta(&conn, "migration_phase").await;
    assert!(
        in_progress.is_none(),
        "pre-DDL failure should clear migration_in_progress"
    );
    assert!(
        phase.is_none(),
        "pre-DDL failure should clear migration_phase"
    );

    converge(&conn, test_schema()).await.unwrap();
    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn failpoint_after_execute_preserves_crash_recovery_state() {
    let (_db, conn) = empty_db().await;

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        failpoint: Some(Failpoint::AfterExecuteBeforeState),
        ..Default::default()
    };

    let err = converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        MigrateError::InjectedFailure { failpoint } if failpoint == "after_execute_before_state"
    ));

    let in_progress = get_meta(&conn, "migration_in_progress").await;
    assert_eq!(in_progress.as_deref(), Some("1"));

    let recover_options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    let report = converge_with_options(&conn, test_schema(), &recover_options)
        .await
        .unwrap();
    assert_eq!(report.mode, ConvergeMode::CrashRecovery);

    let cleared = get_meta(&conn, "migration_in_progress").await;
    assert!(cleared.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn schema_version_overflow_returns_error() {
    let (_db, conn) = empty_db().await;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL)",
        (),
    )
    .await
    .unwrap();
    conn.execute("DELETE FROM schema_version", ())
        .await
        .unwrap();
    conn.execute(
        "INSERT INTO schema_version (version, updated_at) VALUES (?1, ?2)",
        (u32::MAX as i64, "0"),
    )
    .await
    .unwrap();

    let schema = "\
        CREATE TABLE foo (id TEXT PRIMARY KEY);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);";

    let err = converge(&conn, schema).await.unwrap_err();
    assert!(
        err.to_string().contains("schema_version overflow"),
        "expected overflow error, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_before_destructive_writes_snapshot_file() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();

    conn.execute("CREATE TABLE extra_table (id TEXT PRIMARY KEY)", ())
        .await
        .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    let dir = tempdir().unwrap();
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        backup_before_destructive: Some(dir.path().to_path_buf()),
        ..Default::default()
    };

    converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap();

    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1, "expected one backup file");

    let backup_sql = std::fs::read_to_string(&entries[0]).unwrap();
    assert!(backup_sql.contains("CREATE TABLE"));
    assert!(backup_sql.contains("extra_table"));
}

#[tokio::test(flavor = "multi_thread")]
async fn data_migrations_apply_once() {
    let (_db, conn) = empty_db().await;
    let schema = "CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT NOT NULL);";

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        data_migrations: vec![DataMigration {
            id: "seed-users".to_string(),
            statements: vec![
                "INSERT INTO users (id, name) VALUES ('u1', 'alice')".to_string(),
                "INSERT INTO users (id, name) VALUES ('u2', 'bob')".to_string(),
            ],
        }],
        ..Default::default()
    };

    let r1 = converge_with_options(&conn, schema, &options)
        .await
        .unwrap();
    assert_eq!(r1.data_migrations_applied, 1);

    let mut rows = conn.query("SELECT COUNT(*) FROM users", ()).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let count: i64 = row.get(0).unwrap();
    assert_eq!(count, 2);

    let r2 = converge_with_options(&conn, schema, &options)
        .await
        .unwrap();
    assert_eq!(r2.data_migrations_applied, 0);

    let mut rows2 = conn.query("SELECT COUNT(*) FROM users", ()).await.unwrap();
    let row2 = rows2.next().await.unwrap().unwrap();
    let count2: i64 = row2.get(0).unwrap();
    assert_eq!(count2, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn data_migrations_run_even_when_schema_hash_matches() {
    let (_db, conn) = empty_db().await;
    let schema = "CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT NOT NULL);";

    converge(&conn, schema).await.unwrap();

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        data_migrations: vec![DataMigration {
            id: "seed-after-hash".to_string(),
            statements: vec!["INSERT INTO users (id, name) VALUES ('u3', 'charlie')".to_string()],
        }],
        ..Default::default()
    };

    let report = converge_with_options(&conn, schema, &options)
        .await
        .unwrap();
    assert_eq!(report.mode, ConvergeMode::SlowPath);
    assert_eq!(report.data_migrations_applied, 1);

    let mut rows = conn
        .query("SELECT name FROM users WHERE id = 'u3'", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let name: String = row.get(0).unwrap();
    assert_eq!(name, "charlie");
}
