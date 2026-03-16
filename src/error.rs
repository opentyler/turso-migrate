#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("turso error: {0}")]
    Turso(#[from] turso::Error),
    #[error("I/O error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("migration statement failed: {stmt}; cause: {source}")]
    Statement { stmt: String, source: turso::Error },
    #[error("foreign key violations after migration: {0:?}")]
    ForeignKeyViolation(Vec<String>),
    #[error("schema error: {0}")]
    Schema(String),
}
