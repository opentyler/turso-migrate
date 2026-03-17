use std::path::Path;

use crate::diff::normalize_for_hash;
use crate::schema::SchemaSnapshot;
use crate::{MigrateError, compute_diff, execute_plan, generate_plan};

pub async fn converge(conn: &turso::Connection, schema_sql: &str) -> Result<(), MigrateError> {
    if schema_sql.trim().is_empty() {
        return Err(MigrateError::Schema("empty schema SQL".into()));
    }

    let normalized = normalize_for_hash(schema_sql);
    let schema_hash = blake3::hash(normalized.as_bytes()).to_hex().to_string();

    bootstrap_schema_meta(conn).await?;

    let stored_hash = get_meta(conn, "schema_hash").await?;
    let in_progress = get_meta(conn, "migration_in_progress").await?;

    if stored_hash.as_deref() == Some(schema_hash.as_str()) && in_progress.as_deref() != Some("1") {
        return Ok(());
    }

    set_meta(conn, "migration_in_progress", "1").await?;

    let desired = SchemaSnapshot::from_schema_sql(schema_sql).await?;
    let actual = SchemaSnapshot::from_connection(conn).await?;

    let diff = compute_diff(&desired, &actual);
    let had_ddl = !diff.is_empty();
    if had_ddl {
        let plan = generate_plan(&diff, &desired, &actual)?;
        execute_plan(conn, &plan).await?;
    }

    set_meta(conn, "schema_hash", schema_hash.as_str()).await?;
    delete_meta(conn, "migration_in_progress").await?;

    if had_ddl {
        increment_schema_version(conn).await?;
    }

    Ok(())
}

pub async fn converge_from_path(
    conn: &turso::Connection,
    path: impl AsRef<Path>,
) -> Result<(), MigrateError> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path).map_err(|source| MigrateError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    converge(conn, &contents).await
}

pub async fn schema_version(conn: &turso::Connection) -> Result<u32, MigrateError> {
    let mut rows = conn
        .query("SELECT version FROM schema_version LIMIT 1", ())
        .await?;

    if let Some(row) = rows.next().await? {
        let version: i32 = row.get(0)?;
        Ok(version as u32)
    } else {
        Ok(0)
    }
}

async fn increment_schema_version(conn: &turso::Connection) -> Result<(), MigrateError> {
    let current = schema_version(conn).await.unwrap_or(0);
    let next = current + 1;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();

    conn.execute("DELETE FROM schema_version", ()).await?;
    conn.execute(
        "INSERT INTO schema_version (version, updated_at) VALUES (?1, ?2)",
        (next as i64, now.as_str()),
    )
    .await?;

    Ok(())
}

async fn bootstrap_schema_meta(conn: &turso::Connection) -> Result<(), MigrateError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        (),
    )
    .await?;
    Ok(())
}

async fn get_meta(conn: &turso::Connection, key: &str) -> Result<Option<String>, MigrateError> {
    let mut rows = conn
        .query("SELECT value FROM _schema_meta WHERE key = ?1", [key])
        .await?;

    if let Some(row) = rows.next().await? {
        let value: String = row.get(0)?;
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

async fn set_meta(conn: &turso::Connection, key: &str, value: &str) -> Result<(), MigrateError> {
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES (?1, ?2)",
        [key, value],
    )
    .await?;
    Ok(())
}

async fn delete_meta(conn: &turso::Connection, key: &str) -> Result<(), MigrateError> {
    conn.execute("DELETE FROM _schema_meta WHERE key = ?1", [key])
        .await?;
    Ok(())
}
