use crate::error::MigrateError;

const DATA_VERSION: i64 = 0;

pub async fn converge_data(conn: &turso::Connection) -> Result<(), MigrateError> {
    let current = get_data_version(conn).await?;

    if current < DATA_VERSION {
        set_data_version(conn, DATA_VERSION).await?;
    }

    Ok(())
}

pub fn current_data_version() -> i64 {
    DATA_VERSION
}

async fn get_data_version(conn: &turso::Connection) -> Result<i64, MigrateError> {
    let mut rows = conn
        .query(
            "SELECT value FROM _schema_meta WHERE key = 'data_version'",
            (),
        )
        .await?;

    if let Some(row) = rows.next().await? {
        let val: String = row.get(0)?;
        Ok(val.parse::<i64>().unwrap_or(0))
    } else {
        Ok(0)
    }
}

async fn set_data_version(conn: &turso::Connection, version: i64) -> Result<(), MigrateError> {
    conn.execute(
        "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('data_version', ?1)",
        [version.to_string()],
    )
    .await?;
    Ok(())
}
