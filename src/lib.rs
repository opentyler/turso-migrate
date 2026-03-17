pub mod bridge;
pub mod converge;
pub mod diff;
pub mod error;
pub mod execute;
pub mod introspect;
pub mod options;
pub mod plan;
pub mod schema;

pub use bridge::bridge_legacy;
pub use converge::{converge, converge_from_path, converge_with_options, schema_version};
pub use diff::{SchemaDiff, compute_diff};
pub use error::MigrateError;
pub use execute::execute_plan;
pub use options::{ConvergeMode, ConvergeOptions, ConvergePolicy, ConvergeReport};
pub use plan::{MigrationPlan, generate_plan};
pub use schema::{
    CIString, Capabilities, ColumnInfo, ForeignKey, IndexInfo, SchemaSnapshot, TableInfo,
    TriggerInfo, ViewInfo,
};
