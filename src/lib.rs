pub mod bridge;
pub mod converge;
pub mod data_migrations;
pub mod diff;
pub mod error;
pub mod execute;
pub mod introspect;
pub mod migrator;
pub mod options;
pub mod plan;
pub mod schema;

pub use bridge::bridge_legacy;
pub use converge::{converge, converge_from_path, converge_with_options, schema_version};
pub use data_migrations::converge_data;
pub use diff::{SchemaDiff, compute_diff};
pub use error::MigrateError;
pub use execute::execute_plan;
pub use migrator::Migrator;
pub use options::{ConvergeMode, ConvergeOptions, ConvergePolicy, ConvergeReport};
pub use plan::{MigrationPlan, generate_plan};
pub use schema::{
    CIString, Capabilities, ColumnInfo, ForeignKey, IndexInfo, SchemaSnapshot, TableInfo,
    TriggerInfo, ViewInfo,
};
