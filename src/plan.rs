use std::collections::{BTreeSet, HashSet};

use crate::{ColumnInfo, SchemaDiff, SchemaSnapshot};

#[derive(Debug, Clone)]
pub struct MigrationPlan {
    pub new_tables: Vec<String>,
    pub altered_tables: Vec<String>,
    pub rebuilt_tables: Vec<String>,
    pub new_indexes: Vec<String>,
    pub changed_indexes: Vec<String>,
    pub new_views: Vec<String>,
    pub changed_views: Vec<String>,
    pub transactional_stmts: Vec<String>,
    pub non_transactional_stmts: Vec<String>,
}

impl MigrationPlan {
    pub fn is_empty(&self) -> bool {
        self.transactional_stmts.is_empty() && self.non_transactional_stmts.is_empty()
    }
}

pub fn generate_plan(
    diff: &SchemaDiff,
    desired: &SchemaSnapshot,
    actual: &SchemaSnapshot,
) -> MigrationPlan {
    let rebuilt_tables: BTreeSet<String> = diff.tables_to_rebuild.iter().cloned().collect();

    let altered_tables: BTreeSet<String> =
        diff.columns_to_add.iter().map(|(t, _)| t.clone()).collect();

    let mut views_to_drop: BTreeSet<String> = diff.views_to_drop.iter().cloned().collect();
    let mut views_to_create: BTreeSet<String> = diff.views_to_create.iter().cloned().collect();

    for (name, view) in &actual.views {
        if rebuilt_tables
            .iter()
            .any(|table| view_depends_on_table(&view.sql, table))
        {
            views_to_drop.insert(name.clone());
        }
    }
    for (name, view) in &desired.views {
        if rebuilt_tables
            .iter()
            .any(|table| view_depends_on_table(&view.sql, table))
        {
            views_to_create.insert(name.clone());
        }
    }

    let mut triggers_to_drop: BTreeSet<String> = diff.triggers_to_drop.iter().cloned().collect();
    for (name, trigger) in &actual.triggers {
        if rebuilt_tables.contains(&trigger.table_name) {
            triggers_to_drop.insert(name.clone());
        }
    }

    let mut triggers_to_create: BTreeSet<String> =
        diff.triggers_to_create.iter().cloned().collect();
    for (name, trigger) in &desired.triggers {
        if rebuilt_tables.contains(&trigger.table_name) {
            triggers_to_create.insert(name.clone());
        }
    }

    let mut indexes_to_drop: BTreeSet<String> = diff.indexes_to_drop.iter().cloned().collect();
    for (name, idx) in &actual.indexes {
        if !idx.is_fts && rebuilt_tables.contains(&idx.table_name) {
            indexes_to_drop.insert(name.clone());
        }
    }

    let mut indexes_to_create: BTreeSet<String> = diff.indexes_to_create.iter().cloned().collect();
    for (name, idx) in &desired.indexes {
        if !idx.is_fts && rebuilt_tables.contains(&idx.table_name) {
            indexes_to_create.insert(name.clone());
        }
    }

    let mut fts_indexes_to_drop: BTreeSet<String> =
        diff.fts_indexes_to_drop.iter().cloned().collect();
    for (name, idx) in &actual.indexes {
        if idx.is_fts && rebuilt_tables.contains(&idx.table_name) {
            fts_indexes_to_drop.insert(name.clone());
        }
    }

    let mut fts_indexes_to_create: BTreeSet<String> =
        diff.fts_indexes_to_create.iter().cloned().collect();
    for (name, idx) in &desired.indexes {
        if idx.is_fts && rebuilt_tables.contains(&idx.table_name) {
            fts_indexes_to_create.insert(name.clone());
        }
    }

    let mut transactional_stmts = Vec::new();

    for trigger_name in &triggers_to_drop {
        transactional_stmts.push(format!(
            "DROP TRIGGER IF EXISTS {}",
            quote_ident(trigger_name)
        ));
    }

    for view_name in &views_to_drop {
        if actual.views.contains_key(view_name) {
            transactional_stmts.push(format!("DROP VIEW IF EXISTS {}", quote_ident(view_name)));
        }
    }

    for idx_name in &indexes_to_drop {
        transactional_stmts.push(format!("DROP INDEX IF EXISTS {}", quote_ident(idx_name)));
    }

    for table_name in &diff.tables_to_drop {
        transactional_stmts.push(format!("DROP TABLE IF EXISTS {}", quote_ident(table_name)));
    }

    let ordered_new_tables = order_new_tables(&diff.tables_to_create, desired, actual);
    for table_name in &ordered_new_tables {
        if let Some(table) = desired.tables.get(table_name) {
            transactional_stmts.push(table.sql.clone());
        }
    }

    for (table_name, col) in &diff.columns_to_add {
        transactional_stmts.push(build_add_column_stmt(table_name, col));
    }

    for table_name in &diff.tables_to_rebuild {
        if let (Some(desired_table), Some(actual_table)) = (
            desired.tables.get(table_name),
            actual.tables.get(table_name),
        ) {
            let temp_table_name = format!("_converge_new_{table_name}");
            transactional_stmts.push(rewrite_create_table_name(
                &desired_table.sql,
                &temp_table_name,
            ));
            transactional_stmts.push(build_copy_data_stmt(
                table_name,
                &temp_table_name,
                desired_table,
                actual_table,
            ));
            transactional_stmts.push(format!("DROP TABLE {}", quote_ident(table_name)));
            transactional_stmts.push(format!(
                "ALTER TABLE {} RENAME TO {}",
                quote_ident(&temp_table_name),
                quote_ident(table_name)
            ));
        }
    }

    for idx_name in &indexes_to_create {
        if let Some(index) = desired.indexes.get(idx_name) {
            transactional_stmts.push(index.sql.clone());
        }
    }

    for view_name in &views_to_create {
        if let Some(view) = desired.views.get(view_name) {
            transactional_stmts.push(sanitize_view_sql(&view.sql));
        }
    }

    for trigger_name in &triggers_to_create {
        if let Some(trigger) = desired.triggers.get(trigger_name) {
            transactional_stmts.push(trigger.sql.clone());
        }
    }

    let mut non_transactional_stmts = Vec::new();

    for idx_name in &fts_indexes_to_drop {
        non_transactional_stmts.push(format!("DROP INDEX IF EXISTS {}", quote_ident(idx_name)));
    }
    for idx_name in &fts_indexes_to_create {
        if let Some(index) = desired.indexes.get(idx_name) {
            non_transactional_stmts.push(index.sql.clone());
        }
    }

    let new_indexes: BTreeSet<String> = indexes_to_create
        .iter()
        .filter(|name| !actual.indexes.contains_key(*name))
        .cloned()
        .collect();
    let changed_indexes: BTreeSet<String> = indexes_to_create
        .iter()
        .filter(|name| actual.indexes.contains_key(*name))
        .cloned()
        .collect();
    let new_views: BTreeSet<String> = views_to_create
        .iter()
        .filter(|name| !actual.views.contains_key(*name))
        .cloned()
        .collect();
    let changed_views: BTreeSet<String> = views_to_create
        .iter()
        .filter(|name| actual.views.contains_key(*name))
        .cloned()
        .collect();

    MigrationPlan {
        new_tables: ordered_new_tables,
        altered_tables: altered_tables.into_iter().collect(),
        rebuilt_tables: rebuilt_tables.into_iter().collect(),
        new_indexes: new_indexes.into_iter().collect(),
        changed_indexes: changed_indexes.into_iter().collect(),
        new_views: new_views.into_iter().collect(),
        changed_views: changed_views.into_iter().collect(),
        transactional_stmts,
        non_transactional_stmts,
    }
}

fn order_new_tables(
    to_create: &[String],
    desired: &SchemaSnapshot,
    actual: &SchemaSnapshot,
) -> Vec<String> {
    let mut remaining: BTreeSet<String> = to_create.iter().cloned().collect();
    let existing: HashSet<&str> = actual.tables.keys().map(|s| s.as_str()).collect();
    let mut ordered = Vec::new();

    while !remaining.is_empty() {
        let mut progressed = false;
        let candidates: Vec<String> = remaining.iter().cloned().collect();

        for table in candidates {
            let refs = desired
                .tables
                .get(&table)
                .map(|t| referenced_tables(&t.sql))
                .unwrap_or_default();

            let ready = refs
                .into_iter()
                .all(|r| existing.contains(r.as_str()) || !remaining.contains(&r));

            if ready {
                remaining.remove(&table);
                ordered.push(table);
                progressed = true;
            }
        }

        if !progressed {
            ordered.extend(remaining.into_iter());
            break;
        }
    }

    ordered
}

pub(crate) fn referenced_tables(table_sql: &str) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    let bytes = table_sql.as_bytes();
    let lower = table_sql.to_lowercase();
    let lower_bytes = lower.as_bytes();
    let mut idx = 0;

    while idx < lower_bytes.len() {
        if lower_bytes[idx..].starts_with(b"references") {
            idx += "references".len();
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
                refs.insert(name);
            }
        } else {
            idx += 1;
        }
    }

    refs
}

fn build_add_column_stmt(table_name: &str, col: &ColumnInfo) -> String {
    let mut ddl = format!(
        "ALTER TABLE {} ADD COLUMN {} {}",
        quote_ident(table_name),
        quote_ident(&col.name),
        col.col_type
    );
    if col.notnull {
        ddl.push_str(" NOT NULL");
    }
    if let Some(default) = &col.default_value {
        ddl.push_str(&format!(" DEFAULT {default}"));
    }
    ddl
}

fn build_copy_data_stmt(
    source_table: &str,
    temp_table: &str,
    desired_table: &crate::TableInfo,
    actual_table: &crate::TableInfo,
) -> String {
    let actual_cols: HashSet<&str> = actual_table
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let shared_cols: Vec<&str> = desired_table
        .columns
        .iter()
        .filter(|c| actual_cols.contains(c.name.as_str()))
        .map(|c| c.name.as_str())
        .collect();

    let cols = shared_cols
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "INSERT INTO {} ({cols}) SELECT {cols} FROM {}",
        quote_ident(temp_table),
        quote_ident(source_table)
    )
}

fn rewrite_create_table_name(create_sql: &str, temp_table_name: &str) -> String {
    let lower = create_sql.to_lowercase();
    let Some(create_pos) = lower.find("create table") else {
        return create_sql.to_string();
    };

    let mut idx = create_pos + "create table".len();
    let bytes = create_sql.as_bytes();
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }

    if idx >= bytes.len() {
        return create_sql.to_string();
    }

    let (start, end) = if bytes[idx] == b'"' {
        let start = idx;
        idx += 1;
        while idx < bytes.len() && bytes[idx] != b'"' {
            idx += 1;
        }
        let end = if idx < bytes.len() { idx + 1 } else { idx };
        (start, end)
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
        (start, idx)
    };

    format!(
        "{}{}{}",
        &create_sql[..start],
        quote_ident(temp_table_name),
        &create_sql[end..]
    )
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn view_depends_on_table(view_sql: &str, table_name: &str) -> bool {
    let table = table_name.to_lowercase();
    view_sql
        .to_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|token| token == table)
}

fn sanitize_view_sql(sql: &str) -> String {
    sql.replace("COUNT (", "COUNT(")
        .replace("AVG (", "AVG(")
        .replace("MIN (", "MIN(")
        .replace("MAX (", "MAX(")
        .replace("count (", "count(")
        .replace("avg (", "avg(")
        .replace("min (", "min(")
        .replace("max (", "max(")
}
