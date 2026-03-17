use turso_migrate::SchemaSnapshot;

fn test_schema() -> &'static str {
    include_str!("fixtures/schema.sql")
}

async fn pristine_snapshot() -> SchemaSnapshot {
    SchemaSnapshot::from_schema_sql(test_schema())
        .await
        .expect("pristine snapshot")
}

#[tokio::test]
async fn snapshot_contains_all_tables() {
    let snap = pristine_snapshot().await;
    let expected = [
        "documents",
        "document_tags",
        "links",
        "entities",
        "entity_document_links",
        "entity_links",
        "cache_manifest",
        "sync_state",
        "settings",
        "schema_version",
    ];
    assert_eq!(snap.tables.len(), expected.len());
    for name in expected {
        assert!(snap.has_table(name), "Missing table: {}", name);
    }
}

#[tokio::test]
async fn snapshot_captures_columns() {
    let snap = pristine_snapshot().await;
    let docs = snap.get_table("documents").expect("documents table");
    assert!(
        docs.columns.len() >= 14,
        "documents should have at least 14 columns, got {}",
        docs.columns.len()
    );

    let title = docs
        .columns
        .iter()
        .find(|c| c.name == "title")
        .expect("title column");
    assert!(title.notnull, "title should be NOT NULL");

    let source_url = docs
        .columns
        .iter()
        .find(|c| c.name == "source_url")
        .expect("source_url column");
    assert!(!source_url.notnull, "source_url should be nullable");

    let content_type = docs
        .columns
        .iter()
        .find(|c| c.name == "content_type")
        .expect("content_type");
    assert!(
        content_type.default_value.is_some(),
        "content_type should have DEFAULT"
    );
}

#[tokio::test]
async fn snapshot_captures_vector_columns() {
    let snap = pristine_snapshot().await;
    let docs = snap.get_table("documents").expect("documents table");
    let embedding = docs
        .columns
        .iter()
        .find(|c| c.name == "embedding")
        .expect("embedding column");
    assert!(
        !embedding.col_type.is_empty(),
        "embedding col_type should not be empty"
    );
}

#[tokio::test]
async fn snapshot_captures_standard_indexes() {
    let snap = pristine_snapshot().await;
    let standard: Vec<_> = snap.indexes.values().filter(|i| !i.is_fts).collect();
    assert!(
        standard.len() >= 7,
        "Should have at least 7 standard indexes, got {}",
        standard.len()
    );
    assert!(snap.has_index("idx_docs_workspace"));
    assert!(snap.has_index("idx_tags_tag"));
}

#[tokio::test]
async fn snapshot_detects_fts_indexes() {
    let snap = pristine_snapshot().await;
    let fts: Vec<_> = snap.indexes.values().filter(|i| i.is_fts).collect();
    assert_eq!(fts.len(), 2, "Should have exactly 2 FTS indexes");

    let docs_fts = snap.get_index("idx_docs_fts").expect("idx_docs_fts");
    assert!(docs_fts.is_fts, "idx_docs_fts should be FTS");
    assert_eq!(docs_fts.table_name, "documents");
}

#[tokio::test]
async fn snapshot_captures_materialized_views() {
    let snap = pristine_snapshot().await;
    assert!(snap.views.len() >= 4, "Should have at least 4 views");

    let mv = snap.get_view("mv_type_counts").expect("mv_type_counts");
    assert!(mv.is_materialized, "mv_type_counts should be materialized");
}

#[tokio::test]
async fn snapshot_filters_internal_objects() {
    let snap = pristine_snapshot().await;
    for name in snap.tables.keys() {
        assert!(
            !name.raw().starts_with("sqlite_"),
            "Should not contain sqlite_ tables"
        );
        assert!(
            !name.raw().starts_with("fts_dir_"),
            "Should not contain fts_dir_ tables"
        );
        assert!(
            !name.raw().starts_with("__turso_internal"),
            "Should not contain __turso_internal tables"
        );
    }
    for name in snap.indexes.keys() {
        assert!(
            !name.raw().starts_with("sqlite_autoindex_"),
            "Should not contain autoindexes"
        );
    }
}

#[tokio::test]
async fn two_pristine_snapshots_are_equal() {
    let snap1 = pristine_snapshot().await;
    let snap2 = pristine_snapshot().await;
    assert_eq!(
        snap1, snap2,
        "Two pristine snapshots from same SQL should be identical"
    );
}

#[tokio::test]
async fn snapshot_captures_foreign_keys() {
    let snap = pristine_snapshot().await;
    let tags = snap.get_table("document_tags").expect("document_tags");
    assert!(
        !tags.foreign_keys.is_empty(),
        "document_tags should have FK to documents"
    );
    assert_eq!(tags.foreign_keys[0].to_table, "documents");
}

#[tokio::test]
async fn snapshot_detects_table_properties() {
    let snap = pristine_snapshot().await;
    let docs = snap.get_table("documents").expect("documents");
    assert!(!docs.is_strict, "documents is not STRICT");
    assert!(!docs.is_without_rowid, "documents is not WITHOUT ROWID");
    assert!(!docs.has_autoincrement, "documents has no AUTOINCREMENT");
}
