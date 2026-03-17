use crate::diff::normalize_for_hash;
use crate::error::MigrateError;
use crate::plan::MigrationPlan;
use crate::schema::SchemaSnapshot;

pub struct Migrator {
    schema_sql: String,
    schema_hash: String,
    allow_deletions: bool,
}

impl Migrator {
    pub fn new(schema_sql: impl Into<String>) -> Self {
        let sql = schema_sql.into();
        let normalized = normalize_for_hash(&sql);
        let hash = blake3::hash(normalized.as_bytes()).to_hex().to_string();
        Self {
            schema_sql: sql,
            schema_hash: hash,
            allow_deletions: false,
        }
    }

    pub fn allow_deletions(mut self, allow: bool) -> Self {
        self.allow_deletions = allow;
        self
    }

    pub fn schema_hash(&self) -> &str {
        &self.schema_hash
    }

    pub async fn plan(&self, conn: &turso::Connection) -> Result<MigrationPlan, MigrateError> {
        let desired = SchemaSnapshot::from_schema_sql(&self.schema_sql).await?;
        let actual = SchemaSnapshot::from_connection(conn).await?;
        let diff = crate::compute_diff(&desired, &actual);
        crate::generate_plan(&diff, &desired, &actual)
    }

    pub async fn migrate(&self, conn: &turso::Connection) -> Result<MigrationPlan, MigrateError> {
        let desired = SchemaSnapshot::from_schema_sql(&self.schema_sql).await?;
        let actual = SchemaSnapshot::from_connection(conn).await?;
        let mut diff = crate::compute_diff(&desired, &actual);

        if !self.allow_deletions && !diff.tables_to_drop.is_empty() {
            tracing::warn!(
                "Tables in DB but not in schema (skipped, allow_deletions=false): {:?}",
                diff.tables_to_drop
            );
            diff.tables_to_drop.clear();
        }

        let plan = crate::generate_plan(&diff, &desired, &actual)?;
        if !plan.is_empty() {
            crate::execute_plan(conn, &plan).await?;
        }
        Ok(plan)
    }
}
