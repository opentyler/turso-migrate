use std::fmt;

use crate::schema::{ColumnInfo, SchemaSnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDiff {
    pub tables_to_create: Vec<String>,
    pub tables_to_drop: Vec<String>,
    pub tables_to_rebuild: Vec<String>,
    pub columns_to_add: Vec<(String, ColumnInfo)>,
    pub columns_to_drop: Vec<(String, String)>,
    pub indexes_to_create: Vec<String>,
    pub indexes_to_drop: Vec<String>,
    pub fts_indexes_to_create: Vec<String>,
    pub fts_indexes_to_drop: Vec<String>,
    pub views_to_create: Vec<String>,
    pub views_to_drop: Vec<String>,
    pub triggers_to_create: Vec<String>,
    pub triggers_to_drop: Vec<String>,
}

impl SchemaDiff {
    pub fn is_empty(&self) -> bool {
        self.tables_to_create.is_empty()
            && self.tables_to_drop.is_empty()
            && self.tables_to_rebuild.is_empty()
            && self.columns_to_add.is_empty()
            && self.columns_to_drop.is_empty()
            && self.indexes_to_create.is_empty()
            && self.indexes_to_drop.is_empty()
            && self.fts_indexes_to_create.is_empty()
            && self.fts_indexes_to_drop.is_empty()
            && self.views_to_create.is_empty()
            && self.views_to_drop.is_empty()
            && self.triggers_to_create.is_empty()
            && self.triggers_to_drop.is_empty()
    }
}

impl fmt::Display for SchemaDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return write!(f, "(no changes)");
        }
        for t in &self.tables_to_create {
            writeln!(f, "+ TABLE {t}")?;
        }
        for t in &self.tables_to_drop {
            writeln!(f, "- TABLE {t}")?;
        }
        for t in &self.tables_to_rebuild {
            writeln!(f, "~ TABLE {t}: REBUILD")?;
        }
        for (t, col) in &self.columns_to_add {
            writeln!(f, "+ COLUMN {t}.{} {}", col.name, col.col_type)?;
        }
        for (t, col) in &self.columns_to_drop {
            writeln!(f, "- COLUMN {t}.{col}")?;
        }
        for i in &self.indexes_to_create {
            writeln!(f, "+ INDEX {i}")?;
        }
        for i in &self.indexes_to_drop {
            writeln!(f, "- INDEX {i}")?;
        }
        for i in &self.fts_indexes_to_create {
            writeln!(f, "+ FTS INDEX {i}")?;
        }
        for i in &self.fts_indexes_to_drop {
            writeln!(f, "- FTS INDEX {i}")?;
        }
        for v in &self.views_to_create {
            writeln!(f, "+ VIEW {v}")?;
        }
        for v in &self.views_to_drop {
            writeln!(f, "- VIEW {v}")?;
        }
        for t in &self.triggers_to_create {
            writeln!(f, "+ TRIGGER {t}")?;
        }
        for t in &self.triggers_to_drop {
            writeln!(f, "- TRIGGER {t}")?;
        }
        Ok(())
    }
}

/// Normalize SQL for comparison: collapse whitespace and lowercase
/// identifiers/keywords while preserving case inside string literals.
pub(crate) fn normalize_sql(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut prev_was_space = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\'' | '"' => {
                prev_was_space = false;
                result.push(ch);
                let quote = ch;
                loop {
                    match chars.next() {
                        Some(c) if c == quote => {
                            result.push(c);
                            if chars.peek() == Some(&quote) {
                                result.push(chars.next().unwrap());
                            } else {
                                break;
                            }
                        }
                        Some(c) => result.push(c),
                        None => break,
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                if !prev_was_space && !result.is_empty() {
                    result.push(' ');
                    prev_was_space = true;
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut depth = 1;
                while depth > 0 {
                    match chars.next() {
                        Some('*') if chars.peek() == Some(&'/') => {
                            chars.next();
                            depth -= 1;
                        }
                        Some('/') if chars.peek() == Some(&'*') => {
                            chars.next();
                            depth += 1;
                        }
                        None => break,
                        _ => {}
                    }
                }
                if !prev_was_space && !result.is_empty() {
                    result.push(' ');
                    prev_was_space = true;
                }
            }
            c if c.is_ascii_whitespace() => {
                if !prev_was_space && !result.is_empty() {
                    result.push(' ');
                }
                prev_was_space = true;
            }
            c => {
                result.push(c.to_ascii_lowercase());
                prev_was_space = false;
            }
        }
    }
    result.trim().to_string()
}

pub fn normalize_for_hash(sql: &str) -> String {
    let mut normalized = normalize_sql(sql);
    while normalized.ends_with(';') {
        normalized.pop();
    }
    normalized.trim().to_string()
}

fn types_match(desired: &str, actual: &str) -> bool {
    desired.eq_ignore_ascii_case(actual)
}

fn defaults_match(desired: &Option<String>, actual: &Option<String>) -> bool {
    match (desired, actual) {
        (Some(d), Some(a)) => normalize_sql(d) == normalize_sql(a),
        (None, None) => true,
        _ => false,
    }
}

fn can_drop_column(
    col: &ColumnInfo,
    table: &crate::schema::TableInfo,
    snapshot: &SchemaSnapshot,
) -> bool {
    if col.pk != 0 {
        return false;
    }
    let col_lower = col.name.to_ascii_lowercase();
    for idx in snapshot.indexes.values() {
        if idx.table_name.eq_ignore_ascii_case(&table.name)
            && idx
                .columns
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&col_lower))
        {
            return false;
        }
    }
    for trigger in snapshot.triggers.values() {
        if trigger.table_name.eq_ignore_ascii_case(&table.name) {
            return false;
        }
    }
    let table_lower = table.name.to_ascii_lowercase();
    for view in snapshot.views.values() {
        let lower_sql = view.sql.to_ascii_lowercase();
        if lower_sql.contains(&table_lower) {
            return false;
        }
    }
    for fk in &table.foreign_keys {
        if fk
            .from_columns
            .iter()
            .any(|c| c.eq_ignore_ascii_case(&col.name))
        {
            return false;
        }
    }
    for other in snapshot.tables.values() {
        for fk in &other.foreign_keys {
            if fk.to_table.eq_ignore_ascii_case(&table.name)
                && fk
                    .to_columns
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(&col.name))
            {
                return false;
            }
        }
    }
    true
}

fn can_add_column(col: &ColumnInfo) -> bool {
    col.pk == 0 && (!col.notnull || col.default_value.is_some()) && !col.is_generated
}

pub fn compute_diff(desired: &SchemaSnapshot, actual: &SchemaSnapshot) -> SchemaDiff {
    let mut diff = SchemaDiff {
        tables_to_create: Vec::new(),
        tables_to_drop: Vec::new(),
        tables_to_rebuild: Vec::new(),
        columns_to_add: Vec::new(),
        columns_to_drop: Vec::new(),
        indexes_to_create: Vec::new(),
        indexes_to_drop: Vec::new(),
        fts_indexes_to_create: Vec::new(),
        fts_indexes_to_drop: Vec::new(),
        views_to_create: Vec::new(),
        views_to_drop: Vec::new(),
        triggers_to_create: Vec::new(),
        triggers_to_drop: Vec::new(),
    };

    for name in desired.tables.keys() {
        if !actual.has_table(name.raw()) {
            diff.tables_to_create.push(name.raw().to_string());
        }
    }
    for name in actual.tables.keys() {
        if !desired.has_table(name.raw()) {
            diff.tables_to_drop.push(name.raw().to_string());
        }
    }

    for (name, desired_table) in &desired.tables {
        let Some(actual_table) = actual.get_table(name.raw()) else {
            continue;
        };

        let mut needs_rebuild = false;
        let mut addable_columns: Vec<ColumnInfo> = Vec::new();
        let mut droppable_columns: Vec<String> = Vec::new();
        let mut has_other_changes = false;

        for actual_col in &actual_table.columns {
            if actual_col.is_hidden {
                continue;
            }
            if !desired_table
                .columns
                .iter()
                .any(|dc| dc.name.eq_ignore_ascii_case(&actual_col.name))
            {
                if can_drop_column(actual_col, actual_table, actual) {
                    droppable_columns.push(actual_col.name.clone());
                } else {
                    needs_rebuild = true;
                    break;
                }
            }
        }

        if !needs_rebuild {
            for desired_col in &desired_table.columns {
                if desired_col.is_hidden {
                    continue;
                }
                if let Some(actual_col) = actual_table
                    .columns
                    .iter()
                    .find(|ac| ac.name.eq_ignore_ascii_case(&desired_col.name))
                {
                    if !types_match(&actual_col.col_type, &desired_col.col_type)
                        || actual_col.notnull != desired_col.notnull
                        || !defaults_match(&actual_col.default_value, &desired_col.default_value)
                        || actual_col.pk != desired_col.pk
                        || actual_col.collation != desired_col.collation
                        || actual_col.is_generated != desired_col.is_generated
                    {
                        has_other_changes = true;
                        needs_rebuild = true;
                        break;
                    }
                } else if can_add_column(desired_col) {
                    addable_columns.push(desired_col.clone());
                } else {
                    has_other_changes = true;
                    needs_rebuild = true;
                    break;
                }
            }
        }

        if needs_rebuild || (!droppable_columns.is_empty() && has_other_changes) {
            diff.tables_to_rebuild.push(name.raw().to_string());
        } else {
            for col in addable_columns {
                diff.columns_to_add.push((name.raw().to_string(), col));
            }
            for col in droppable_columns {
                diff.columns_to_drop.push((name.raw().to_string(), col));
            }
        }
    }

    let rebuilt_tables: std::collections::HashSet<String> = diff
        .tables_to_rebuild
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();

    for (name, desired_idx) in &desired.indexes {
        if let Some(actual_idx) = actual.get_index(name.raw()) {
            let sql_changed = normalize_sql(&desired_idx.sql) != normalize_sql(&actual_idx.sql);
            let parent_rebuilt =
                rebuilt_tables.contains(&desired_idx.table_name.to_ascii_lowercase());

            if sql_changed || parent_rebuilt {
                if desired_idx.is_fts {
                    diff.fts_indexes_to_drop.push(name.raw().to_string());
                    diff.fts_indexes_to_create.push(name.raw().to_string());
                } else {
                    diff.indexes_to_drop.push(name.raw().to_string());
                    diff.indexes_to_create.push(name.raw().to_string());
                }
            }
        } else if desired_idx.is_fts {
            diff.fts_indexes_to_create.push(name.raw().to_string());
        } else {
            diff.indexes_to_create.push(name.raw().to_string());
        }
    }
    for (name, actual_idx) in &actual.indexes {
        if !desired.has_index(name.raw()) {
            if actual_idx.is_fts {
                diff.fts_indexes_to_drop.push(name.raw().to_string());
            } else {
                diff.indexes_to_drop.push(name.raw().to_string());
            }
        }
    }

    for (name, desired_view) in &desired.views {
        if let Some(actual_view) = actual.get_view(name.raw()) {
            if normalize_sql(&desired_view.sql) != normalize_sql(&actual_view.sql) {
                diff.views_to_drop.push(name.raw().to_string());
                diff.views_to_create.push(name.raw().to_string());
            }
        } else {
            diff.views_to_create.push(name.raw().to_string());
        }
    }
    for name in actual.views.keys() {
        if !desired.has_view(name.raw()) {
            diff.views_to_drop.push(name.raw().to_string());
        }
    }

    for (name, desired_trigger) in &desired.triggers {
        if let Some(actual_trigger) = actual.get_trigger(name.raw()) {
            if normalize_sql(&desired_trigger.sql) != normalize_sql(&actual_trigger.sql) {
                diff.triggers_to_drop.push(name.raw().to_string());
                diff.triggers_to_create.push(name.raw().to_string());
            }
        } else {
            diff.triggers_to_create.push(name.raw().to_string());
        }
    }
    for name in actual.triggers.keys() {
        if !desired.has_trigger(name.raw()) {
            diff.triggers_to_drop.push(name.raw().to_string());
        }
    }

    diff
}
