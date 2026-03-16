use turso_migrate::{converge, converge_data};

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
async fn converge_data_on_fresh_db() {
    let (_db, conn) = empty_db().await;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        (),
    )
    .await
    .unwrap();

    converge_data(&conn).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_data_is_idempotent() {
    let (_db, conn) = empty_db().await;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        (),
    )
    .await
    .unwrap();

    converge_data(&conn).await.unwrap();
    converge_data(&conn).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn full_convergence_runs_data_migrations() {
    let (_db, conn) = empty_db().await;
    converge(&conn, test_schema()).await.unwrap();

    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = '_schema_meta'",
            (),
        )
        .await
        .unwrap();
    assert!(rows.next().await.unwrap().is_some());
}
