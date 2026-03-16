use std::collections::{BTreeMap, BTreeSet};

use crate::plan::referenced_tables;

#[derive(Debug, Clone, PartialEq)]
pub struct SchemaSnapshot {
    pub tables: BTreeMap<String, TableInfo>,
    pub indexes: BTreeMap<String, IndexInfo>,
    pub views: BTreeMap<String, ViewInfo>,
    pub triggers: BTreeMap<String, TriggerInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableInfo {
    pub name: String,
    pub sql: String,
    pub columns: Vec<ColumnInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnInfo {
    pub name: String,
    pub col_type: String,
    pub notnull: bool,
    pub default_value: Option<String>,
    pub pk: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexInfo {
    pub name: String,
    pub table_name: String,
    pub sql: String,
    pub is_fts: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewInfo {
    pub name: String,
    pub sql: String,
    pub is_materialized: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TriggerInfo {
    pub name: String,
    pub table_name: String,
    pub sql: String,
}

impl SchemaSnapshot {
    pub async fn from_connection(conn: &turso::Connection) -> Result<Self, turso::Error> {
        let mut tables = BTreeMap::new();
        let mut indexes = BTreeMap::new();
        let mut views = BTreeMap::new();
        let mut triggers = BTreeMap::new();

        let mut table_rows = conn
            .query(
                "SELECT name, sql FROM sqlite_schema
WHERE type = 'table'
  AND name NOT LIKE 'sqlite_%'
  AND name NOT IN ('_schema_meta', '_turso_migrations')
ORDER BY name",
                (),
            )
            .await?;

        while let Some(row) = table_rows.next().await? {
            let name: String = row.get(0)?;
            if is_internal_object(&name) || !is_safe_identifier(&name) {
                continue;
            }
            let sql: String = row.get(1)?;
            let columns = table_columns(conn, &name).await?;

            tables.insert(name.clone(), TableInfo { name, sql, columns });
        }

        let mut index_rows = conn
            .query(
                "SELECT name, tbl_name, sql FROM sqlite_schema
WHERE type = 'index'
  AND sql IS NOT NULL
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
            let is_fts = sql.to_lowercase().contains("using fts");

            indexes.insert(
                name.clone(),
                IndexInfo {
                    name,
                    table_name,
                    sql,
                    is_fts,
                },
            );
        }

        let mut view_rows = conn
            .query(
                "SELECT name, sql FROM sqlite_schema
WHERE type = 'view'
ORDER BY name",
                (),
            )
            .await?;

        while let Some(row) = view_rows.next().await? {
            let name: String = row.get(0)?;
            if is_internal_object(&name) {
                continue;
            }
            let sql: String = row.get(1)?;
            let is_materialized = sql.to_lowercase().contains("materialized");

            views.insert(
                name.clone(),
                ViewInfo {
                    name,
                    sql,
                    is_materialized,
                },
            );
        }

        let mut trigger_rows = conn
            .query(
                "SELECT name, tbl_name, sql FROM sqlite_schema
WHERE type = 'trigger'
ORDER BY name",
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
                name.clone(),
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
        let db = turso::Builder::new_local(":memory:")
            .experimental_index_method(true)
            .experimental_materialized_views(true)
            .build()
            .await?;
        let conn = db.connect()?;
        conn.execute_batch(schema_sql).await?;
        Self::from_connection(&conn).await
    }

    pub fn to_sql(&self) -> String {
        let mut sql = String::new();

        let mut remaining: BTreeSet<String> = self
            .tables
            .keys()
            .filter(|name| !is_meta_table(name))
            .cloned()
            .collect();
        let mut ordered_tables = Vec::new();

        while !remaining.is_empty() {
            let mut progressed = false;
            let candidates: Vec<String> = remaining.iter().cloned().collect();

            for table in candidates {
                let refs = self
                    .tables
                    .get(&table)
                    .map(|t| referenced_tables(&t.sql))
                    .unwrap_or_default();

                let ready = refs.into_iter().all(|r| !remaining.contains(&r));

                if ready {
                    remaining.remove(&table);
                    ordered_tables.push(table);
                    progressed = true;
                }
            }

            if !progressed {
                ordered_tables.extend(remaining.iter().cloned());
                break;
            }
        }

        for table_name in ordered_tables {
            if let Some(table) = self.tables.get(&table_name) {
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

fn is_meta_table(name: &str) -> bool {
    name == "_schema_meta" || name == "_turso_migrations" || name == "schema_version"
}

fn is_internal_object(name: &str) -> bool {
    name.starts_with("sqlite_")
        || name.starts_with("fts_dir_")
        || name.starts_with("__turso_internal")
        || name.starts_with("sqlite_autoindex_")
        || name == "_schema_meta"
        || name == "_turso_migrations"
}

fn is_safe_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

async fn table_columns(
    conn: &turso::Connection,
    table_name: &str,
) -> Result<Vec<ColumnInfo>, turso::Error> {
    let pragma = format!("PRAGMA table_info('{table_name}')");
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
        });
    }

    Ok(columns)
}
