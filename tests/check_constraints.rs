//! TDD tests for CHECK constraint tracking and diffing (TC-1).
//!
//! RED phase: These tests define the expected behavior for CHECK constraint
//! detection in the diff engine. They should FAIL until the GREEN phase
//! implements CHECK constraint parsing, storage, and comparison.

mod common;

use turso_converge::{
    ConvergeOptions, ConvergePolicy, SchemaSnapshot, compute_diff, converge, converge_with_options,
};

// ── Same CHECK constraint produces no diff (no-op) ─────────────────

#[tokio::test(flavor = "multi_thread")]
async fn same_check_constraint_produces_no_diff() {
    let schema = r#"
        CREATE TABLE items (
            id TEXT PRIMARY KEY,
            type TEXT NOT NULL CHECK(type IN ('a', 'b'))
        );
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(diff.is_empty(), "Same CHECK should produce no diff: {diff}");
}

// ── Modified CHECK constraint triggers rebuild ─────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn modified_check_constraint_triggers_rebuild() {
    let schema_v1 = r#"
        CREATE TABLE items (
            id TEXT PRIMARY KEY,
            type TEXT NOT NULL CHECK(type IN ('a', 'b'))
        );
    "#;
    let schema_v2 = r#"
        CREATE TABLE items (
            id TEXT PRIMARY KEY,
            type TEXT NOT NULL CHECK(type IN ('a', 'b', 'c'))
        );
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"items".to_string()),
        "Modified CHECK should trigger rebuild, got diff: {diff}"
    );
}

// ── Column-level CHECK constraint tracked ──────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn column_level_check_tracked() {
    let schema_v1 = r#"
        CREATE TABLE scores (
            id TEXT PRIMARY KEY,
            value INTEGER CHECK(value BETWEEN 0 AND 100)
        );
    "#;
    let schema_v2 = r#"
        CREATE TABLE scores (
            id TEXT PRIMARY KEY,
            value INTEGER CHECK(value BETWEEN 0 AND 200)
        );
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"scores".to_string()),
        "Column-level CHECK change should trigger rebuild, got diff: {diff}"
    );
}

// ── Function-based CHECK constraint tracked ────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn function_based_check_tracked() {
    let schema_v1 = r#"
        CREATE TABLE docs (
            id TEXT PRIMARY KEY,
            properties TEXT CHECK(json_valid(properties))
        );
    "#;
    let schema_v2 = r#"
        CREATE TABLE docs (
            id TEXT PRIMARY KEY,
            properties TEXT CHECK(json_valid(properties) AND json_type(properties) = 'object')
        );
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"docs".to_string()),
        "Function-based CHECK change should trigger rebuild, got diff: {diff}"
    );
}

// ── Adding a CHECK to existing table triggers rebuild ──────────────

#[tokio::test(flavor = "multi_thread")]
async fn adding_check_to_existing_table_triggers_rebuild() {
    let schema_v1 = r#"
        CREATE TABLE items (
            id TEXT PRIMARY KEY,
            status TEXT NOT NULL
        );
    "#;
    let schema_v2 = r#"
        CREATE TABLE items (
            id TEXT PRIMARY KEY,
            status TEXT NOT NULL CHECK(status IN ('active', 'archived'))
        );
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"items".to_string()),
        "Adding CHECK to existing table should trigger rebuild, got diff: {diff}"
    );
}

// ── Removing a CHECK from existing table triggers rebuild ──────────

#[tokio::test(flavor = "multi_thread")]
async fn removing_check_from_existing_table_triggers_rebuild() {
    let schema_v1 = r#"
        CREATE TABLE items (
            id TEXT PRIMARY KEY,
            status TEXT NOT NULL CHECK(status IN ('active', 'archived'))
        );
    "#;
    let schema_v2 = r#"
        CREATE TABLE items (
            id TEXT PRIMARY KEY,
            status TEXT NOT NULL
        );
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"items".to_string()),
        "Removing CHECK should trigger rebuild, got diff: {diff}"
    );
}

// ── Table-level CHECK constraint tracked ───────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn table_level_check_triggers_rebuild() {
    let schema_v1 = r#"
        CREATE TABLE edges (
            id TEXT PRIMARY KEY,
            from_id TEXT NOT NULL,
            to_id TEXT NOT NULL,
            CHECK(from_id != to_id)
        );
    "#;
    let schema_v2 = r#"
        CREATE TABLE edges (
            id TEXT PRIMARY KEY,
            from_id TEXT NOT NULL,
            to_id TEXT NOT NULL,
            CHECK(from_id != to_id AND from_id != '')
        );
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"edges".to_string()),
        "Table-level CHECK change should trigger rebuild, got diff: {diff}"
    );
}

// ── End-to-end: CHECK change converges correctly ───────────────────

#[tokio::test(flavor = "multi_thread")]
async fn check_constraint_change_converges_end_to_end() {
    let (_db, conn) = common::empty_db().await;

    let schema_v1 = r#"
        CREATE TABLE items (
            id TEXT PRIMARY KEY,
            type TEXT NOT NULL CHECK(type IN ('doc', 'link'))
        );
    "#;
    converge(&conn, schema_v1).await.unwrap();

    conn.execute("INSERT INTO items VALUES ('1', 'doc')", ())
        .await
        .unwrap();

    let schema_v2 = r#"
        CREATE TABLE items (
            id TEXT PRIMARY KEY,
            type TEXT NOT NULL CHECK(type IN ('doc', 'link', 'comment'))
        );
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
    assert_eq!(
        report.tables_rebuilt, 1,
        "CHECK change should trigger table rebuild"
    );

    let mut rows = conn
        .query("SELECT type FROM items WHERE id = '1'", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let val: String = row.get(0).unwrap();
    assert_eq!(val, "doc", "Data should survive CHECK constraint rebuild");

    let result = conn
        .execute("INSERT INTO items VALUES ('2', 'comment')", ())
        .await;
    assert!(
        result.is_ok(),
        "New CHECK should allow 'comment': {:?}",
        result.err()
    );
}

// ── Same CHECK, different whitespace = no diff ─────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn check_whitespace_normalization_no_diff() {
    let schema_v1 = r#"
        CREATE TABLE items (
            id TEXT PRIMARY KEY,
            type TEXT CHECK( type IN ('a','b') )
        );
    "#;
    let schema_v2 = r#"
        CREATE TABLE items (
            id TEXT PRIMARY KEY,
            type TEXT CHECK(type IN ('a','b'))
        );
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.is_empty(),
        "Whitespace-only CHECK difference should not trigger rebuild: {diff}"
    );
}

// ── Adding table-level CHECK to existing table triggers rebuild ────

#[tokio::test(flavor = "multi_thread")]
async fn adding_table_level_check_triggers_rebuild() {
    let schema_v1 = r#"
        CREATE TABLE edges (
            id TEXT PRIMARY KEY,
            from_id TEXT NOT NULL,
            to_id TEXT NOT NULL
        );
    "#;
    let schema_v2 = r#"
        CREATE TABLE edges (
            id TEXT PRIMARY KEY,
            from_id TEXT NOT NULL,
            to_id TEXT NOT NULL,
            CHECK(from_id != to_id)
        );
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"edges".to_string()),
        "Adding table-level CHECK should trigger rebuild, got diff: {diff}"
    );
}

// ── Removing table-level CHECK triggers rebuild ────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn removing_table_level_check_triggers_rebuild() {
    let schema_v1 = r#"
        CREATE TABLE edges (
            id TEXT PRIMARY KEY,
            from_id TEXT NOT NULL,
            to_id TEXT NOT NULL,
            CHECK(from_id != to_id)
        );
    "#;
    let schema_v2 = r#"
        CREATE TABLE edges (
            id TEXT PRIMARY KEY,
            from_id TEXT NOT NULL,
            to_id TEXT NOT NULL
        );
    "#;

    let desired = SchemaSnapshot::from_schema_sql(schema_v2).await.unwrap();
    let actual = SchemaSnapshot::from_schema_sql(schema_v1).await.unwrap();
    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.contains(&"edges".to_string()),
        "Removing table-level CHECK should trigger rebuild, got diff: {diff}"
    );
}
