use std::path::PathBuf;

use tempfile::tempdir;
use turso_migrate::{MigrateError, SchemaSnapshot, converge, converge_from_path};

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

#[tokio::test(flavor = "multi_thread")]
async fn converge_creates_tables() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);
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
async fn converge_empty_sql_errors() {
    let (_db, conn) = empty_db().await;
    let err = converge(&conn, "").await.unwrap_err();

    match err {
        MigrateError::Schema(msg) => assert!(msg.contains("empty schema SQL")),
        other => panic!("expected schema error for empty SQL, got: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_invalid_sql_errors() {
    let (_db, conn) = empty_db().await;
    let err = converge(&conn, "NOT VALID SQL").await.unwrap_err();
    assert!(
        matches!(err, MigrateError::Turso(_) | MigrateError::Statement { .. }),
        "expected SQL execution error, got: {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_from_path_works() {
    let (_db, conn) = empty_db().await;
    let dir = tempdir().unwrap();
    let schema_path = dir.path().join("schema.sql");
    std::fs::write(&schema_path, test_schema()).unwrap();

    converge_from_path(&conn, &schema_path).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_from_path_missing_file() {
    let (_db, conn) = empty_db().await;
    let missing = PathBuf::from("/definitely/missing/schema.sql");

    let err = converge_from_path(&conn, &missing).await.unwrap_err();
    match err {
        MigrateError::Io { path, .. } => assert_eq!(path, missing),
        other => panic!("expected io error, got: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn to_sql_round_trips() {
    let (_db1, conn1) = empty_db().await;
    conn1.execute_batch(test_schema()).await.unwrap();
    let snap1 = SchemaSnapshot::from_connection(&conn1).await.unwrap();

    let sql = snap1.to_sql();

    let (_db2, conn2) = empty_db().await;
    conn2.execute_batch(&sql).await.unwrap();
    let snap2 = SchemaSnapshot::from_connection(&conn2).await.unwrap();

    let tables1: Vec<_> = snap1
        .tables
        .keys()
        .filter(|name| name.raw() != "schema_version")
        .cloned()
        .collect();
    let tables2: Vec<_> = snap2.tables.keys().cloned().collect();
    assert_eq!(tables1, tables2);
}

#[tokio::test(flavor = "multi_thread")]
async fn to_sql_fk_order() {
    let (_db, conn) = empty_db().await;
    conn.execute_batch(test_schema()).await.unwrap();
    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let sql = snap.to_sql();

    let documents_pos = sql.find("CREATE TABLE documents").unwrap();
    let document_tags_pos = sql.find("CREATE TABLE document_tags").unwrap();
    assert!(documents_pos < document_tags_pos);
}

#[tokio::test(flavor = "multi_thread")]
async fn to_sql_output_is_executable() {
    let (_db1, conn1) = empty_db().await;
    conn1.execute_batch(test_schema()).await.unwrap();
    let snap = SchemaSnapshot::from_connection(&conn1).await.unwrap();

    let (_db2, conn2) = empty_db().await;
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
