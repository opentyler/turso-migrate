-- turso_schema.sql: Canonical schema for per-user Turso databases
-- This is the single source of truth. Edit this file to change the schema.
-- The turso-migrate crate will automatically converge live databases to match.

------------------------------------------------------------
-- Tables
------------------------------------------------------------

CREATE TABLE documents (
    doc_id          TEXT PRIMARY KEY,
    title           TEXT NOT NULL,
    body_text       TEXT NOT NULL,
    source_url      TEXT,
    source_html     BLOB,
    author_id       TEXT NOT NULL,
    workspace_id    TEXT NOT NULL,
    content_type    TEXT DEFAULT 'article',
    confidence      INTEGER DEFAULT 50,
    importance      INTEGER DEFAULT 50,
    quality         INTEGER DEFAULT 50,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    is_deleted      INTEGER DEFAULT 0,
    embedding       vector32(768)
);

CREATE TABLE document_tags (
    doc_id      TEXT NOT NULL REFERENCES documents(doc_id),
    tag         TEXT NOT NULL,
    PRIMARY KEY (doc_id, tag)
);

CREATE TABLE links (
    from_doc    TEXT NOT NULL,
    to_doc      TEXT NOT NULL,
    link_type   TEXT NOT NULL,
    weight      REAL DEFAULT 1.0,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (from_doc, to_doc, link_type)
);

CREATE TABLE entities (
    entity_id   TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    description TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    embedding   vector32(768)
);

CREATE TABLE entity_document_links (
    entity_id   TEXT NOT NULL REFERENCES entities(entity_id),
    doc_id      TEXT NOT NULL REFERENCES documents(doc_id),
    relation    TEXT NOT NULL,
    PRIMARY KEY (entity_id, doc_id, relation)
);

CREATE TABLE entity_links (
    from_entity TEXT NOT NULL REFERENCES entities(entity_id),
    to_entity   TEXT NOT NULL REFERENCES entities(entity_id),
    relation    TEXT NOT NULL,
    weight      REAL DEFAULT 1.0,
    PRIMARY KEY (from_entity, to_entity, relation)
);

CREATE TABLE cache_manifest (
    doc_id          TEXT NOT NULL,
    source_user_id  TEXT NOT NULL,
    cached_at       TEXT NOT NULL,
    expires_at      TEXT NOT NULL,
    access_revoked  INTEGER DEFAULT 0,
    PRIMARY KEY (doc_id, source_user_id)
);

CREATE TABLE sync_state (
    key   TEXT PRIMARY KEY,
    value TEXT
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT
);

CREATE TABLE schema_version (
    version     INTEGER NOT NULL,
    updated_at  TEXT NOT NULL
);

------------------------------------------------------------
-- Standard Indexes
------------------------------------------------------------

CREATE INDEX idx_docs_workspace ON documents(workspace_id);
CREATE INDEX idx_docs_updated ON documents(updated_at);
CREATE INDEX idx_docs_type ON documents(content_type);
CREATE INDEX idx_docs_deleted ON documents(is_deleted);
CREATE INDEX idx_tags_tag ON document_tags(tag);
CREATE INDEX idx_links_from ON links(from_doc);
CREATE INDEX idx_links_to ON links(to_doc);

------------------------------------------------------------
-- FTS Indexes (Turso-specific, requires experimental_index_method)
------------------------------------------------------------

CREATE INDEX idx_docs_fts ON documents USING fts (title, body_text) WITH (weights = 'title=2.0,body_text=1.0');
CREATE INDEX idx_entities_fts ON entities USING fts (name, description) WITH (weights = 'name=3.0,description=1.0');

------------------------------------------------------------
-- Materialized Views (Turso-specific, requires experimental_materialized_views)
------------------------------------------------------------

CREATE MATERIALIZED VIEW mv_type_counts AS
SELECT content_type, COUNT(*) as doc_count
FROM documents
WHERE is_deleted = 0
GROUP BY content_type;

CREATE MATERIALIZED VIEW mv_workspace_counts AS
SELECT workspace_id, COUNT(*) as doc_count
FROM documents
WHERE is_deleted = 0
GROUP BY workspace_id;

CREATE MATERIALIZED VIEW mv_tag_counts AS
SELECT dt.tag, COUNT(*) as doc_count
FROM document_tags dt
JOIN documents d ON dt.doc_id = d.doc_id
WHERE d.is_deleted = 0
GROUP BY dt.tag;

CREATE MATERIALIZED VIEW mv_doc_stats AS
SELECT
    COUNT(*) as total_docs,
    AVG(confidence) as avg_confidence,
    AVG(importance) as avg_importance,
    AVG(quality) as avg_quality,
    MIN(created_at) as earliest_doc,
    MAX(created_at) as latest_doc
FROM documents
WHERE is_deleted = 0;
