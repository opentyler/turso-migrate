use std::fmt;

use crate::options::ColumnRenameHint;
use crate::schema::{ColumnInfo, SchemaSnapshot, TableInfo};

/// Categorized differences between desired and actual database schemas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDiff {
    pub tables_to_create: Vec<String>,
    pub tables_to_drop: Vec<String>,
    pub tables_to_rebuild: Vec<String>,
    pub columns_to_add: Vec<(String, ColumnInfo)>,
    pub columns_to_drop: Vec<(String, String)>,
    pub columns_to_rename: Vec<(String, String, String)>,
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
            && self.columns_to_rename.is_empty()
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
        for (t, old, new) in &self.columns_to_rename {
            writeln!(f, "~ COLUMN {t}.{old} -> {new}")?;
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

/// Normalize schema SQL for BLAKE3 hashing (strips comments, collapses whitespace, preserves literals).
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

/// Compute schema diff without rename hints.
pub fn compute_diff(desired: &SchemaSnapshot, actual: &SchemaSnapshot) -> SchemaDiff {
    compute_diff_with_hints(desired, actual, &[])
}

/// Compute schema diff with optional column rename detection hints.
pub fn compute_diff_with_hints(
    desired: &SchemaSnapshot,
    actual: &SchemaSnapshot,
    rename_hints: &[ColumnRenameHint],
) -> SchemaDiff {
    let mut diff = SchemaDiff {
        tables_to_create: Vec::new(),
        tables_to_drop: Vec::new(),
        tables_to_rebuild: Vec::new(),
        columns_to_add: Vec::new(),
        columns_to_drop: Vec::new(),
        columns_to_rename: Vec::new(),
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
        let mut missing_actual: Vec<ColumnInfo> = Vec::new();
        let mut missing_desired: Vec<ColumnInfo> = Vec::new();
        let mut renamed_columns: Vec<(String, String)> = Vec::new();

        for actual_col in &actual_table.columns {
            if actual_col.is_hidden {
                continue;
            }
            if !desired_table
                .columns
                .iter()
                .any(|dc| dc.name.eq_ignore_ascii_case(&actual_col.name))
            {
                missing_actual.push(actual_col.clone());
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
                        needs_rebuild = true;
                        break;
                    }
                } else {
                    missing_desired.push(desired_col.clone());
                }
            }
        }

        if !needs_rebuild {
            let missing_actual_names: Vec<String> =
                missing_actual.iter().map(|c| c.name.clone()).collect();
            let (detected_renames, remaining_add, remaining_drop) = detect_column_renames(
                name.raw(),
                desired_table,
                actual_table,
                missing_desired,
                missing_actual_names,
                rename_hints,
            );
            renamed_columns = detected_renames;
            addable_columns = remaining_add;

            for col_name in remaining_drop {
                if let Some(col) = missing_actual
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(&col_name))
                {
                    if can_drop_column(col, actual_table, actual) {
                        droppable_columns.push(col_name);
                    } else {
                        needs_rebuild = true;
                        break;
                    }
                }
            }

            if !needs_rebuild {
                for col in &addable_columns {
                    if !can_add_column(col) {
                        needs_rebuild = true;
                        break;
                    }
                }
            }
        }

        if needs_rebuild {
            diff.tables_to_rebuild.push(name.raw().to_string());
        } else {
            for col in addable_columns {
                diff.columns_to_add.push((name.raw().to_string(), col));
            }
            for col in droppable_columns {
                diff.columns_to_drop.push((name.raw().to_string(), col));
            }
            for (from, to) in renamed_columns {
                diff.columns_to_rename
                    .push((name.raw().to_string(), from, to));
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

fn detect_column_renames(
    table_name: &str,
    desired_table: &TableInfo,
    actual_table: &TableInfo,
    addable_columns: Vec<ColumnInfo>,
    droppable_columns: Vec<String>,
    rename_hints: &[ColumnRenameHint],
) -> (Vec<(String, String)>, Vec<ColumnInfo>, Vec<String>) {
    let mut remaining_add = addable_columns;
    let mut remaining_drop = droppable_columns;
    let mut renamed = Vec::new();

    let mut hinted_pairs = Vec::new();
    for hint in rename_hints {
        if !hint.table.eq_ignore_ascii_case(table_name) {
            continue;
        }
        let drop_idx = remaining_drop
            .iter()
            .position(|old| old.eq_ignore_ascii_case(&hint.from));
        let add_idx = remaining_add
            .iter()
            .position(|new_col| new_col.name.eq_ignore_ascii_case(&hint.to));

        if let (Some(di), Some(ai)) = (drop_idx, add_idx) {
            let old_name = remaining_drop[di].clone();
            let new_name = remaining_add[ai].name.clone();
            let old_col = actual_table
                .columns
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(&old_name));
            let new_col = desired_table
                .columns
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(&new_name));

            if let (Some(old_col), Some(new_col)) = (old_col, new_col)
                && columns_compatible_for_rename(old_col, new_col)
            {
                hinted_pairs.push((di, ai, old_name, new_name));
            }
        }
    }

    hinted_pairs.sort_by(|a, b| b.0.cmp(&a.0));
    let mut remove_add = Vec::new();
    for (drop_idx, add_idx, old_name, new_name) in hinted_pairs {
        renamed.push((old_name, new_name));
        remaining_drop.remove(drop_idx);
        remove_add.push(add_idx);
    }
    remove_add.sort_unstable();
    remove_add.dedup();
    for idx in remove_add.into_iter().rev() {
        remaining_add.remove(idx);
    }

    if remaining_add.is_empty() || remaining_drop.is_empty() {
        return (renamed, remaining_add, remaining_drop);
    }

    let mut candidates: Vec<Vec<usize>> = Vec::new();
    for old_name in &remaining_drop {
        let Some(old_col) = actual_table
            .columns
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(old_name))
        else {
            candidates.push(Vec::new());
            continue;
        };

        let old_pos = column_position(actual_table, &old_col.name);
        let mut matches = Vec::new();
        for (idx, new_col) in remaining_add.iter().enumerate() {
            if !columns_compatible_for_rename(old_col, new_col) {
                continue;
            }
            if column_position(desired_table, &new_col.name) == old_pos {
                matches.push(idx);
            }
        }
        candidates.push(matches);
    }

    let mut add_usage = vec![0usize; remaining_add.len()];
    for cand in &candidates {
        if cand.len() == 1 {
            add_usage[cand[0]] += 1;
        }
    }

    let mut drop_remove = Vec::new();
    let mut add_remove = Vec::new();

    for (drop_idx, cand) in candidates.iter().enumerate() {
        if cand.len() == 1 {
            let add_idx = cand[0];
            if add_usage[add_idx] == 1 {
                let old_name = remaining_drop[drop_idx].clone();
                let new_name = remaining_add[add_idx].name.clone();
                renamed.push((old_name, new_name));
                drop_remove.push(drop_idx);
                add_remove.push(add_idx);
            }
        }
    }

    drop_remove.sort_unstable();
    drop_remove.dedup();
    for idx in drop_remove.into_iter().rev() {
        remaining_drop.remove(idx);
    }

    add_remove.sort_unstable();
    add_remove.dedup();
    for idx in add_remove.into_iter().rev() {
        remaining_add.remove(idx);
    }

    (renamed, remaining_add, remaining_drop)
}

fn columns_compatible_for_rename(old_col: &ColumnInfo, new_col: &ColumnInfo) -> bool {
    !old_col.is_generated
        && !old_col.is_hidden
        && !new_col.is_generated
        && !new_col.is_hidden
        && types_match(&old_col.col_type, &new_col.col_type)
        && old_col.notnull == new_col.notnull
        && old_col.pk == new_col.pk
        && old_col.collation == new_col.collation
        && defaults_match(&old_col.default_value, &new_col.default_value)
}

fn column_position(table: &TableInfo, name: &str) -> Option<usize> {
    let mut pos = 0usize;
    for col in &table.columns {
        if col.is_hidden {
            continue;
        }
        if col.name.eq_ignore_ascii_case(name) {
            return Some(pos);
        }
        pos += 1;
    }
    None
}
