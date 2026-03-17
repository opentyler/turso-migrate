pub mod bridge;
pub mod connection;
pub mod converge;
pub mod diff;
pub mod error;
pub mod execute;
pub mod introspect;
pub mod options;
pub mod plan;
pub mod schema;

pub use bridge::bridge_legacy;
pub use connection::{
    ConnectionLike, converge_like, converge_like_with_options, schema_version_like,
};
pub use converge::{
    converge, converge_from_path, converge_multi, converge_multi_with_options,
    converge_with_options, is_read_only, rollback_to_previous, schema_version, validate_schema,
};
pub use diff::{SchemaDiff, compute_diff};
pub use error::MigrateError;
pub use execute::execute_plan;
pub use options::{
    ColumnRenameHint, ConvergeMode, ConvergeOptions, ConvergePolicy, ConvergeReport, DataMigration,
    DestructiveChangeSet, Failpoint,
};
pub use plan::{MigrationPlan, generate_plan};
pub use schema::{
    CIString, Capabilities, ColumnInfo, ForeignKey, IndexInfo, SchemaSnapshot, TableInfo,
    TriggerInfo, ViewInfo,
};
