//! Canonical schema intermediate representation.
//!
//! Every schema object carries a [`CIString`] name that preserves original
//! spelling via `.raw()` while using case-folded comparison for `Ord`/`Eq`/`Hash`.
//! This matches Turso's case-insensitive identifier semantics.

use std::collections::BTreeMap;
use std::fmt;

/// Case-insensitive string wrapper for use as BTreeMap keys.
///
/// Stores both the original spelling and a pre-computed ASCII-lowercase form.
/// `Ord`, `Eq`, and `Hash` operate on the lowercase form; `.raw()` returns
/// the original for DDL output and error messages.
#[derive(Debug, Clone)]
pub struct CIString {
    raw: String,
    lower: String,
}

impl CIString {
    pub fn new(s: impl Into<String>) -> Self {
        let raw = s.into();
        let lower = raw.to_ascii_lowercase();
        Self { raw, lower }
    }

    /// Original spelling (for DDL output, error messages).
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Lowercase form (for comparison, used internally).
    pub fn lower(&self) -> &str {
        &self.lower
    }
}

impl fmt::Display for CIString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl PartialEq for CIString {
    fn eq(&self, other: &Self) -> bool {
        self.lower == other.lower
    }
}

impl Eq for CIString {}

impl Ord for CIString {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.lower.cmp(&other.lower)
    }
}

impl PartialOrd for CIString {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::hash::Hash for CIString {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.lower.hash(state);
    }
}

/// Complete schema representation: tables, indexes, views, and triggers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaSnapshot {
    pub tables: BTreeMap<CIString, TableInfo>,
    pub indexes: BTreeMap<CIString, IndexInfo>,
    pub views: BTreeMap<CIString, ViewInfo>,
    pub triggers: BTreeMap<CIString, TriggerInfo>,
}

impl SchemaSnapshot {
    /// Convenience lookup by plain string (creates CIString internally).
    pub fn get_table(&self, name: &str) -> Option<&TableInfo> {
        self.tables.get(&CIString::new(name))
    }

    pub fn get_index(&self, name: &str) -> Option<&IndexInfo> {
        self.indexes.get(&CIString::new(name))
    }

    pub fn get_view(&self, name: &str) -> Option<&ViewInfo> {
        self.views.get(&CIString::new(name))
    }

    pub fn get_trigger(&self, name: &str) -> Option<&TriggerInfo> {
        self.triggers.get(&CIString::new(name))
    }

    pub fn has_table(&self, name: &str) -> bool {
        self.tables.contains_key(&CIString::new(name))
    }

    pub fn has_index(&self, name: &str) -> bool {
        self.indexes.contains_key(&CIString::new(name))
    }

    pub fn has_view(&self, name: &str) -> bool {
        self.views.contains_key(&CIString::new(name))
    }

    pub fn has_trigger(&self, name: &str) -> bool {
        self.triggers.contains_key(&CIString::new(name))
    }
}

/// Parsed table metadata: columns, foreign keys, and table options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    pub sql: String,
    pub columns: Vec<ColumnInfo>,
    pub foreign_keys: Vec<ForeignKey>,
    pub is_strict: bool,
    pub is_without_rowid: bool,
    pub has_autoincrement: bool,
}

impl TableInfo {
    /// Tables referenced by foreign keys in this table.
    pub fn referenced_tables(&self) -> std::collections::BTreeSet<String> {
        self.foreign_keys
            .iter()
            .map(|fk| fk.to_table.clone())
            .collect()
    }
}

/// Column metadata from `PRAGMA table_xinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub col_type: String,
    pub notnull: bool,
    pub default_value: Option<String>,
    pub pk: i64,
    /// COLLATE clause (e.g., "NOCASE", "BINARY"). None if not specified.
    pub collation: Option<String>,
    /// Whether this is a GENERATED ALWAYS AS column.
    pub is_generated: bool,
    /// Whether this column is hidden (e.g., rowid alias on virtual tables).
    pub is_hidden: bool,
}

impl ColumnInfo {
    /// Whether this column can be the target of an INSERT statement.
    /// Generated and hidden columns cannot be inserted into.
    pub fn is_insertable(&self) -> bool {
        !self.is_generated && !self.is_hidden
    }
}

/// Foreign key constraint with source/target columns and actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    pub from_columns: Vec<String>,
    pub to_table: String,
    pub to_columns: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
}

/// Index metadata including FTS and UNIQUE flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub table_name: String,
    pub sql: String,
    pub is_fts: bool,
    pub is_unique: bool,
    pub columns: Vec<String>,
}

/// View metadata with materialized view detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewInfo {
    pub name: String,
    pub sql: String,
    pub is_materialized: bool,
}

/// Trigger metadata: name, attached table, and DDL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerInfo {
    pub name: String,
    pub table_name: String,
    pub sql: String,
}

/// Detected capabilities of the target Turso database connection.
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub database_version: (u32, u32, u32),
    /// >= 3.35.0: ALTER TABLE DROP COLUMN
    pub supports_drop_column: bool,
    /// >= 3.25.0: ALTER TABLE RENAME COLUMN
    pub supports_rename_column: bool,
    /// Whether the FTS (tantivy) module is available.
    pub has_fts_module: bool,
    /// Whether vector column types are available.
    pub has_vector_module: bool,
    /// Whether MATERIALIZED VIEW is supported.
    pub has_materialized_views: bool,
    /// Whether WITHOUT ROWID tables are supported.
    pub supports_without_rowid: bool,
    /// Whether GENERATED ALWAYS AS columns are supported.
    pub supports_generated_columns: bool,
    /// Whether triggers are supported (requires `.experimental_triggers(true)` on Turso).
    pub has_triggers: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            database_version: (3, 45, 0),
            supports_drop_column: true,
            supports_rename_column: true,
            has_fts_module: false,
            has_vector_module: false,
            has_materialized_views: false,
            supports_without_rowid: false,
            supports_generated_columns: false,
            has_triggers: false,
        }
    }
}
