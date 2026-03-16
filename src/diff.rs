use crate::introspect::{ColumnInfo, SchemaSnapshot};

#[derive(Debug, Clone, PartialEq)]
pub struct SchemaDiff {
    pub tables_to_create: Vec<String>,
    pub tables_to_drop: Vec<String>,
    pub tables_to_rebuild: Vec<String>,
    pub columns_to_add: Vec<(String, ColumnInfo)>,
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

pub(crate) fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn can_add_column(col: &ColumnInfo) -> bool {
    col.pk == 0 && (!col.notnull || col.default_value.is_some())
}

pub fn compute_diff(desired: &SchemaSnapshot, actual: &SchemaSnapshot) -> SchemaDiff {
    let mut diff = SchemaDiff {
        tables_to_create: Vec::new(),
        tables_to_drop: Vec::new(),
        tables_to_rebuild: Vec::new(),
        columns_to_add: Vec::new(),
        indexes_to_create: Vec::new(),
        indexes_to_drop: Vec::new(),
        fts_indexes_to_create: Vec::new(),
        fts_indexes_to_drop: Vec::new(),
        views_to_create: Vec::new(),
        views_to_drop: Vec::new(),
        triggers_to_create: Vec::new(),
        triggers_to_drop: Vec::new(),
    };

    // --- Tables ---
    for name in desired.tables.keys() {
        if !actual.tables.contains_key(name) {
            diff.tables_to_create.push(name.clone());
        }
    }
    for name in actual.tables.keys() {
        if !desired.tables.contains_key(name) {
            diff.tables_to_drop.push(name.clone());
        }
    }

    // Tables in both — compare columns
    for (name, desired_table) in &desired.tables {
        let Some(actual_table) = actual.tables.get(name) else {
            continue; // already in tables_to_create
        };

        let mut needs_rebuild = false;
        let mut addable_columns: Vec<ColumnInfo> = Vec::new();

        // Check for removed columns (in actual but not in desired)
        for actual_col in &actual_table.columns {
            if !desired_table
                .columns
                .iter()
                .any(|dc| dc.name == actual_col.name)
            {
                needs_rebuild = true;
                break;
            }
        }

        if !needs_rebuild {
            // Check for changed columns
            for desired_col in &desired_table.columns {
                if let Some(actual_col) = actual_table
                    .columns
                    .iter()
                    .find(|ac| ac.name == desired_col.name)
                {
                    // Column exists in both — compare attributes
                    if actual_col.col_type != desired_col.col_type
                        || actual_col.notnull != desired_col.notnull
                        || actual_col.default_value != desired_col.default_value
                        || actual_col.pk != desired_col.pk
                    {
                        needs_rebuild = true;
                        break;
                    }
                } else {
                    // New column — check ADD COLUMN eligibility
                    if can_add_column(desired_col) {
                        addable_columns.push(desired_col.clone());
                    } else {
                        needs_rebuild = true;
                        break;
                    }
                }
            }
        }

        if needs_rebuild {
            diff.tables_to_rebuild.push(name.clone());
            // When rebuilding, individual column adds are handled by the rebuild
        } else {
            for col in addable_columns {
                diff.columns_to_add.push((name.clone(), col));
            }
        }
    }

    // Collect tables being rebuilt for index dependency check
    let rebuilt_tables: std::collections::HashSet<&str> =
        diff.tables_to_rebuild.iter().map(|s| s.as_str()).collect();

    // --- Indexes ---
    for (name, desired_idx) in &desired.indexes {
        if let Some(actual_idx) = actual.indexes.get(name) {
            // Exists in both — check if changed
            if normalize_sql(&desired_idx.sql) != normalize_sql(&actual_idx.sql) {
                if desired_idx.is_fts {
                    diff.fts_indexes_to_drop.push(name.clone());
                    diff.fts_indexes_to_create.push(name.clone());
                } else {
                    diff.indexes_to_drop.push(name.clone());
                    diff.indexes_to_create.push(name.clone());
                }
            } else if rebuilt_tables.contains(desired_idx.table_name.as_str()) {
                // Parent table is being rebuilt — index must be recreated
                if desired_idx.is_fts {
                    diff.fts_indexes_to_drop.push(name.clone());
                    diff.fts_indexes_to_create.push(name.clone());
                } else {
                    diff.indexes_to_drop.push(name.clone());
                    diff.indexes_to_create.push(name.clone());
                }
            }
        } else {
            // New index
            if desired_idx.is_fts {
                diff.fts_indexes_to_create.push(name.clone());
            } else {
                diff.indexes_to_create.push(name.clone());
            }
        }
    }
    for (name, actual_idx) in &actual.indexes {
        if !desired.indexes.contains_key(name) {
            if actual_idx.is_fts {
                diff.fts_indexes_to_drop.push(name.clone());
            } else {
                diff.indexes_to_drop.push(name.clone());
            }
        }
    }

    // --- Views ---
    for (name, desired_view) in &desired.views {
        if let Some(actual_view) = actual.views.get(name) {
            if normalize_sql(&desired_view.sql) != normalize_sql(&actual_view.sql) {
                diff.views_to_drop.push(name.clone());
                diff.views_to_create.push(name.clone());
            }
        } else {
            diff.views_to_create.push(name.clone());
        }
    }
    for name in actual.views.keys() {
        if !desired.views.contains_key(name) {
            diff.views_to_drop.push(name.clone());
        }
    }

    // --- Triggers ---
    for (name, desired_trigger) in &desired.triggers {
        if let Some(actual_trigger) = actual.triggers.get(name) {
            if normalize_sql(&desired_trigger.sql) != normalize_sql(&actual_trigger.sql) {
                diff.triggers_to_drop.push(name.clone());
                diff.triggers_to_create.push(name.clone());
            }
        } else {
            diff.triggers_to_create.push(name.clone());
        }
    }
    for name in actual.triggers.keys() {
        if !desired.triggers.contains_key(name) {
            diff.triggers_to_drop.push(name.clone());
        }
    }

    diff
}
