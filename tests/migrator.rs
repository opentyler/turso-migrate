use turso_migrate::{
    ConvergeMode, ConvergeOptions, ConvergePolicy, SchemaSnapshot, converge_with_options,
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

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_does_not_execute() {
    let (_db, conn) = empty_db().await;
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        dry_run: true,
        ..Default::default()
    };
    let report = converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap();
    assert!(!report.plan_sql.is_empty());
    assert_eq!(report.mode, ConvergeMode::DryRun);

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(snap.tables.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn converge_creates_schema() {
    let (_db, conn) = empty_db().await;
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    let report = converge_with_options(&conn, test_schema(), &options)
        .await
        .unwrap();
    assert!(report.had_changes());

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(snap.tables.len(), 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn safe_policy_keeps_extra_tables() {
    let (_db, conn) = empty_db().await;
    let permissive = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    converge_with_options(&conn, test_schema(), &permissive)
        .await
        .unwrap();

    conn.execute("CREATE TABLE extra_legacy (id TEXT PRIMARY KEY)", ())
        .await
        .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    let safe = ConvergeOptions::default();
    let result = converge_with_options(&conn, test_schema(), &safe).await;
    assert!(result.is_err(), "Safe policy should block table drops");

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(
        snap.has_table("extra_legacy"),
        "Extra table should NOT be dropped when policy blocks drops"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn permissive_policy_drops_extra_tables() {
    let (_db, conn) = empty_db().await;
    let permissive = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    converge_with_options(&conn, test_schema(), &permissive)
        .await
        .unwrap();

    conn.execute("CREATE TABLE extra_legacy (id TEXT PRIMARY KEY)", ())
        .await
        .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    let report = converge_with_options(&conn, test_schema(), &permissive)
        .await
        .unwrap();
    assert_eq!(report.tables_dropped, 1);

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(
        !snap.has_table("extra_legacy"),
        "Extra table SHOULD be dropped when policy allows drops"
    );
}
