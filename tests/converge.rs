use turso_migrate::diff::normalize_for_hash;
use turso_migrate::{
    ConvergeMode, ConvergeOptions, ConvergePolicy, SchemaSnapshot, converge, converge_with_options,
    schema_version,
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
