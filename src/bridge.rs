use crate::error::MigrateError;

pub async fn bridge_legacy(conn: &turso::Connection) -> Result<bool, MigrateError> {
    let has_legacy = table_exists(conn, "_turso_migrations").await?;
    if !has_legacy {
        return Ok(false);
    }

    let already_bridged = get_meta_value(conn, "legacy_complete").await?;
    if already_bridged.as_deref() == Some("1") {
        return Ok(false);
    }

    tracing::info!("Legacy _turso_migrations table detected, marking as bridged");
    set_meta_value(conn, "legacy_complete", "1").await?;

    Ok(true)
}

async fn table_exists(conn: &turso::Connection, name: &str) -> Result<bool, MigrateError> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [name],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

async fn get_meta_value(
    conn: &turso::Connection,
    key: &str,
) -> Result<Option<String>, MigrateError> {
    let mut rows = conn
        .query("SELECT value FROM _schema_meta WHERE key = ?1", [key])
        .await?;
    if let Some(row) = rows.next().await? {
        Ok(Some(row.get::<String>(0)?))
    } else {
        Ok(None)
    }
}

async fn set_meta_value(
    conn: &turso::Connection,
    key: &str,
    value: &str,
) -> Result<(), MigrateError> {
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES (?1, ?2)",
        [key, value],
    )
    .await?;
    Ok(())
}
