use turso_migrate::{SchemaSnapshot, converge, schema_version};

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

    assert!(!test_schema().is_empty());

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);

    let expected_hash = blake3::hash(test_schema().as_bytes()).to_hex().to_string();
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

    let expected_hash = blake3::hash(test_schema().as_bytes()).to_hex().to_string();
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

    let expected_hash = blake3::hash(test_schema().as_bytes()).to_hex().to_string();
    let hash = get_meta(&conn, "schema_hash").await;
    assert_eq!(hash.as_deref(), Some(expected_hash.as_str()));
}
