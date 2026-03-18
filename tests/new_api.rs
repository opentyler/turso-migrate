mod common;

use std::path::PathBuf;

use tempfile::tempdir;
use turso_converge::{
    ConnectionLike, ConvergeMode, ConvergeOptions, ConvergePolicy, MigrateError, SchemaSnapshot,
    converge, converge_from_path, converge_like, converge_multi, converge_multi_with_options,
    is_read_only, schema_version_like, validate_schema,
};

#[tokio::test(flavor = "multi_thread")]
async fn converge_creates_tables() {
    let (_db, conn) = common::empty_db().await;
    converge(&conn, common::test_schema()).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_is_idempotent() {
    let (_db, conn) = common::empty_db().await;
    converge(&conn, common::test_schema()).await.unwrap();
    converge(&conn, common::test_schema()).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_empty_sql_errors() {
    let (_db, conn) = common::empty_db().await;
    let err = converge(&conn, "").await.unwrap_err();

    match err {
        MigrateError::Schema(msg) => assert!(msg.contains("empty schema SQL")),
        other => panic!("expected schema error for empty SQL, got: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_invalid_sql_errors() {
    let (_db, conn) = common::empty_db().await;
    let err = converge(&conn, "NOT VALID SQL").await.unwrap_err();
    assert!(
        matches!(err, MigrateError::Turso(_) | MigrateError::Statement { .. }),
        "expected SQL execution error, got: {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_from_path_works() {
    let (_db, conn) = common::empty_db().await;
    let dir = tempdir().unwrap();
    let schema_path = dir.path().join("schema.sql");
    std::fs::write(&schema_path, common::test_schema()).unwrap();

    converge_from_path(&conn, &schema_path).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_from_path_missing_file() {
    let (_db, conn) = common::empty_db().await;
    let missing = PathBuf::from("/definitely/missing/schema.sql");

    let err = converge_from_path(&conn, &missing).await.unwrap_err();
    match err {
        MigrateError::Io { path, .. } => assert_eq!(path, missing),
        other => panic!("expected io error, got: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn to_sql_round_trips() {
    let (_db1, conn1) = common::empty_db().await;
    conn1.execute_batch(common::test_schema()).await.unwrap();
    let snap1 = SchemaSnapshot::from_connection(&conn1).await.unwrap();

    let sql = snap1.to_sql();

    let (_db2, conn2) = common::empty_db().await;
    conn2.execute_batch(&sql).await.unwrap();
    let snap2 = SchemaSnapshot::from_connection(&conn2).await.unwrap();

    let tables1: Vec<_> = snap1.tables.keys().cloned().collect();
    let tables2: Vec<_> = snap2.tables.keys().cloned().collect();
    assert_eq!(tables1, tables2);
}

#[tokio::test(flavor = "multi_thread")]
async fn to_sql_fk_order() {
    let (_db, conn) = common::empty_db().await;
    conn.execute_batch(common::test_schema()).await.unwrap();
    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let sql = snap.to_sql();

    let documents_pos = sql.find("CREATE TABLE documents").unwrap();
    let document_tags_pos = sql.find("CREATE TABLE document_tags").unwrap();
    assert!(documents_pos < document_tags_pos);
}

#[tokio::test(flavor = "multi_thread")]
async fn to_sql_output_is_executable() {
    let (_db1, conn1) = common::empty_db().await;
    conn1.execute_batch(common::test_schema()).await.unwrap();
    let snap = SchemaSnapshot::from_connection(&conn1).await.unwrap();

    let (_db2, conn2) = common::empty_db().await;
    conn2.execute_batch(&snap.to_sql()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn to_sql_empty_snapshot() {
    let snap = SchemaSnapshot {
        tables: Default::default(),
        indexes: Default::default(),
        views: Default::default(),
        triggers: Default::default(),
    };

    assert_eq!(snap.to_sql(), "");
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_schema_accepts_valid_sql() {
    validate_schema(common::test_schema()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn validate_schema_rejects_invalid_sql() {
    let err = validate_schema("CREATE TABLE broken (").await.unwrap_err();
    assert!(matches!(err, MigrateError::Schema(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_multi_combines_schema_parts() {
    let (_db, conn) = common::empty_db().await;
    let parts = [
        "CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT NOT NULL);",
        "CREATE TABLE posts (id TEXT PRIMARY KEY, user_id TEXT REFERENCES users(id));",
        "CREATE INDEX idx_posts_user ON posts(user_id);",
    ];

    converge_multi(&conn, &parts).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(snap.has_table("users"));
    assert!(snap.has_table("posts"));
    assert!(snap.has_index("idx_posts_user"));
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_multi_with_options_supports_dry_run() {
    let (_db, conn) = common::empty_db().await;
    let parts = [
        "CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT NOT NULL);",
        "CREATE TABLE posts (id TEXT PRIMARY KEY, user_id TEXT REFERENCES users(id));",
    ];

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        dry_run: true,
        ..Default::default()
    };

    let report = converge_multi_with_options(&conn, &parts, &options)
        .await
        .unwrap();
    assert_eq!(report.mode, ConvergeMode::DryRun);
    assert!(!report.plan_sql.is_empty());

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(snap.tables.is_empty(), "dry-run should not mutate DB");
}

#[tokio::test(flavor = "multi_thread")]
async fn is_read_only_reflects_query_only_pragma() {
    let (_db, conn) = common::empty_db().await;
    assert!(!is_read_only(&conn).await.unwrap());

    conn.execute("PRAGMA query_only = 1", ()).await.unwrap();
    assert!(is_read_only(&conn).await.unwrap());
    conn.execute("PRAGMA query_only = 0", ()).await.unwrap();
}

struct WrappedConnection<'a> {
    inner: &'a turso::Connection,
}

impl ConnectionLike for WrappedConnection<'_> {
    fn as_turso_connection(&self) -> &turso::Connection {
        self.inner
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connection_like_wrappers_work() {
    let (_db, conn) = common::empty_db().await;
    let wrapped = WrappedConnection { inner: &conn };

    converge_like(&wrapped, common::test_schema())
        .await
        .unwrap();
    let version = schema_version_like(&wrapped).await.unwrap();
    assert_eq!(version, 1);
}
