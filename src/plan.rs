use std::collections::{BTreeSet, HashSet};

use crate::diff::SchemaDiff;
use crate::error::MigrateError;
use crate::schema::{ColumnInfo, SchemaSnapshot};

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
) -> Result<MigrationPlan, MigrateError> {
    let rebuilt_tables: BTreeSet<String> = diff.tables_to_rebuild.iter().cloned().collect();
    let rebuilt_lower: HashSet<String> = rebuilt_tables
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();

    let altered_tables: BTreeSet<String> =
        diff.columns_to_add.iter().map(|(t, _)| t.clone()).collect();

    let mut views_to_drop: BTreeSet<String> = diff.views_to_drop.iter().cloned().collect();
    let mut views_to_create: BTreeSet<String> = diff.views_to_create.iter().cloned().collect();

    for (name, view) in &actual.views {
        if rebuilt_lower
            .iter()
            .any(|table| view_depends_on_table(&view.sql, table))
        {
            views_to_drop.insert(name.raw().to_string());
        }
    }
    for (name, view) in &desired.views {
        if rebuilt_lower
            .iter()
            .any(|table| view_depends_on_table(&view.sql, table))
        {
            views_to_create.insert(name.raw().to_string());
        }
    }

    let mut triggers_to_drop: BTreeSet<String> = diff.triggers_to_drop.iter().cloned().collect();
    for trigger in actual.triggers.values() {
        if rebuilt_lower.contains(&trigger.table_name.to_ascii_lowercase()) {
            triggers_to_drop.insert(trigger.name.clone());
        }
    }

    let mut triggers_to_create: BTreeSet<String> =
        diff.triggers_to_create.iter().cloned().collect();
    for trigger in desired.triggers.values() {
        if rebuilt_lower.contains(&trigger.table_name.to_ascii_lowercase()) {
            triggers_to_create.insert(trigger.name.clone());
        }
    }

    let mut indexes_to_drop: BTreeSet<String> = diff.indexes_to_drop.iter().cloned().collect();
    for idx in actual.indexes.values() {
        if !idx.is_fts && rebuilt_lower.contains(&idx.table_name.to_ascii_lowercase()) {
            indexes_to_drop.insert(idx.name.clone());
        }
    }

    let mut indexes_to_create: BTreeSet<String> = diff.indexes_to_create.iter().cloned().collect();
    for idx in desired.indexes.values() {
        if !idx.is_fts && rebuilt_lower.contains(&idx.table_name.to_ascii_lowercase()) {
            indexes_to_create.insert(idx.name.clone());
        }
    }

    let mut fts_indexes_to_drop: BTreeSet<String> =
        diff.fts_indexes_to_drop.iter().cloned().collect();
    for idx in actual.indexes.values() {
        if idx.is_fts && rebuilt_lower.contains(&idx.table_name.to_ascii_lowercase()) {
            fts_indexes_to_drop.insert(idx.name.clone());
        }
    }

    let mut fts_indexes_to_create: BTreeSet<String> =
        diff.fts_indexes_to_create.iter().cloned().collect();
    for idx in desired.indexes.values() {
        if idx.is_fts && rebuilt_lower.contains(&idx.table_name.to_ascii_lowercase()) {
            fts_indexes_to_create.insert(idx.name.clone());
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
        if actual.has_view(view_name) {
            transactional_stmts.push(format!("DROP VIEW IF EXISTS {}", quote_ident(view_name)));
        }
    }

    for idx_name in &indexes_to_drop {
        transactional_stmts.push(format!("DROP INDEX IF EXISTS {}", quote_ident(idx_name)));
    }

    for table_name in &diff.tables_to_drop {
        if is_protected_table(table_name) {
            continue;
        }
        transactional_stmts.push(format!("DROP TABLE IF EXISTS {}", quote_ident(table_name)));
    }

    let ordered_new_tables = order_new_tables(&diff.tables_to_create, desired, actual);
    for table_name in &ordered_new_tables {
        if let Some(table) = desired.get_table(table_name) {
            transactional_stmts.push(table.sql.clone());
        }
    }

    for (table_name, col) in &diff.columns_to_add {
        transactional_stmts.push(build_add_column_stmt(table_name, col));
    }

    for table_name in &diff.tables_to_rebuild {
        if let (Some(desired_table), Some(actual_table)) =
            (desired.get_table(table_name), actual.get_table(table_name))
        {
            validate_rebuild_safety(table_name, desired_table, actual_table)?;

            let temp_table_name = format!("_converge_new_{table_name}");

            if actual_table.has_autoincrement {
                transactional_stmts.push(format!(
                    "INSERT OR REPLACE INTO _schema_meta (key, value) \
                     SELECT 'autoincrement_seq_{}', seq FROM sqlite_sequence WHERE name = '{}'",
                    table_name.replace('\'', "''"),
                    table_name.replace('\'', "''")
                ));
            }

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

            if desired_table.has_autoincrement {
                transactional_stmts.push(format!(
                    "INSERT OR REPLACE INTO sqlite_sequence (name, seq) \
                     SELECT '{}', CAST(value AS INTEGER) FROM _schema_meta \
                     WHERE key = 'autoincrement_seq_{}'",
                    table_name.replace('\'', "''"),
                    table_name.replace('\'', "''")
                ));
                transactional_stmts.push(format!(
                    "DELETE FROM _schema_meta WHERE key = 'autoincrement_seq_{}'",
                    table_name.replace('\'', "''")
                ));
            }
        }
    }

    for idx_name in &indexes_to_create {
        if let Some(index) = desired.get_index(idx_name) {
            transactional_stmts.push(index.sql.clone());
        }
    }

    let ordered_views = order_view_creation(&views_to_create, desired);
    for view_name in &ordered_views {
        if let Some(view) = desired.get_view(view_name) {
            transactional_stmts.push(view.sql.clone());
        }
    }

    for trigger_name in &triggers_to_create {
        if let Some(trigger) = desired.get_trigger(trigger_name) {
            transactional_stmts.push(trigger.sql.clone());
        }
    }

    let mut non_transactional_stmts = Vec::new();

    for idx_name in &fts_indexes_to_drop {
        non_transactional_stmts.push(format!("DROP INDEX IF EXISTS {}", quote_ident(idx_name)));
    }
    for idx_name in &fts_indexes_to_create {
        if let Some(index) = desired.get_index(idx_name) {
            non_transactional_stmts.push(index.sql.clone());
        }
    }

    let new_indexes: BTreeSet<String> = indexes_to_create
        .iter()
        .filter(|name| !actual.has_index(name))
        .cloned()
        .collect();
    let changed_indexes: BTreeSet<String> = indexes_to_create
        .iter()
        .filter(|name| actual.has_index(name))
        .cloned()
        .collect();
    let new_views: BTreeSet<String> = views_to_create
        .iter()
        .filter(|name| !actual.has_view(name))
        .cloned()
        .collect();
    let changed_views: BTreeSet<String> = views_to_create
        .iter()
        .filter(|name| actual.has_view(name))
        .cloned()
        .collect();

    Ok(MigrationPlan {
        new_tables: ordered_new_tables,
        altered_tables: altered_tables.into_iter().collect(),
        rebuilt_tables: rebuilt_tables.into_iter().collect(),
        new_indexes: new_indexes.into_iter().collect(),
        changed_indexes: changed_indexes.into_iter().collect(),
        new_views: new_views.into_iter().collect(),
        changed_views: changed_views.into_iter().collect(),
        transactional_stmts,
        non_transactional_stmts,
    })
}

fn order_new_tables(
    to_create: &[String],
    desired: &SchemaSnapshot,
    actual: &SchemaSnapshot,
) -> Vec<String> {
    let mut remaining: BTreeSet<String> = to_create.iter().cloned().collect();
    let existing: HashSet<String> = actual
        .tables
        .keys()
        .map(|k| k.lower().to_string())
        .collect();
    let mut ordered = Vec::new();

    while !remaining.is_empty() {
        let mut progressed = false;
        let candidates: Vec<String> = remaining.iter().cloned().collect();

        for table in candidates {
            let refs = desired
                .get_table(&table)
                .map(|t| t.referenced_tables())
                .unwrap_or_default();

            let ready = refs
                .into_iter()
                .all(|r| existing.contains(&r.to_ascii_lowercase()) || !remaining.contains(&r));

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

fn order_view_creation(
    views_to_create: &BTreeSet<String>,
    desired: &SchemaSnapshot,
) -> Vec<String> {
    let mut remaining: BTreeSet<String> = views_to_create.clone();
    let mut ordered = Vec::new();
    let max_rounds = remaining.len() + 1;

    for _ in 0..max_rounds {
        if remaining.is_empty() {
            break;
        }
        let mut created = Vec::new();

        for name in &remaining {
            let depends_on_remaining = if let Some(view) = desired.get_view(name) {
                remaining
                    .iter()
                    .any(|other| other != name && view_depends_on_table(&view.sql, other))
            } else {
                false
            };

            if !depends_on_remaining {
                created.push(name.clone());
            }
        }

        if created.is_empty() {
            tracing::warn!("Circular view dependency detected: {:?}", remaining);
            ordered.extend(remaining.into_iter());
            break;
        }

        for name in &created {
            remaining.remove(name);
        }
        ordered.extend(created);
    }

    ordered
}

fn validate_rebuild_safety(
    table_name: &str,
    desired: &crate::schema::TableInfo,
    actual: &crate::schema::TableInfo,
) -> Result<(), MigrateError> {
    let actual_cols: HashSet<String> = actual
        .columns
        .iter()
        .map(|c| c.name.to_ascii_lowercase())
        .collect();

    for col in &desired.columns {
        if col.is_generated || col.is_hidden {
            continue;
        }
        let is_new = !actual_cols.contains(&col.name.to_ascii_lowercase());
        if is_new && col.notnull && col.default_value.is_none() {
            return Err(MigrateError::Schema(format!(
                "Table '{}' rebuild would add NOT NULL column '{}' without DEFAULT. \
                 Existing rows would violate the constraint. \
                 Add a DEFAULT value to the column definition.",
                table_name, col.name
            )));
        }
    }
    Ok(())
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
    if let Some(collation) = &col.collation {
        ddl.push_str(&format!(" COLLATE {collation}"));
    }
    ddl
}

fn build_copy_data_stmt(
    source_table: &str,
    temp_table: &str,
    desired_table: &crate::schema::TableInfo,
    actual_table: &crate::schema::TableInfo,
) -> String {
    let actual_cols: HashSet<String> = actual_table
        .columns
        .iter()
        .filter(|c| !c.is_generated && !c.is_hidden)
        .map(|c| c.name.to_ascii_lowercase())
        .collect();

    let mut insert_cols = Vec::new();
    let mut select_exprs = Vec::new();

    for col in &desired_table.columns {
        if col.is_generated || col.is_hidden {
            continue;
        }

        let col_lower = col.name.to_ascii_lowercase();
        insert_cols.push(quote_ident(&col.name));

        if actual_cols.contains(&col_lower) {
            select_exprs.push(quote_ident(&col.name));
        } else if let Some(default) = &col.default_value {
            select_exprs.push(default.clone());
        } else {
            select_exprs.push("NULL".to_string());
        }
    }

    let cols_str = insert_cols.join(", ");
    let exprs_str = select_exprs.join(", ");

    format!(
        "INSERT INTO {} ({cols_str}) SELECT {exprs_str} FROM {}",
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

pub(crate) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn is_protected_table(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "_schema_meta"
        || lower.starts_with("_converge_new_")
        || lower.starts_with("sqlite_")
        || lower.starts_with("fts_dir_")
        || lower.starts_with("__turso_internal")
        || lower == "_turso_migrations"
}

fn view_depends_on_table(view_sql: &str, table_name: &str) -> bool {
    let table = table_name.to_ascii_lowercase();
    view_sql
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|token| token == table)
}
