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
    #[error("policy violation: {message}")]
    PolicyViolation {
        message: String,
        blocked_operations: Vec<String>,
    },
}
