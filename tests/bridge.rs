mod common;

use turso_converge::{SchemaSnapshot, bridge_legacy, converge};

#[tokio::test(flavor = "multi_thread")]
async fn fresh_db_no_bridge_needed() {
    let (_db, conn) = common::empty_db().await;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        (),
    )
    .await
    .unwrap();

    let bridged = bridge_legacy(&conn).await.unwrap();
    assert!(!bridged, "Fresh DB should not need bridging");
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_db_gets_bridged() {
    let (_db, conn) = common::empty_db().await;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE TABLE _turso_migrations (version INTEGER, name TEXT, applied_at TEXT)",
        (),
    )
    .await
    .unwrap();

    let bridged = bridge_legacy(&conn).await.unwrap();
    assert!(bridged, "Legacy DB should be bridged");

    let mut rows = conn
        .query(
            "SELECT value FROM _schema_meta WHERE key = 'legacy_complete'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "1");
}

#[tokio::test(flavor = "multi_thread")]
async fn already_bridged_db_skips() {
    let (_db, conn) = common::empty_db().await;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE TABLE _turso_migrations (version INTEGER, name TEXT, applied_at TEXT)",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO _schema_meta (key, value) VALUES ('legacy_complete', '1')",
        (),
    )
    .await
    .unwrap();

    let bridged = bridge_legacy(&conn).await.unwrap();
    assert!(!bridged, "Already-bridged DB should skip");
}

#[tokio::test(flavor = "multi_thread")]
async fn full_convergence_with_legacy_db() {
    let (_db, conn) = common::empty_db().await;
    conn.execute(
        "CREATE TABLE _turso_migrations (version INTEGER, name TEXT, applied_at TEXT)",
        (),
    )
    .await
    .unwrap();

    converge(&conn, common::test_schema()).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);

    bridge_legacy(&conn).await.unwrap();

    let mut rows = conn
        .query(
            "SELECT value FROM _schema_meta WHERE key = 'legacy_complete'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "1");
}
