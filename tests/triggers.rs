mod common;

use turso_converge::schema::SchemaSnapshot;
use turso_converge::{compute_diff, converge};

#[tokio::test(flavor = "multi_thread")]
async fn diff_detects_new_trigger() {
    let desired = SchemaSnapshot::from_schema_sql(
        "CREATE TABLE users (id TEXT PRIMARY KEY);\n\
         CREATE TRIGGER trg_ins AFTER INSERT ON users BEGIN SELECT 1; END;",
    )
    .await
    .unwrap();
    let actual = SchemaSnapshot::from_schema_sql("CREATE TABLE users (id TEXT PRIMARY KEY);")
        .await
        .unwrap();

    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.triggers_to_create.iter().any(|t| t == "trg_ins"),
        "Should detect new trigger, got: {:?}",
        diff.triggers_to_create
    );
    assert!(diff.triggers_to_drop.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_detects_removed_trigger() {
    let desired = SchemaSnapshot::from_schema_sql("CREATE TABLE users (id TEXT PRIMARY KEY);")
        .await
        .unwrap();
    let actual = SchemaSnapshot::from_schema_sql(
        "CREATE TABLE users (id TEXT PRIMARY KEY);\n\
         CREATE TRIGGER trg_old AFTER INSERT ON users BEGIN SELECT 1; END;",
    )
    .await
    .unwrap();

    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.triggers_to_drop.iter().any(|t| t == "trg_old"),
        "Should detect removed trigger, got: {:?}",
        diff.triggers_to_drop
    );
    assert!(diff.triggers_to_create.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_detects_changed_trigger() {
    let desired = SchemaSnapshot::from_schema_sql(
        "CREATE TABLE users (id TEXT PRIMARY KEY);\n\
         CREATE TRIGGER trg_audit AFTER UPDATE ON users BEGIN SELECT 2; END;",
    )
    .await
    .unwrap();
    let actual = SchemaSnapshot::from_schema_sql(
        "CREATE TABLE users (id TEXT PRIMARY KEY);\n\
         CREATE TRIGGER trg_audit AFTER UPDATE ON users BEGIN SELECT 1; END;",
    )
    .await
    .unwrap();

    let diff = compute_diff(&desired, &actual);
    assert!(diff.triggers_to_drop.iter().any(|t| t == "trg_audit"));
    assert!(diff.triggers_to_create.iter().any(|t| t == "trg_audit"));
}

#[tokio::test(flavor = "multi_thread")]
async fn trigger_created_via_execution() {
    let (_db, conn) = common::empty_db().await;

    let schema = "\
        CREATE TABLE audit_log (id INTEGER PRIMARY KEY, msg TEXT);\n\
        CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);\n\
        CREATE TRIGGER trg_user_insert AFTER INSERT ON users BEGIN INSERT INTO audit_log (msg) VALUES ('user created: ' || NEW.id); END;";

    converge(&conn, schema).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(
        snap.triggers.values().any(|t| t.name == "trg_user_insert"),
        "Trigger should be created"
    );

    conn.execute("INSERT INTO users VALUES ('u1', 'Alice')", ())
        .await
        .unwrap();

    let mut rows = conn.query("SELECT msg FROM audit_log", ()).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let msg: String = row.get(0).unwrap();
    assert!(msg.contains("u1"), "Trigger should have fired");
}

#[tokio::test(flavor = "multi_thread")]
async fn trigger_dropped_when_removed_from_schema() {
    let (_db, conn) = common::empty_db().await;

    let schema_v1 = "\
        CREATE TABLE audit_log (id INTEGER PRIMARY KEY, msg TEXT);\n\
        CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);\n\
        CREATE TRIGGER trg_user_insert AFTER INSERT ON users BEGIN INSERT INTO audit_log (msg) VALUES ('created'); END;";

    converge(&conn, schema_v1).await.unwrap();

    let schema_v2 = "\
        CREATE TABLE audit_log (id INTEGER PRIMARY KEY, msg TEXT);\n\
        CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);";

    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    converge(&conn, schema_v2).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(
        !snap.triggers.values().any(|t| t.name == "trg_user_insert"),
        "Trigger should be dropped"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn trigger_recreated_on_table_rebuild() {
    let (_db, conn) = common::empty_db().await;

    let schema_v1 = "\
        CREATE TABLE audit_log (id INTEGER PRIMARY KEY, msg TEXT);\n\
        CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT, old_col TEXT);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);\n\
        CREATE TRIGGER trg_user_insert AFTER INSERT ON users BEGIN INSERT INTO audit_log (msg) VALUES ('created'); END;";

    converge(&conn, schema_v1).await.unwrap();

    let schema_v2 = "\
        CREATE TABLE audit_log (id INTEGER PRIMARY KEY, msg TEXT);\n\
        CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT);\n\
        CREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);\n\
        CREATE TRIGGER trg_user_insert AFTER INSERT ON users BEGIN INSERT INTO audit_log (msg) VALUES ('created'); END;";

    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('schema_hash', 'force')",
        (),
    )
    .await
    .unwrap();

    converge(&conn, schema_v2).await.unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(
        snap.triggers.values().any(|t| t.name == "trg_user_insert"),
        "Trigger should be recreated after table rebuild"
    );

    conn.execute("INSERT INTO users VALUES ('u1', 'Alice')", ())
        .await
        .unwrap();
    let mut rows = conn.query("SELECT msg FROM audit_log", ()).await.unwrap();
    assert!(
        rows.next().await.unwrap().is_some(),
        "Trigger should fire after recreation"
    );
}
