use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use crate::diff::normalize_for_hash;
use crate::schema::{
    CIString, Capabilities, ColumnInfo, ForeignKey, IndexInfo, SchemaSnapshot, TableInfo,
    TriggerInfo, ViewInfo,
};

impl SchemaSnapshot {
    pub async fn from_connection(conn: &turso::Connection) -> Result<Self, turso::Error> {
        let mut tables = BTreeMap::new();
        let mut indexes = BTreeMap::new();
        let mut views = BTreeMap::new();
        let mut triggers = BTreeMap::new();

        let mut table_rows = conn
            .query(
                "SELECT name, sql FROM sqlite_schema \
                 WHERE type = 'table' \
                   AND name NOT LIKE 'sqlite_%' \
                   AND name != '_schema_meta' \
                 ORDER BY name",
                (),
            )
            .await?;

        let mut table_defs = Vec::new();

        while let Some(row) = table_rows.next().await? {
            let name: String = row.get(0)?;
            if is_internal_object(&name) {
                continue;
            }
            let sql: String = row.get(1)?;

            table_defs.push((name, sql));
        }

        let mut batched_columns = table_columns_xinfo_batched(conn).await.unwrap_or_default();

        for (name, sql) in table_defs {
            let mut columns = if let Some(cols) = batched_columns.remove(&name.to_ascii_lowercase())
            {
                cols
            } else {
                table_columns_xinfo(conn, &name).await?
            };
            let mut foreign_keys = table_foreign_keys(conn, &name).await.unwrap_or_default();
            if foreign_keys.is_empty() {
                foreign_keys = parse_fk_references_from_sql(&sql);
            }
            let has_autoincrement = detect_autoincrement(&sql);
            let is_strict = detect_strict(&sql);
            let is_without_rowid = detect_without_rowid(&sql);

            enrich_columns_with_collation(&sql, &mut columns);

            tables.insert(
                CIString::new(&name),
                TableInfo {
                    name,
                    sql,
                    columns,
                    foreign_keys,
                    is_strict,
                    is_without_rowid,
                    has_autoincrement,
                },
            );
        }

        let mut index_rows = conn
            .query(
                "SELECT name, tbl_name, sql FROM sqlite_schema \
                 WHERE type = 'index' AND sql IS NOT NULL \
                 ORDER BY name",
                (),
            )
            .await?;

        while let Some(row) = index_rows.next().await? {
            let name: String = row.get(0)?;
            if is_internal_object(&name) {
                continue;
            }
            let table_name: String = row.get(1)?;
            if is_internal_object(&table_name) {
                continue;
            }
            let sql: String = row.get(2)?;
            let is_fts = sql.to_ascii_lowercase().contains("using fts");

            let (is_unique, index_columns) =
                index_details(conn, &name).await.unwrap_or((false, vec![]));

            indexes.insert(
                CIString::new(&name),
                IndexInfo {
                    name,
                    table_name,
                    sql,
                    is_fts,
                    is_unique,
                    columns: index_columns,
                },
            );
        }

        let mut view_rows = conn
            .query(
                "SELECT name, sql FROM sqlite_schema WHERE type = 'view' ORDER BY name",
                (),
            )
            .await?;

        while let Some(row) = view_rows.next().await? {
            let name: String = row.get(0)?;
            if is_internal_object(&name) {
                continue;
            }
            let sql: String = row.get(1)?;
            let is_materialized = is_materialized_view_sql(&sql);

            views.insert(
                CIString::new(&name),
                ViewInfo {
                    name,
                    sql,
                    is_materialized,
                },
            );
        }

        let mut trigger_rows = conn
            .query(
                "SELECT name, tbl_name, sql FROM sqlite_schema \
                 WHERE type = 'trigger' ORDER BY name",
                (),
            )
            .await?;

        while let Some(row) = trigger_rows.next().await? {
            let name: String = row.get(0)?;
            if is_internal_object(&name) {
                continue;
            }
            let table_name: String = row.get(1)?;
            if is_internal_object(&table_name) {
                continue;
            }
            let sql: String = row.get(2)?;

            triggers.insert(
                CIString::new(&name),
                TriggerInfo {
                    name,
                    table_name,
                    sql,
                },
            );
        }

        Ok(Self {
            tables,
            indexes,
            views,
            triggers,
        })
    }

    pub async fn from_schema_sql(schema_sql: &str) -> Result<Self, turso::Error> {
        let normalized = normalize_for_hash(schema_sql);
        let hash = blake3::hash(normalized.as_bytes()).to_hex().to_string();

        if let Some(cached) = snapshot_cache_get(&hash) {
            return Ok(cached);
        }

        let db = turso::Builder::new_local(":memory:")
            .experimental_index_method(true)
            .experimental_materialized_views(true)
            .experimental_triggers(true)
            .build()
            .await?;
        let conn = db.connect()?;
        conn.execute_batch(schema_sql).await?;
        let snapshot = Self::from_connection(&conn).await?;

        snapshot_cache_put(hash, snapshot.clone());
        Ok(snapshot)
    }

    pub async fn validate(schema_sql: &str) -> Result<(), turso::Error> {
        let db = turso::Builder::new_local(":memory:")
            .experimental_index_method(true)
            .experimental_materialized_views(true)
            .experimental_triggers(true)
            .build()
            .await?;
        let conn = db.connect()?;
        conn.execute_batch(schema_sql).await?;
        Ok(())
    }

    #[doc(hidden)]
    pub fn snapshot_cache_len_for_tests() -> usize {
        let cache = SNAPSHOT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        cache.lock().map(|m| m.len()).unwrap_or(0)
    }

    #[doc(hidden)]
    pub fn clear_snapshot_cache_for_tests() {
        let cache = SNAPSHOT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(mut map) = cache.lock() {
            map.clear();
        }
    }

    pub fn to_sql(&self) -> String {
        let mut sql = String::new();

        let mut remaining: BTreeSet<CIString> = self
            .tables
            .keys()
            .filter(|name| !is_meta_table(name.raw()))
            .cloned()
            .collect();
        let mut ordered_tables = Vec::new();

        while !remaining.is_empty() {
            let mut progressed = false;
            let candidates: Vec<CIString> = remaining.iter().cloned().collect();

            for key in candidates {
                let refs = self
                    .tables
                    .get(&key)
                    .map(|t| t.referenced_tables())
                    .unwrap_or_default();

                let ready = refs
                    .into_iter()
                    .all(|r| !remaining.contains(&CIString::new(&r)));

                if ready {
                    remaining.remove(&key);
                    ordered_tables.push(key);
                    progressed = true;
                }
            }

            if !progressed {
                ordered_tables.extend(remaining.into_iter());
                break;
            }
        }

        for key in &ordered_tables {
            if let Some(table) = self.tables.get(key) {
                sql.push_str(&table.sql);
                sql.push_str(";\n\n");
            }
        }

        for index in self.indexes.values().filter(|idx| !idx.is_fts) {
            sql.push_str(&index.sql);
            sql.push_str(";\n\n");
        }

        for index in self.indexes.values().filter(|idx| idx.is_fts) {
            sql.push_str(&index.sql);
            sql.push_str(";\n\n");
        }

        for view in self.views.values() {
            sql.push_str(&view.sql);
            sql.push_str(";\n\n");
        }

        for trigger in self.triggers.values() {
            sql.push_str(&trigger.sql);
            sql.push_str(";\n\n");
        }

        sql.trim_end().to_string()
    }
}

impl Capabilities {
    pub async fn detect(conn: &turso::Connection) -> Result<Self, turso::Error> {
        let mut rows = conn.query("SELECT sqlite_version()", ()).await?;
        let version_str: String = if let Some(row) = rows.next().await? {
            row.get(0)?
        } else {
            "3.0.0".to_string()
        };

        let (major, minor, patch) = parse_version(&version_str);

        let has_fts = probe_fts(conn).await;
        let has_vector = probe_vector(conn).await;
        let has_materialized = probe_materialized_views(conn).await;
        let has_without_rowid = probe_without_rowid(conn).await;
        let has_generated = probe_generated_columns(conn).await;
        let has_triggers = probe_triggers(conn).await;

        Ok(Self {
            database_version: (major, minor, patch),
            supports_drop_column: (major, minor, patch) >= (3, 35, 0),
            supports_rename_column: (major, minor, patch) >= (3, 25, 0),
            has_fts_module: has_fts,
            has_vector_module: has_vector,
            has_materialized_views: has_materialized,
            supports_without_rowid: has_without_rowid,
            supports_generated_columns: has_generated,
            has_triggers,
        })
    }
}

async fn probe_fts(conn: &turso::Connection) -> bool {
    let setup = conn
        .execute("CREATE TABLE IF NOT EXISTS _cap_probe_fts (x TEXT)", ())
        .await;
    if setup.is_err() {
        return false;
    }
    let result = conn
        .execute_batch("CREATE INDEX _cap_probe_idx ON _cap_probe_fts USING fts (x);")
        .await;
    let _ = conn
        .execute_batch("DROP INDEX IF EXISTS _cap_probe_idx;")
        .await;
    let _ = conn
        .execute("DROP TABLE IF EXISTS _cap_probe_fts", ())
        .await;
    result.is_ok()
}

async fn probe_vector(conn: &turso::Connection) -> bool {
    let result = conn
        .execute(
            "CREATE TABLE IF NOT EXISTS _cap_probe_vec (v vector32(1))",
            (),
        )
        .await;
    let _ = conn
        .execute("DROP TABLE IF EXISTS _cap_probe_vec", ())
        .await;
    result.is_ok()
}

async fn probe_materialized_views(conn: &turso::Connection) -> bool {
    let _ = conn
        .execute("CREATE TABLE IF NOT EXISTS _cap_probe_mv (x TEXT)", ())
        .await;
    let result = conn
        .execute(
            "CREATE MATERIALIZED VIEW IF NOT EXISTS _cap_probe_matview AS SELECT x FROM _cap_probe_mv",
            (),
        )
        .await;
    let _ = conn
        .execute("DROP VIEW IF EXISTS _cap_probe_matview", ())
        .await;
    let _ = conn.execute("DROP TABLE IF EXISTS _cap_probe_mv", ()).await;
    result.is_ok()
}

async fn probe_without_rowid(conn: &turso::Connection) -> bool {
    let result = conn
        .execute(
            "CREATE TABLE IF NOT EXISTS _cap_probe_wr (id INTEGER PRIMARY KEY) WITHOUT ROWID",
            (),
        )
        .await;
    let _ = conn.execute("DROP TABLE IF EXISTS _cap_probe_wr", ()).await;
    result.is_ok()
}

async fn probe_generated_columns(conn: &turso::Connection) -> bool {
    let result = conn
        .execute(
            "CREATE TABLE IF NOT EXISTS _cap_probe_gen (x INTEGER, y INTEGER GENERATED ALWAYS AS (x * 2) STORED)",
            (),
        )
        .await;
    let _ = conn
        .execute("DROP TABLE IF EXISTS _cap_probe_gen", ())
        .await;
    result.is_ok()
}

async fn probe_triggers(conn: &turso::Connection) -> bool {
    let setup = conn
        .execute("CREATE TABLE IF NOT EXISTS _cap_probe_trg (x INTEGER)", ())
        .await;
    if setup.is_err() {
        return false;
    }
    let result = conn
        .execute(
            "CREATE TRIGGER _cap_probe_trigger AFTER INSERT ON _cap_probe_trg BEGIN SELECT 1; END",
            (),
        )
        .await;
    let _ = conn
        .execute("DROP TRIGGER IF EXISTS _cap_probe_trigger", ())
        .await;
    let _ = conn
        .execute("DROP TABLE IF EXISTS _cap_probe_trg", ())
        .await;
    result.is_ok()
}

fn parse_version(s: &str) -> (u32, u32, u32) {
    let parts: Vec<u32> = s
        .split('.')
        .filter_map(|p| p.split('-').next()?.parse().ok())
        .collect();
    (
        parts.first().copied().unwrap_or(3),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    )
}

fn is_meta_table(name: &str) -> bool {
    name.eq_ignore_ascii_case("_schema_meta")
}

fn is_materialized_view_sql(sql: &str) -> bool {
    let tokens: Vec<&str> = sql.split_whitespace().take(3).collect();
    tokens.len() == 3
        && tokens[0].eq_ignore_ascii_case("create")
        && tokens[1].eq_ignore_ascii_case("materialized")
        && tokens[2].eq_ignore_ascii_case("view")
}

fn is_internal_object(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("sqlite_")
        || lower.starts_with("fts_dir_")
        || lower.starts_with("__turso_internal")
        || lower.starts_with("_cap_probe_")
        || lower.starts_with("sqlite_autoindex_")
        || lower == "_schema_meta"
        || lower.starts_with("_converge_new_")
}

async fn table_columns_xinfo(
    conn: &turso::Connection,
    table_name: &str,
) -> Result<Vec<ColumnInfo>, turso::Error> {
    let xinfo_pragma = format!("PRAGMA table_xinfo('{}')", table_name.replace('\'', "''"));
    match conn.query(&xinfo_pragma, ()).await {
        Ok(mut rows) => {
            let mut columns = Vec::new();
            while let Some(row) = rows.next().await? {
                let name: String = row.get(1)?;
                let col_type: String = row.get(2)?;
                let notnull: i64 = row.get(3)?;
                let default_value: Option<String> = row.get(4)?;
                let pk: i64 = row.get(5)?;
                let hidden: i64 = row.get(6).unwrap_or(0);

                let is_generated = hidden == 2 || hidden == 3;
                let is_hidden = hidden == 1;

                columns.push(ColumnInfo {
                    name,
                    col_type,
                    notnull: notnull != 0,
                    default_value,
                    pk,
                    collation: None,
                    is_generated,
                    is_hidden,
                });
            }
            Ok(columns)
        }
        Err(_) => table_columns_fallback(conn, table_name).await,
    }
}

async fn table_columns_xinfo_batched(
    conn: &turso::Connection,
) -> Result<HashMap<String, Vec<ColumnInfo>>, turso::Error> {
    let mut rows = conn
        .query(
            "SELECT m.name, p.name, p.type, p.\"notnull\", p.dflt_value, p.pk, p.hidden \
             FROM sqlite_schema m \
             JOIN pragma_table_xinfo(m.name) p \
             WHERE m.type = 'table' \
               AND m.name NOT LIKE 'sqlite_%' \
               AND m.name != '_schema_meta' \
               AND m.name NOT LIKE '_converge_new_%' \
             ORDER BY m.name, p.cid",
            (),
        )
        .await?;

    let mut out: HashMap<String, Vec<ColumnInfo>> = HashMap::new();
    while let Some(row) = rows.next().await? {
        let table_name: String = row.get(0)?;
        let name: String = row.get(1)?;
        let col_type: String = row.get(2)?;
        let notnull: i64 = row.get(3)?;
        let default_value: Option<String> = row.get(4)?;
        let pk: i64 = row.get(5)?;
        let hidden: i64 = row.get(6).unwrap_or(0);

        let is_generated = hidden == 2 || hidden == 3;
        let is_hidden = hidden == 1;

        out.entry(table_name.to_ascii_lowercase())
            .or_default()
            .push(ColumnInfo {
                name,
                col_type,
                notnull: notnull != 0,
                default_value,
                pk,
                collation: None,
                is_generated,
                is_hidden,
            });
    }

    Ok(out)
}

async fn table_columns_fallback(
    conn: &turso::Connection,
    table_name: &str,
) -> Result<Vec<ColumnInfo>, turso::Error> {
    let pragma = format!("PRAGMA table_info('{}')", table_name.replace('\'', "''"));
    let mut rows = conn.query(&pragma, ()).await?;
    let mut columns = Vec::new();

    while let Some(row) = rows.next().await? {
        let name: String = row.get(1)?;
        let col_type: String = row.get(2)?;
        let notnull: i64 = row.get(3)?;
        let default_value: Option<String> = row.get(4)?;
        let pk: i64 = row.get(5)?;

        columns.push(ColumnInfo {
            name,
            col_type,
            notnull: notnull != 0,
            default_value,
            pk,
            collation: None,
            is_generated: false,
            is_hidden: false,
        });
    }

    Ok(columns)
}

async fn table_foreign_keys(
    conn: &turso::Connection,
    table_name: &str,
) -> Result<Vec<ForeignKey>, turso::Error> {
    let pragma = format!(
        "PRAGMA foreign_key_list('{}')",
        table_name.replace('\'', "''")
    );
    let mut rows = conn.query(&pragma, ()).await?;

    let mut fk_map: BTreeMap<i64, ForeignKey> = BTreeMap::new();

    while let Some(row) = rows.next().await? {
        let id: i64 = row.get(0)?;
        let _seq: i64 = row.get(1)?;
        let to_table: String = row.get(2)?;
        let from_col: String = row.get(3)?;
        let to_col: String = row.get(4)?;
        let on_update: String = row.get(5).unwrap_or_else(|_| "NO ACTION".to_string());
        let on_delete: String = row.get(6).unwrap_or_else(|_| "NO ACTION".to_string());

        fk_map
            .entry(id)
            .and_modify(|fk| {
                fk.from_columns.push(from_col.clone());
                fk.to_columns.push(to_col.clone());
            })
            .or_insert_with(|| ForeignKey {
                from_columns: vec![from_col],
                to_table,
                to_columns: vec![to_col],
                on_delete,
                on_update,
            });
    }

    Ok(fk_map.into_values().collect())
}

async fn index_details(
    conn: &turso::Connection,
    index_name: &str,
) -> Result<(bool, Vec<String>), turso::Error> {
    let pragma = format!("PRAGMA index_info('{}')", index_name.replace('\'', "''"));
    let mut rows = conn.query(&pragma, ()).await?;
    let mut columns = Vec::new();

    while let Some(row) = rows.next().await? {
        let col_name: String = row.get(2)?;
        columns.push(col_name);
    }

    let pragma2 = format!("PRAGMA index_list('{}')", index_name.replace('\'', "''"));
    let mut list_rows = conn.query(&pragma2, ()).await;
    let is_unique = match &mut list_rows {
        Ok(rows) => {
            let mut found = false;
            while let Ok(Some(row)) = rows.next().await {
                let name: String = row.get(1).unwrap_or_default();
                if name == index_name {
                    let u: i64 = row.get(2).unwrap_or(0);
                    found = u != 0;
                    break;
                }
            }
            found
        }
        Err(_) => false,
    };

    Ok((is_unique, columns))
}

fn parse_fk_references_from_sql(table_sql: &str) -> Vec<ForeignKey> {
    let mut refs = Vec::new();
    let bytes = table_sql.as_bytes();
    let lower = table_sql.to_lowercase();
    let lower_bytes = lower.as_bytes();
    let mut idx = 0;

    while idx < lower_bytes.len() {
        if bytes[idx] == b'\'' {
            idx += 1;
            while idx < bytes.len() && bytes[idx] != b'\'' {
                idx += 1;
            }
            if idx < bytes.len() {
                idx += 1;
            }
            continue;
        }

        if idx + 1 < bytes.len() && bytes[idx] == b'-' && bytes[idx + 1] == b'-' {
            while idx < bytes.len() && bytes[idx] != b'\n' {
                idx += 1;
            }
            continue;
        }

        if idx + 1 < bytes.len() && bytes[idx] == b'/' && bytes[idx + 1] == b'*' {
            idx += 2;
            while idx + 1 < bytes.len() && !(bytes[idx] == b'*' && bytes[idx + 1] == b'/') {
                idx += 1;
            }
            if idx + 1 < bytes.len() {
                idx += 2;
            }
            continue;
        }

        if lower_bytes[idx..].starts_with(b"references") {
            let before = if idx > 0 { bytes[idx - 1] } else { b' ' };
            let after_pos = idx + "references".len();
            let after = if after_pos < bytes.len() {
                bytes[after_pos]
            } else {
                b' '
            };
            if (before.is_ascii_alphanumeric() || before == b'_')
                || (after.is_ascii_alphanumeric() || after == b'_')
            {
                idx += 1;
                continue;
            }

            idx = after_pos;
            while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
                idx += 1;
            }
            if idx >= bytes.len() {
                break;
            }

            let name = if bytes[idx] == b'"' {
                idx += 1;
                let start = idx;
                while idx < bytes.len() && bytes[idx] != b'"' {
                    idx += 1;
                }
                let val = table_sql[start..idx].to_string();
                if idx < bytes.len() {
                    idx += 1;
                }
                val
            } else {
                let start = idx;
                while idx < bytes.len() {
                    let ch = bytes[idx] as char;
                    if ch.is_ascii_alphanumeric() || ch == '_' {
                        idx += 1;
                    } else {
                        break;
                    }
                }
                table_sql[start..idx].to_string()
            };

            if !name.is_empty() {
                refs.push(ForeignKey {
                    from_columns: vec![],
                    to_table: name,
                    to_columns: vec![],
                    on_delete: "NO ACTION".to_string(),
                    on_update: "NO ACTION".to_string(),
                });
            }
        } else {
            idx += 1;
        }
    }

    refs
}

fn detect_autoincrement(create_sql: &str) -> bool {
    create_sql.to_ascii_lowercase().contains("autoincrement")
}

fn detect_strict(create_sql: &str) -> bool {
    let lower = create_sql.trim().to_ascii_lowercase();
    lower.ends_with(") strict")
        || lower.ends_with(") strict;")
        || lower.ends_with(")strict")
        || lower.ends_with(")strict;")
        || lower.contains(") strict,")
}

fn detect_without_rowid(create_sql: &str) -> bool {
    create_sql.to_ascii_lowercase().contains("without rowid")
}

fn enrich_columns_with_collation(create_sql: &str, columns: &mut [ColumnInfo]) {
    let lower = create_sql.to_ascii_lowercase();

    for col in columns.iter_mut() {
        let col_lower = col.name.to_ascii_lowercase();
        let search_quoted = format!("\"{}\"", col_lower);

        let relevant_section = find_column_section(&lower, &search_quoted)
            .or_else(|| find_column_section(&lower, &col_lower));

        if let Some(section) = relevant_section {
            if let Some(pos) = section.find("collate") {
                let after = section[pos + "collate".len()..].trim_start();
                let collation: String = after
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !collation.is_empty() {
                    col.collation = Some(collation.to_ascii_uppercase());
                }
            }
        }
    }
}

fn find_column_section(lower_sql: &str, col_pattern: &str) -> Option<String> {
    let pos = lower_sql.find(col_pattern)?;
    let after = &lower_sql[pos..];
    let end = after.find(',').or_else(|| after.find(')'))?;
    Some(after[..end].to_string())
}

static SNAPSHOT_CACHE: OnceLock<Mutex<HashMap<String, Arc<SchemaSnapshot>>>> = OnceLock::new();

fn snapshot_cache_get(hash: &str) -> Option<SchemaSnapshot> {
    let cache = SNAPSHOT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let map = cache.lock().ok()?;
    map.get(hash).map(|arc| (**arc).clone())
}

fn snapshot_cache_put(hash: String, snapshot: SchemaSnapshot) {
    let cache = SNAPSHOT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = cache.lock() {
        if map.len() > 16 {
            map.clear();
        }
        map.insert(hash, Arc::new(snapshot));
    }
}

#[cfg(test)]
mod tests {
    use super::parse_fk_references_from_sql;

    #[test]
    fn parse_fk_references_skips_literals_and_comments() {
        let sql = r#"
            CREATE TABLE child (
                id TEXT PRIMARY KEY,
                note TEXT DEFAULT 'this references fake_table(id)',
                -- REFERENCES comment_table(id)
                parent_id TEXT REFERENCES parent(id),
                other_parent TEXT REFERENCES "parent_two"(id),
                blocky TEXT /* REFERENCES block_comment_table(id) */
            );
        "#;

        let refs = parse_fk_references_from_sql(sql);
        let tables: Vec<String> = refs.into_iter().map(|fk| fk.to_table).collect();

        assert_eq!(tables, vec!["parent".to_string(), "parent_two".to_string()]);
    }

    #[test]
    fn parse_fk_references_enforces_word_boundaries() {
        let sql = r#"
            CREATE TABLE child (
                id TEXT PRIMARY KEY,
                references_flag TEXT,
                x TEXT REFERENCES parent(id),
                y TEXT notreferencesz
            );
        "#;

        let refs = parse_fk_references_from_sql(sql);
        let tables: Vec<String> = refs.into_iter().map(|fk| fk.to_table).collect();

        assert_eq!(tables, vec!["parent".to_string()]);
    }
}
