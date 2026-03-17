use turso_migrate::{Migrator, SchemaSnapshot};

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
async fn migrator_plan_without_execute() {
    let (_db, conn) = empty_db().await;
    let plan = Migrator::new(test_schema()).plan(&conn).await.unwrap();
    assert!(!plan.is_empty());

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(snap.tables.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn migrator_migrate_creates_schema() {
    let (_db, conn) = empty_db().await;
    let plan = Migrator::new(test_schema()).migrate(&conn).await.unwrap();
    assert!(!plan.is_empty());

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_deletions_false_keeps_extra_tables() {
    let (_db, conn) = empty_db().await;

    conn.execute_batch(test_schema()).await.unwrap();
    conn.execute("CREATE TABLE extra_legacy (id TEXT PRIMARY KEY)", ())
        .await
        .unwrap();

    Migrator::new(test_schema())
        .allow_deletions(false)
        .migrate(&conn)
        .await
        .unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(
        snap.has_table("extra_legacy"),
        "Extra table should NOT be dropped when allow_deletions=false"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_deletions_true_drops_extra_tables() {
    let (_db, conn) = empty_db().await;

    conn.execute_batch(test_schema()).await.unwrap();
    conn.execute("CREATE TABLE extra_legacy (id TEXT PRIMARY KEY)", ())
        .await
        .unwrap();

    Migrator::new(test_schema())
        .allow_deletions(true)
        .migrate(&conn)
        .await
        .unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(
        !snap.has_table("extra_legacy"),
        "Extra table SHOULD be dropped when allow_deletions=true"
    );
}
