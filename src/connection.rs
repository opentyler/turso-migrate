use crate::MigrateError;
use crate::converge::{converge, converge_with_options, schema_version};
use crate::options::{ConvergeOptions, ConvergeReport};

pub trait ConnectionLike {
    fn as_turso_connection(&self) -> &turso::Connection;
}

impl ConnectionLike for turso::Connection {
    fn as_turso_connection(&self) -> &turso::Connection {
        self
    }
}

pub async fn converge_like<C: ConnectionLike>(
    conn: &C,
    schema_sql: &str,
) -> Result<(), MigrateError> {
    converge(conn.as_turso_connection(), schema_sql).await
}

pub async fn converge_like_with_options<C: ConnectionLike>(
    conn: &C,
    schema_sql: &str,
    options: &ConvergeOptions,
) -> Result<ConvergeReport, MigrateError> {
    converge_with_options(conn.as_turso_connection(), schema_sql, options).await
}

pub async fn schema_version_like<C: ConnectionLike>(conn: &C) -> Result<u32, MigrateError> {
    schema_version(conn.as_turso_connection()).await
}
