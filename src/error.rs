/// Errors returned by convergence, plan execution, and schema validation.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("turso error: {0}")]
    Turso(#[from] turso::Error),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("migration statement failed ({phase}): {stmt}; cause: {source}")]
    Statement {
        stmt: String,
        source: turso::Error,
        phase: String,
    },
    #[error("foreign key violation: table={table}, rowid={rowid}, references={parent}")]
    ForeignKeyViolation {
        table: String,
        rowid: i64,
        parent: String,
    },
    #[error("schema error: {0}")]
    Schema(String),
    #[error("database is read-only: migrations require write access")]
    ReadOnly,
    #[error(
        "migration busy: another migration is in progress (owner={owner}, expires in {remaining_secs}s)"
    )]
    MigrationBusy { owner: String, remaining_secs: u64 },
    #[error("pre-destructive hook rejected migration: {message}")]
    PreDestructiveHookRejected {
        message: String,
        blocked_operations: Vec<String>,
    },
    #[error("unsupported feature: {0}")]
    UnsupportedFeature(String),
    #[error("injected failpoint triggered: {failpoint}")]
    InjectedFailure { failpoint: String },
    #[error("policy violation: {message}")]
    PolicyViolation {
        message: String,
        blocked_operations: Vec<String>,
    },
}
