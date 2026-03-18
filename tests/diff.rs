use std::collections::BTreeMap;

use turso_converge::diff::{compute_diff, compute_diff_with_hints};
use turso_converge::{
    CIString, ColumnInfo, ColumnRenameHint, IndexInfo, SchemaSnapshot, TableInfo, ViewInfo,
};

fn test_schema() -> &'static str {
    include_str!("fixtures/schema.sql")
}

fn empty_snapshot() -> SchemaSnapshot {
    SchemaSnapshot {
        tables: BTreeMap::new(),
        indexes: BTreeMap::new(),
        views: BTreeMap::new(),
        triggers: BTreeMap::new(),
    }
}

fn make_column(
    name: &str,
    col_type: &str,
    notnull: bool,
    default: Option<&str>,
    pk: i64,
) -> ColumnInfo {
    ColumnInfo {
        name: name.to_string(),
        col_type: col_type.to_string(),
        notnull,
        default_value: default.map(|s| s.to_string()),
        pk,
        collation: None,
        is_generated: false,
        is_hidden: false,
    }
}

fn make_table(name: &str, columns: Vec<ColumnInfo>) -> TableInfo {
    TableInfo {
        name: name.to_string(),
        sql: format!("CREATE TABLE {name} (...)"),
        columns,
        foreign_keys: vec![],
        is_strict: false,
        is_without_rowid: false,
        has_autoincrement: false,
    }
}

fn ci(s: &str) -> CIString {
    CIString::new(s)
}

#[test]
fn identical_schemas_produce_empty_diff() {
    let mut desired = empty_snapshot();
    desired.tables.insert(
        ci("foo"),
        make_table("foo", vec![make_column("id", "TEXT", true, None, 1)]),
    );

    let actual = desired.clone();
    let diff = compute_diff(&desired, &actual);
    assert!(diff.is_empty(), "Expected empty diff, got: {diff:?}");
}

#[test]
fn new_table_detected() {
    let mut desired = empty_snapshot();
    desired.tables.insert(ci("foo"), make_table("foo", vec![]));
    let actual = empty_snapshot();

    let diff = compute_diff(&desired, &actual);
    assert_eq!(diff.tables_to_create, vec!["foo".to_string()]);
}

#[test]
fn removed_table_detected() {
    let desired = empty_snapshot();
    let mut actual = empty_snapshot();
    actual.tables.insert(ci("foo"), make_table("foo", vec![]));

    let diff = compute_diff(&desired, &actual);
    assert_eq!(diff.tables_to_drop, vec!["foo".to_string()]);
}

#[test]
fn added_nullable_column_detected() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    actual.tables.insert(
        ci("foo"),
        make_table("foo", vec![make_column("id", "TEXT", true, None, 1)]),
    );
    desired.tables.insert(
        ci("foo"),
        make_table(
            "foo",
            vec![
                make_column("id", "TEXT", true, None, 1),
                make_column("extra", "TEXT", false, None, 0),
            ],
        ),
    );

    let diff = compute_diff(&desired, &actual);
    assert_eq!(diff.tables_to_rebuild, Vec::<String>::new());
    assert_eq!(diff.columns_to_add.len(), 1);
    assert_eq!(diff.columns_to_add[0].0, "foo");
    assert_eq!(diff.columns_to_add[0].1.name, "extra");
}

#[test]
fn added_notnull_default_column_detected() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    actual.tables.insert(
        ci("foo"),
        make_table("foo", vec![make_column("id", "TEXT", true, None, 1)]),
    );
    desired.tables.insert(
        ci("foo"),
        make_table(
            "foo",
            vec![
                make_column("id", "TEXT", true, None, 1),
                make_column("status", "TEXT", true, Some("'new'"), 0),
            ],
        ),
    );

    let diff = compute_diff(&desired, &actual);
    assert_eq!(diff.tables_to_rebuild, Vec::<String>::new());
    assert_eq!(diff.columns_to_add.len(), 1);
    assert_eq!(diff.columns_to_add[0].0, "foo");
    assert_eq!(diff.columns_to_add[0].1.name, "status");
}

#[test]
fn added_notnull_no_default_triggers_rebuild() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    actual.tables.insert(
        ci("foo"),
        make_table("foo", vec![make_column("id", "TEXT", true, None, 1)]),
    );
    desired.tables.insert(
        ci("foo"),
        make_table(
            "foo",
            vec![
                make_column("id", "TEXT", true, None, 1),
                make_column("required", "TEXT", true, None, 0),
            ],
        ),
    );

    let diff = compute_diff(&desired, &actual);
    assert_eq!(diff.tables_to_rebuild, vec!["foo".to_string()]);
    assert!(diff.columns_to_add.is_empty());
}

#[test]
fn removed_eligible_column_uses_drop_column() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    desired.tables.insert(
        ci("foo"),
        make_table("foo", vec![make_column("id", "TEXT", true, None, 1)]),
    );
    actual.tables.insert(
        ci("foo"),
        make_table(
            "foo",
            vec![
                make_column("id", "TEXT", true, None, 1),
                make_column("legacy", "TEXT", false, None, 0),
            ],
        ),
    );

    let diff = compute_diff(&desired, &actual);
    assert!(diff.tables_to_rebuild.is_empty(), "Should NOT rebuild");
    assert_eq!(diff.columns_to_drop.len(), 1);
    assert_eq!(
        diff.columns_to_drop[0],
        ("foo".to_string(), "legacy".to_string())
    );
}

#[test]
fn removed_pk_column_triggers_rebuild() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    desired.tables.insert(
        ci("foo"),
        make_table("foo", vec![make_column("name", "TEXT", false, None, 0)]),
    );
    actual.tables.insert(
        ci("foo"),
        make_table(
            "foo",
            vec![
                make_column("id", "TEXT", true, None, 1),
                make_column("name", "TEXT", false, None, 0),
            ],
        ),
    );

    let diff = compute_diff(&desired, &actual);
    assert_eq!(diff.tables_to_rebuild, vec!["foo".to_string()]);
    assert!(
        diff.columns_to_drop.is_empty(),
        "PK column cannot use DROP COLUMN"
    );
}

#[test]
fn removed_indexed_column_triggers_rebuild() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    desired.tables.insert(
        ci("foo"),
        make_table("foo", vec![make_column("id", "TEXT", true, None, 1)]),
    );
    actual.tables.insert(
        ci("foo"),
        make_table(
            "foo",
            vec![
                make_column("id", "TEXT", true, None, 1),
                make_column("status", "TEXT", false, None, 0),
            ],
        ),
    );
    actual.indexes.insert(
        ci("idx_foo_status"),
        IndexInfo {
            name: "idx_foo_status".to_string(),
            table_name: "foo".to_string(),
            sql: "CREATE INDEX idx_foo_status ON foo(status)".to_string(),
            is_fts: false,
            is_unique: false,
            columns: vec!["status".to_string()],
        },
    );

    let diff = compute_diff(&desired, &actual);
    assert_eq!(diff.tables_to_rebuild, vec!["foo".to_string()]);
    assert!(
        diff.columns_to_drop.is_empty(),
        "Indexed column cannot use DROP COLUMN"
    );
}

#[test]
fn changed_column_type_triggers_rebuild() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    desired.tables.insert(
        ci("foo"),
        make_table("foo", vec![make_column("value", "INTEGER", false, None, 0)]),
    );
    actual.tables.insert(
        ci("foo"),
        make_table("foo", vec![make_column("value", "TEXT", false, None, 0)]),
    );

    let diff = compute_diff(&desired, &actual);
    assert_eq!(diff.tables_to_rebuild, vec!["foo".to_string()]);
}

#[test]
fn column_rename_is_detected_conservatively() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    actual.tables.insert(
        ci("foo"),
        make_table(
            "foo",
            vec![
                make_column("id", "TEXT", true, None, 1),
                make_column("legacy_name", "TEXT", false, None, 0),
            ],
        ),
    );
    desired.tables.insert(
        ci("foo"),
        make_table(
            "foo",
            vec![
                make_column("id", "TEXT", true, None, 1),
                make_column("display_name", "TEXT", false, None, 0),
            ],
        ),
    );

    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.tables_to_rebuild.is_empty(),
        "rename should not rebuild"
    );
    assert!(diff.columns_to_add.is_empty(), "rename should not add");
    assert!(diff.columns_to_drop.is_empty(), "rename should not drop");
    assert_eq!(
        diff.columns_to_rename,
        vec![(
            "foo".to_string(),
            "legacy_name".to_string(),
            "display_name".to_string()
        )]
    );
}

#[test]
fn rename_hint_enables_non_positional_rename() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    actual.tables.insert(
        ci("foo"),
        make_table(
            "foo",
            vec![
                make_column("id", "TEXT", true, None, 1),
                make_column("legacy_name", "TEXT", false, None, 0),
                make_column("keep", "TEXT", false, None, 0),
            ],
        ),
    );
    desired.tables.insert(
        ci("foo"),
        make_table(
            "foo",
            vec![
                make_column("id", "TEXT", true, None, 1),
                make_column("keep", "TEXT", false, None, 0),
                make_column("display_name", "TEXT", false, None, 0),
            ],
        ),
    );

    let baseline = compute_diff(&desired, &actual);
    assert!(
        baseline.columns_to_rename.is_empty(),
        "position mismatch blocks heuristic rename"
    );

    let hinted = compute_diff_with_hints(
        &desired,
        &actual,
        &[ColumnRenameHint {
            table: "foo".to_string(),
            from: "legacy_name".to_string(),
            to: "display_name".to_string(),
        }],
    );

    assert_eq!(
        hinted.columns_to_rename,
        vec![(
            "foo".to_string(),
            "legacy_name".to_string(),
            "display_name".to_string()
        )]
    );
    assert!(hinted.columns_to_add.is_empty());
    assert!(hinted.columns_to_drop.is_empty());
}

#[test]
fn changed_index_detected() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    desired.indexes.insert(
        ci("idx_foo"),
        IndexInfo {
            name: "idx_foo".to_string(),
            table_name: "foo".to_string(),
            sql: "CREATE INDEX idx_foo ON foo(a, b)".to_string(),
            is_fts: false,
            is_unique: false,
            columns: vec!["a".to_string(), "b".to_string()],
        },
    );
    actual.indexes.insert(
        ci("idx_foo"),
        IndexInfo {
            name: "idx_foo".to_string(),
            table_name: "foo".to_string(),
            sql: "CREATE INDEX idx_foo ON foo(a)".to_string(),
            is_fts: false,
            is_unique: false,
            columns: vec!["a".to_string()],
        },
    );

    let diff = compute_diff(&desired, &actual);
    assert_eq!(diff.indexes_to_drop, vec!["idx_foo".to_string()]);
    assert_eq!(diff.indexes_to_create, vec!["idx_foo".to_string()]);
    assert!(diff.fts_indexes_to_create.is_empty());
    assert!(diff.fts_indexes_to_drop.is_empty());
}

#[test]
fn fts_index_change_uses_fts_fields() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    desired.indexes.insert(
        ci("idx_docs_fts"),
        IndexInfo {
            name: "idx_docs_fts".to_string(),
            table_name: "documents".to_string(),
            sql: "CREATE INDEX idx_docs_fts ON documents USING fts (title, body_text)".to_string(),
            is_fts: true,
            is_unique: false,
            columns: vec![],
        },
    );
    actual.indexes.insert(
        ci("idx_docs_fts"),
        IndexInfo {
            name: "idx_docs_fts".to_string(),
            table_name: "documents".to_string(),
            sql: "CREATE INDEX idx_docs_fts ON documents USING fts (title)".to_string(),
            is_fts: true,
            is_unique: false,
            columns: vec![],
        },
    );

    let diff = compute_diff(&desired, &actual);
    assert_eq!(diff.fts_indexes_to_drop, vec!["idx_docs_fts".to_string()]);
    assert_eq!(diff.fts_indexes_to_create, vec!["idx_docs_fts".to_string()]);
    assert!(diff.indexes_to_create.is_empty());
    assert!(diff.indexes_to_drop.is_empty());
}

#[test]
fn changed_view_detected() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    desired.views.insert(
        ci("v"),
        ViewInfo {
            name: "v".to_string(),
            sql: "CREATE VIEW v AS SELECT 1, 2".to_string(),
            is_materialized: false,
        },
    );
    actual.views.insert(
        ci("v"),
        ViewInfo {
            name: "v".to_string(),
            sql: "CREATE VIEW v AS SELECT 1".to_string(),
            is_materialized: false,
        },
    );

    let diff = compute_diff(&desired, &actual);
    assert_eq!(diff.views_to_drop, vec!["v".to_string()]);
    assert_eq!(diff.views_to_create, vec!["v".to_string()]);
}

#[tokio::test]
async fn pristine_vs_empty_produces_full_create() {
    let desired = SchemaSnapshot::from_schema_sql(test_schema())
        .await
        .expect("pristine snapshot");
    let actual = empty_snapshot();

    let diff = compute_diff(&desired, &actual);

    assert_eq!(diff.tables_to_create.len(), desired.tables.len());
    assert_eq!(
        diff.indexes_to_create.len(),
        desired.indexes.values().filter(|i| !i.is_fts).count()
    );
    assert_eq!(
        diff.fts_indexes_to_create.len(),
        desired.indexes.values().filter(|i| i.is_fts).count()
    );
    assert_eq!(diff.views_to_create.len(), desired.views.len());
    assert_eq!(diff.triggers_to_create.len(), desired.triggers.len());

    assert!(diff.tables_to_drop.is_empty());
    assert!(diff.tables_to_rebuild.is_empty());
    assert!(diff.columns_to_add.is_empty());
    assert!(diff.indexes_to_drop.is_empty());
    assert!(diff.fts_indexes_to_drop.is_empty());
    assert!(diff.views_to_drop.is_empty());
    assert!(diff.triggers_to_drop.is_empty());
}

#[test]
fn case_insensitive_type_comparison_no_rebuild() {
    let mut desired = empty_snapshot();
    let mut actual = empty_snapshot();

    desired.tables.insert(
        ci("foo"),
        make_table("foo", vec![make_column("val", "INTEGER", false, None, 0)]),
    );
    actual.tables.insert(
        ci("foo"),
        make_table("foo", vec![make_column("val", "integer", false, None, 0)]),
    );

    let diff = compute_diff(&desired, &actual);
    assert!(
        diff.is_empty(),
        "Case difference in type should not trigger rebuild: {diff}"
    );
}

#[test]
fn display_impl_shows_changes() {
    let mut desired = empty_snapshot();
    desired.tables.insert(ci("foo"), make_table("foo", vec![]));
    let actual = empty_snapshot();

    let diff = compute_diff(&desired, &actual);
    let output = diff.to_string();
    assert!(
        output.contains("+ TABLE foo"),
        "Display should show created table: {output}"
    );
}

#[test]
fn display_empty_diff() {
    let snap = empty_snapshot();
    let diff = compute_diff(&snap, &snap);
    assert_eq!(diff.to_string(), "(no changes)");
}

#[test]
fn normalize_preserves_string_literal_case() {
    use turso_converge::diff::normalize_for_hash;
    let sql = "SELECT * FROM t WHERE status = 'Active'";
    let normalized = normalize_for_hash(sql);
    assert!(
        normalized.contains("'Active'"),
        "String literal case should be preserved: {normalized}"
    );
    assert!(
        normalized.contains("select"),
        "Keywords should be lowercased: {normalized}"
    );
}

#[test]
fn normalize_strips_comments() {
    use turso_converge::diff::normalize_for_hash;
    let sql = "CREATE TABLE foo ( -- this is a comment\n  id TEXT PRIMARY KEY\n)";
    let normalized = normalize_for_hash(sql);
    assert!(
        !normalized.contains("comment"),
        "Comments should be stripped: {normalized}"
    );
    assert!(
        normalized.contains("id text primary key"),
        "Content should remain: {normalized}"
    );
}

#[test]
fn normalize_strips_block_comments() {
    use turso_converge::diff::normalize_for_hash;
    let sql = "CREATE TABLE /* multi\nline\ncomment */ foo (id TEXT)";
    let normalized = normalize_for_hash(sql);
    assert!(
        !normalized.contains("multi"),
        "Block comments should be stripped: {normalized}"
    );
}

#[test]
fn normalize_handles_escaped_quotes() {
    use turso_converge::diff::normalize_for_hash;
    let sql = "INSERT INTO t VALUES ('it''s a test')";
    let normalized = normalize_for_hash(sql);
    assert!(
        normalized.contains("'it''s a test'"),
        "Escaped quotes should be preserved: {normalized}"
    );
}
