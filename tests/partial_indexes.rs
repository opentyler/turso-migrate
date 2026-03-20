mod common;

use std::collections::BTreeMap;

use turso_converge::diff::compute_diff;
use turso_converge::plan::generate_plan;
use turso_converge::{
    CIString, ConvergeMode, ConvergeOptions, ConvergePolicy, IndexInfo, SchemaSnapshot, converge,
    converge_with_options,
};

fn ci(s: &str) -> CIString {
    CIString::new(s)
}

// ── Partial index converges without unnecessary changes ────────────

#[tokio::test(flavor = "multi_thread")]
async fn partial_index_same_where_no_diff() {
    let schema = r#"
        CREATE TABLE t (id TEXT PRIMARY KEY, deleted INTEGER DEFAULT 0, col TEXT);
        CREATE INDEX idx ON t(col) WHERE deleted = 0;
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.is_empty(),
        "Same partial index should produce no diff: {diff}"
    );
}

// ── Changed WHERE clause triggers index drop + recreate ────────────

#[tokio::test(flavor = "multi_thread")]
async fn changed_where_clause_triggers_index_recreate() {
    let schema_v1 = r#"
        CREATE TABLE t (id TEXT PRIMARY KEY, deleted INTEGER DEFAULT 0, active INTEGER DEFAULT 1, col TEXT);
        CREATE INDEX idx ON t(col) WHERE deleted = 0;
    "#;
    let schema_v2 = r#"
        CREATE TABLE t (id TEXT PRIMARY KEY, deleted INTEGER DEFAULT 0, active INTEGER DEFAULT 1, col TEXT);
        CREATE INDEX idx ON t(col) WHERE deleted = 0 AND active = 1;
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);

    assert!(
        diff.indexes_to_drop.contains(&"idx".to_string()),
        "Changed WHERE should drop old index, got diff: {diff}"
    );
    assert!(
        diff.indexes_to_create.contains(&"idx".to_string()),
        "Changed WHERE should create new index, got diff: {diff}"
    );
}

// ── Adding WHERE clause to non-partial index detects diff ──────────

#[tokio::test(flavor = "multi_thread")]
async fn adding_where_clause_detects_diff() {
    let schema_v1 = r#"
        CREATE TABLE t (id TEXT PRIMARY KEY, deleted INTEGER DEFAULT 0, col TEXT);
        CREATE INDEX idx ON t(col);
    "#;
    let schema_v2 = r#"
        CREATE TABLE t (id TEXT PRIMARY KEY, deleted INTEGER DEFAULT 0, col TEXT);
        CREATE INDEX idx ON t(col) WHERE deleted = 0;
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);

    assert!(
        diff.indexes_to_drop.contains(&"idx".to_string()),
        "Adding WHERE should drop old index: {diff}"
    );
    assert!(
        diff.indexes_to_create.contains(&"idx".to_string()),
        "Adding WHERE should create new index: {diff}"
    );
}

// ── Removing WHERE clause (partial → non-partial) detects diff ─────

#[tokio::test(flavor = "multi_thread")]
async fn removing_where_clause_detects_diff() {
    let schema_v1 = r#"
        CREATE TABLE t (id TEXT PRIMARY KEY, deleted INTEGER DEFAULT 0, col TEXT);
        CREATE INDEX idx ON t(col) WHERE deleted = 0;
    "#;
    let schema_v2 = r#"
        CREATE TABLE t (id TEXT PRIMARY KEY, deleted INTEGER DEFAULT 0, col TEXT);
        CREATE INDEX idx ON t(col);
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);

    assert!(
        diff.indexes_to_drop.contains(&"idx".to_string()),
        "Removing WHERE should drop old index: {diff}"
    );
    assert!(
        diff.indexes_to_create.contains(&"idx".to_string()),
        "Removing WHERE should create new index: {diff}"
    );
}

// ── Unit test: partial index comparison via struct-level diff ───────

#[test]
fn partial_index_sql_diff_detected_in_struct() {
    let mut desired = SchemaSnapshot {
        tables: BTreeMap::new(),
        indexes: BTreeMap::new(),
        views: BTreeMap::new(),
        triggers: BTreeMap::new(),
    };
    let mut actual = desired.clone();

    desired.indexes.insert(
        ci("idx"),
        IndexInfo {
            name: "idx".to_string(),
            table_name: "t".to_string(),
            sql: "CREATE INDEX idx ON t(col) WHERE deleted = 0 AND active = 1".to_string(),
            is_fts: false,
            is_unique: false,
            columns: vec!["col".to_string()],
        },
    );
    actual.indexes.insert(
        ci("idx"),
        IndexInfo {
            name: "idx".to_string(),
            table_name: "t".to_string(),
            sql: "CREATE INDEX idx ON t(col) WHERE deleted = 0".to_string(),
            is_fts: false,
            is_unique: false,
            columns: vec!["col".to_string()],
        },
    );

    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.indexes_to_drop.contains(&"idx".to_string()),
        "WHERE clause change should be detected via SQL comparison: {diff}"
    );
    assert!(diff.indexes_to_create.contains(&"idx".to_string()));
}

// ── End-to-end: partial index WHERE change converges correctly ─────

#[tokio::test(flavor = "multi_thread")]
async fn partial_index_where_change_converges_end_to_end() {
    let (_db, conn) = common::empty_db().await;

    let schema_v1 = r#"
        CREATE TABLE objects (
            id TEXT PRIMARY KEY,
            type TEXT NOT NULL,
            archived_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX idx_objects_active ON objects(type, created_at) WHERE archived_at IS NULL;
    "#;
    converge(&conn, schema_v1).await.unwrap();

    conn.execute(
        "INSERT INTO objects (id, type, created_at) VALUES ('1', 'doc', '2024-01-01')",
        (),
    )
    .await
    .unwrap();

    let schema_v2 = r#"
        CREATE TABLE objects (
            id TEXT PRIMARY KEY,
            type TEXT NOT NULL,
            archived_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX idx_objects_active ON objects(type, created_at) WHERE archived_at IS NULL AND type != 'comment';
    "#;

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
    let report = converge_with_options(&conn, schema_v2, &options)
        .await
        .unwrap();
    assert_eq!(report.mode, ConvergeMode::SlowPath);
    assert!(
        report.indexes_changed > 0,
        "Partial index WHERE change should register as index change"
    );

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    let idx = snap.get_index("idx_objects_active").unwrap();
    let sql_lower = idx.sql.to_ascii_lowercase();
    assert!(
        sql_lower.contains("'comment'") && sql_lower.contains("where"),
        "New WHERE clause should be applied, got: {}",
        idx.sql
    );

    let mut rows = conn
        .query("SELECT id FROM objects WHERE id = '1'", ())
        .await
        .unwrap();
    assert!(
        rows.next().await.unwrap().is_some(),
        "Data should survive index recreation"
    );
}

// ── Plan generates correct SQL for partial index recreation ────────

#[tokio::test(flavor = "multi_thread")]
async fn partial_index_plan_drops_and_recreates() {
    let schema_v1 = r#"
        CREATE TABLE t (id TEXT PRIMARY KEY, deleted INTEGER DEFAULT 0, col TEXT);
        CREATE INDEX idx ON t(col) WHERE deleted = 0;
    "#;
    let schema_v2 = r#"
        CREATE TABLE t (id TEXT PRIMARY KEY, deleted INTEGER DEFAULT 0, col TEXT);
        CREATE INDEX idx ON t(col) WHERE deleted = 0 AND col IS NOT NULL;
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    let plan = generate_plan(&diff, &desired, &actual).unwrap();

    let all_sql = plan
        .transactional_stmts
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        all_sql.contains("drop index"),
        "Plan should drop old partial index: {all_sql}"
    );
    assert!(
        all_sql.contains("where"),
        "Plan should recreate with WHERE clause: {all_sql}"
    );
}

// ── Idempotent: same partial index on second converge = fast path ──

#[tokio::test(flavor = "multi_thread")]
async fn partial_index_idempotent() {
    let (_db, conn) = common::empty_db().await;

    let schema = r#"
        CREATE TABLE t (id TEXT PRIMARY KEY, deleted INTEGER DEFAULT 0, col TEXT);
        CREATE INDEX idx ON t(col) WHERE deleted = 0;
    "#;

    converge(&conn, schema).await.unwrap();

    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    let report = converge_with_options(&conn, schema, &options)
        .await
        .unwrap();
    assert_eq!(
        report.mode,
        ConvergeMode::FastPath,
        "Second converge with same schema should be fast path"
    );
}

// ── WHERE clause whitespace normalization ──────────────────────────

#[test]
fn partial_index_whitespace_normalization() {
    let mut desired = SchemaSnapshot {
        tables: BTreeMap::new(),
        indexes: BTreeMap::new(),
        views: BTreeMap::new(),
        triggers: BTreeMap::new(),
    };
    let mut actual = desired.clone();

    desired.indexes.insert(
        ci("idx"),
        IndexInfo {
            name: "idx".to_string(),
            table_name: "t".to_string(),
            sql: "CREATE INDEX idx ON t(col) WHERE  deleted  =  0".to_string(),
            is_fts: false,
            is_unique: false,
            columns: vec!["col".to_string()],
        },
    );
    actual.indexes.insert(
        ci("idx"),
        IndexInfo {
            name: "idx".to_string(),
            table_name: "t".to_string(),
            sql: "CREATE INDEX idx ON t(col) WHERE deleted = 0".to_string(),
            is_fts: false,
            is_unique: false,
            columns: vec!["col".to_string()],
        },
    );

    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.is_empty(),
        "Whitespace-only WHERE difference should not trigger index change: {diff}"
    );
}
