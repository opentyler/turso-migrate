mod common;

use turso_converge::SchemaSnapshot;

async fn pristine_snapshot() -> SchemaSnapshot {
    SchemaSnapshot::from_schema_sql(common::test_schema())
        .await
        .expect("pristine snapshot")
}

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_detects_fts_indexes() {
    let snap = pristine_snapshot().await;
    let fts: Vec<_> = snap.indexes.values().filter(|i| i.is_fts).collect();
    assert_eq!(fts.len(), 2, "Should have exactly 2 FTS indexes");

    let docs_fts = snap.get_index("idx_docs_fts").expect("idx_docs_fts");
    assert!(docs_fts.is_fts, "idx_docs_fts should be FTS");
    assert_eq!(docs_fts.table_name, "documents");
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_captures_materialized_views() {
    let snap = pristine_snapshot().await;
    assert!(snap.views.len() >= 4, "Should have at least 4 views");

    let mv = snap.get_view("mv_type_counts").expect("mv_type_counts");
    assert!(mv.is_materialized, "mv_type_counts should be materialized");
}

#[tokio::test(flavor = "multi_thread")]
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

#[tokio::test(flavor = "multi_thread")]
async fn two_pristine_snapshots_are_equal() {
    let snap1 = pristine_snapshot().await;
    let snap2 = pristine_snapshot().await;
    assert_eq!(
        snap1, snap2,
        "Two pristine snapshots from same SQL should be identical"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_captures_foreign_keys() {
    let snap = pristine_snapshot().await;
    let tags = snap.get_table("document_tags").expect("document_tags");
    assert!(
        !tags.foreign_keys.is_empty(),
        "document_tags should have FK to documents"
    );
    assert_eq!(tags.foreign_keys[0].to_table, "documents");
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_detects_table_properties() {
    let snap = pristine_snapshot().await;
    let docs = snap.get_table("documents").expect("documents");
    assert!(!docs.is_strict, "documents is not STRICT");
    assert!(!docs.is_without_rowid, "documents is not WITHOUT ROWID");
    assert!(!docs.has_autoincrement, "documents has no AUTOINCREMENT");
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_cache_reuses_normalized_schema_hash() {
    SchemaSnapshot::clear_snapshot_cache_for_tests();
    assert_eq!(SchemaSnapshot::snapshot_cache_len_for_tests(), 0);

    let formatted = format!("\n\n{}\n", common::test_schema());
    let commented = format!(
        "-- same schema with comment\n{}\n-- trailing",
        common::test_schema()
    );

    let _ = SchemaSnapshot::from_schema_sql(&formatted).await.unwrap();
    let len1 = SchemaSnapshot::snapshot_cache_len_for_tests();
    assert_eq!(len1, 1, "first schema load should fill cache");

    let _ = SchemaSnapshot::from_schema_sql(&commented).await.unwrap();
    let len2 = SchemaSnapshot::snapshot_cache_len_for_tests();
    assert_eq!(
        len2, 1,
        "format/comment-only variants should reuse the same cache entry"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_filters_capability_probe_artifacts() {
    let db = turso::Builder::new_local(":memory:")
        .experimental_index_method(true)
        .experimental_materialized_views(true)
        .experimental_triggers(true)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();

    conn.execute("CREATE TABLE _cap_probe_fts (x TEXT)", ())
        .await
        .unwrap();
    conn.execute("CREATE TABLE _cap_probe_vec (x TEXT)", ())
        .await
        .unwrap();

    let snap = SchemaSnapshot::from_connection(&conn).await.unwrap();
    assert!(
        !snap.has_table("_cap_probe_fts"),
        "_cap_probe_fts must be hidden from snapshots"
    );
    assert!(
        !snap.has_table("_cap_probe_vec"),
        "_cap_probe_vec must be hidden from snapshots"
    );
}
