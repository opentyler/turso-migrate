use turso_migrate::execute::execute_plan;
use turso_migrate::plan::generate_plan;
use turso_migrate::{SchemaSnapshot, compute_diff};

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

#[tokio::test]
async fn fresh_db_convergence() {
    let (_db, conn) = empty_db().await;
    let desired = SchemaSnapshot::from_schema_sql(test_schema())
        .await
        .unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    assert!(!plan.is_empty());

    let docs_pos = plan
        .transactional_stmts
        .iter()
        .position(|s| s.to_lowercase().starts_with("create table documents"));
    let tags_pos = plan
        .transactional_stmts
        .iter()
        .position(|s| s.to_lowercase().starts_with("create table document_tags"));
    assert!(docs_pos.is_some());
    assert!(tags_pos.is_some());
    assert!(docs_pos < tags_pos);

    execute_plan(&conn, &plan).await.unwrap();

    let after = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(after.tables.len(), desired.tables.len());
}

#[tokio::test]
async fn idempotent_convergence() {
    let (_db, conn) = empty_db().await;
    let desired = SchemaSnapshot::from_schema_sql(test_schema())
        .await
        .unwrap();

    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    execute_plan(&conn, &plan).await.unwrap();

    let actual2 = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff2 = compute_diff(&desired, &actual2);
    assert!(diff2.is_empty(), "Second diff should be empty: {diff2:?}");
}

#[tokio::test]
async fn add_column_execution() {
    let (_db, conn) = empty_db().await;
    conn.execute(
        "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT NOT NULL)",
        (),
    )
    .await
    .unwrap();

    let desired_sql = "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT NOT NULL, extra TEXT);";
    let desired = SchemaSnapshot::from_schema_sql(desired_sql).await.unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(!diff.columns_to_add.is_empty());

    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    execute_plan(&conn, &plan).await.unwrap();

    let after = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let foo = after.get_table("foo").unwrap();
    assert!(foo.columns.iter().any(|c| c.name == "extra"));
}

#[tokio::test]
async fn table_rebuild_preserves_data() {
    let (_db, conn) = empty_db().await;
    conn.execute(
        "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT, legacy TEXT)",
        (),
    )
    .await
    .unwrap();
    conn.execute("INSERT INTO foo VALUES ('1', 'alice', 'old')", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO foo VALUES ('2', 'bob', 'old')", ())
        .await
        .unwrap();

    let desired_sql = "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT);";
    let desired = SchemaSnapshot::from_schema_sql(desired_sql).await.unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(diff.tables_to_rebuild.contains(&"foo".to_string()));

    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    execute_plan(&conn, &plan).await.unwrap();

    let mut rows = conn
        .query("SELECT id, name FROM foo ORDER BY id", ())
        .await
        .unwrap();
    let row1 = rows.next().await.unwrap().unwrap();
    assert_eq!(row1.get::<String>(0).unwrap(), "1");
    assert_eq!(row1.get::<String>(1).unwrap(), "alice");
    let row2 = rows.next().await.unwrap().unwrap();
    assert_eq!(row2.get::<String>(0).unwrap(), "2");
    assert_eq!(row2.get::<String>(1).unwrap(), "bob");

    let after = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let foo = after.get_table("foo").unwrap();
    assert!(!foo.columns.iter().any(|c| c.name == "legacy"));
}

#[tokio::test]
async fn plan_only_does_not_mutate() {
    let (_db, conn) = empty_db().await;
    let desired = SchemaSnapshot::from_schema_sql(test_schema())
        .await
        .unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    let plan = generate_plan(&diff, &desired, &actual).unwrap();

    assert!(!plan.is_empty());

    let still_empty = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(still_empty.tables.is_empty());
}

#[tokio::test]
async fn empty_plan_for_matching_schemas() {
    let (_db, conn) = empty_db().await;
    conn.execute_batch(test_schema()).await.unwrap();

    let desired = SchemaSnapshot::from_schema_sql(test_schema())
        .await
        .unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    assert!(plan.is_empty(), "Plan should be empty for matching schemas");
}

#[tokio::test]
async fn add_notnull_default_column_execution() {
    let (_db, conn) = empty_db().await;
    conn.execute("CREATE TABLE foo (id TEXT PRIMARY KEY)", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO foo VALUES ('1')", ())
        .await
        .unwrap();

    let desired_sql =
        "CREATE TABLE foo (id TEXT PRIMARY KEY, status TEXT NOT NULL DEFAULT 'active');";
    let desired = SchemaSnapshot::from_schema_sql(desired_sql).await.unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        !diff.columns_to_add.is_empty(),
        "NOT NULL+DEFAULT should be ADD COLUMN eligible"
    );
    assert!(
        diff.tables_to_rebuild.is_empty(),
        "Should NOT trigger rebuild"
    );

    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    execute_plan(&conn, &plan).await.unwrap();

    let after = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let foo = after.get_table("foo").unwrap();
    assert!(foo.columns.iter().any(|c| c.name == "status"));
}

#[tokio::test]
async fn fk_referenced_table_rebuild_preserves_integrity() {
    let (_db, conn) = empty_db().await;
    conn.execute(
        "CREATE TABLE parent (id TEXT PRIMARY KEY, name TEXT, old_col TEXT)",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE TABLE child (id TEXT PRIMARY KEY, parent_id TEXT REFERENCES parent(id))",
        (),
    )
    .await
    .unwrap();
    conn.execute("INSERT INTO parent VALUES ('p1', 'Parent1', 'legacy')", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO child VALUES ('c1', 'p1')", ())
        .await
        .unwrap();

    let desired_sql = "\
        CREATE TABLE parent (id TEXT PRIMARY KEY, name TEXT);\n\
        CREATE TABLE child (id TEXT PRIMARY KEY, parent_id TEXT REFERENCES parent(id));\n";
    let desired = SchemaSnapshot::from_schema_sql(desired_sql).await.unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"parent".to_string()),
        "Parent should be rebuilt (old_col removed)"
    );

    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    execute_plan(&conn, &plan).await.unwrap();

    let mut rows = conn
        .query("SELECT id FROM child WHERE parent_id = 'p1'", ())
        .await
        .unwrap();
    assert!(
        rows.next().await.unwrap().is_some(),
        "Child FK data should survive parent rebuild"
    );
}

#[tokio::test]
async fn fts_index_created_outside_transaction() {
    let (_db, conn) = empty_db().await;
    let schema_with_fts = "\
        CREATE TABLE docs (id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT);\n\
        CREATE INDEX idx_docs_fts ON docs USING fts (title, body);";
    let desired = SchemaSnapshot::from_schema_sql(schema_with_fts)
        .await
        .unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);

    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    assert!(
        !plan.non_transactional_stmts.is_empty(),
        "FTS index should be in non-transactional stmts"
    );
    assert!(
        plan.non_transactional_stmts
            .iter()
            .any(|s| s.contains("USING fts")),
        "Non-transactional stmts should contain FTS CREATE INDEX"
    );

    execute_plan(&conn, &plan).await.unwrap();

    let after = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let fts_indexes: Vec<_> = after.indexes.values().filter(|i| i.is_fts).collect();
    assert_eq!(
        fts_indexes.len(),
        1,
        "Should have 1 FTS index after execution"
    );
}

#[tokio::test]
async fn materialized_view_recreated_after_table_change() {
    let (_db, conn) = empty_db().await;
    let schema_v1 = "\
        CREATE TABLE items (id TEXT PRIMARY KEY, category TEXT, old_col TEXT);\n\
        CREATE MATERIALIZED VIEW mv_counts AS SELECT category, COUNT(*) as cnt FROM items GROUP BY category;";
    conn.execute_batch(schema_v1).await.unwrap();
    conn.execute("INSERT INTO items VALUES ('1', 'books', 'x')", ())
        .await
        .unwrap();

    let schema_v2 = "\
        CREATE TABLE items (id TEXT PRIMARY KEY, category TEXT);\n\
        CREATE MATERIALIZED VIEW mv_counts AS SELECT category, COUNT(*) as cnt FROM items GROUP BY category;";
    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(diff.tables_to_rebuild.contains(&"items".to_string()));

    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    execute_plan(&conn, &plan).await.unwrap();

    let after = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(
        after.has_view("mv_counts"),
        "Materialized view should be recreated"
    );
    assert!(
        !after
            .get_table("items")
            .unwrap()
            .columns
            .iter()
            .any(|c| c.name == "old_col")
    );
}

#[tokio::test]
async fn full_schema_convergence_creates_all_objects() {
    let (_db, conn) = empty_db().await;
    let desired = SchemaSnapshot::from_schema_sql(test_schema())
        .await
        .unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    execute_plan(&conn, &plan).await.unwrap();

    let after = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert_eq!(after.tables.len(), 10, "All 10 tables");
    let std_indexes: Vec<_> = after.indexes.values().filter(|i| !i.is_fts).collect();
    assert!(
        std_indexes.len() >= 7,
        "At least 7 standard indexes, got {}",
        std_indexes.len()
    );
    let fts_indexes: Vec<_> = after.indexes.values().filter(|i| i.is_fts).collect();
    assert_eq!(fts_indexes.len(), 2, "Exactly 2 FTS indexes");
    assert!(
        after.views.len() >= 4,
        "At least 4 views, got {}",
        after.views.len()
    );
}

#[tokio::test]
async fn rebuild_rejects_new_notnull_column_without_default() {
    let (_db, conn) = empty_db().await;
    conn.execute("CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT)", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO foo VALUES ('1', 'alice')", ())
        .await
        .unwrap();

    let desired_sql = "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT, required TEXT NOT NULL);";
    let desired = SchemaSnapshot::from_schema_sql(desired_sql).await.unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(diff.tables_to_rebuild.contains(&"foo".to_string()));

    let result = generate_plan(&diff, &desired, &actual);
    assert!(result.is_err(), "Should reject NOT NULL without DEFAULT");
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("NOT NULL") && msg.contains("required"),
        "Error should mention the column: {msg}"
    );
}

#[tokio::test]
async fn rebuild_with_notnull_default_column_applies_default() {
    let (_db, conn) = empty_db().await;
    conn.execute(
        "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT, old_col TEXT)",
        (),
    )
    .await
    .unwrap();
    conn.execute("INSERT INTO foo VALUES ('1', 'alice', 'legacy')", ())
        .await
        .unwrap();

    let desired_sql =
        "CREATE TABLE foo (id TEXT PRIMARY KEY, name TEXT, status TEXT NOT NULL DEFAULT 'active');";
    let desired = SchemaSnapshot::from_schema_sql(desired_sql).await.unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"foo".to_string()),
        "Removing old_col + adding status should trigger rebuild"
    );

    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    execute_plan(&conn, &plan).await.unwrap();

    let mut rows = conn
        .query("SELECT status FROM foo WHERE id = '1'", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let status: String = row.get(0).unwrap();
    assert_eq!(
        status, "active",
        "Default value should be applied to existing rows during rebuild"
    );
}

#[tokio::test]
async fn rebuild_with_self_referential_fk_succeeds() {
    let (_db, conn) = empty_db().await;
    conn.execute(
        "CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT, parent_id TEXT REFERENCES categories(id), old_col TEXT)",
        (),
    )
    .await
    .unwrap();
    conn.execute("INSERT INTO categories VALUES ('1', 'root', NULL, 'x')", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO categories VALUES ('2', 'child', '1', 'y')", ())
        .await
        .unwrap();

    let desired_sql = "CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT, parent_id TEXT REFERENCES categories(id));";
    let desired = SchemaSnapshot::from_schema_sql(desired_sql).await.unwrap();
    let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(diff.tables_to_rebuild.contains(&"categories".to_string()));

    let plan = generate_plan(&diff, &desired, &actual).unwrap();
    execute_plan(&conn, &plan).await.unwrap();

    let mut rows = conn
        .query("SELECT id, parent_id FROM categories ORDER BY id", ())
        .await
        .unwrap();
    let row1 = rows.next().await.unwrap().unwrap();
    assert_eq!(row1.get::<String>(0).unwrap(), "1");
    let row2 = rows.next().await.unwrap().unwrap();
    assert_eq!(row2.get::<String>(0).unwrap(), "2");
    assert_eq!(row2.get::<String>(1).unwrap(), "1");
}
