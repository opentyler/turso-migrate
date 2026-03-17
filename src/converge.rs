use std::path::Path;
use std::time::Instant;

use crate::diff::normalize_for_hash;
use crate::options::{ConvergeMode, ConvergeOptions, ConvergePolicy, ConvergeReport};
use crate::schema::SchemaSnapshot;
use crate::{MigrateError, compute_diff, generate_plan};

pub async fn converge(conn: &turso::Connection, schema_sql: &str) -> Result<(), MigrateError> {
    let options = ConvergeOptions {
        policy: ConvergePolicy::permissive(),
        ..Default::default()
    };
    converge_with_options(conn, schema_sql, &options).await?;
    Ok(())
}

pub async fn converge_with_options(
    conn: &turso::Connection,
    schema_sql: &str,
    options: &ConvergeOptions,
) -> Result<ConvergeReport, MigrateError> {
    let start = Instant::now();

    if schema_sql.trim().is_empty() {
        return Err(MigrateError::Schema("empty schema SQL".into()));
    }

    let normalized = normalize_for_hash(schema_sql);
    let schema_hash = blake3::hash(normalized.as_bytes()).to_hex().to_string();

    bootstrap_schema_meta(conn).await?;

    let stored_hash = get_meta(conn, "schema_hash").await?;
    let in_progress = get_meta(conn, "migration_in_progress").await?;

    let is_crash_recovery = in_progress.as_deref() == Some("1");

    if stored_hash.as_deref() == Some(schema_hash.as_str()) && !is_crash_recovery {
        if !detect_drift(conn).await? {
            tracing::debug!(hash = %schema_hash, "converge: fast-path, schema unchanged");
            return Ok(ConvergeReport::fast_path(start.elapsed()));
        }
        tracing::warn!("converge: schema drift detected, forcing slow-path");
    }

    if is_crash_recovery {
        tracing::warn!("converge: crash recovery detected, forcing slow-path");
    }

    set_meta(conn, "migration_in_progress", "1").await?;

    tracing::info!("converge: slow-path, computing diff");
    let desired = SchemaSnapshot::from_schema_sql(schema_sql).await?;
    let actual = SchemaSnapshot::from_connection(conn).await?;

    let diff = compute_diff(&desired, &actual);
    let had_ddl = !diff.is_empty();

    if had_ddl {
        check_policy(&diff, &options.policy)?;

        tracing::info!(
            tables_create = diff.tables_to_create.len(),
            tables_drop = diff.tables_to_drop.len(),
            tables_rebuild = diff.tables_to_rebuild.len(),
            columns_add = diff.columns_to_add.len(),
            "converge: generating migration plan"
        );

        let plan = generate_plan(&diff, &desired, &actual)?;

        if options.dry_run {
            tracing::info!("converge: dry-run mode, skipping execution");
            let mut all_stmts = plan.transactional_stmts.clone();
            all_stmts.extend(plan.non_transactional_stmts.clone());
            return Ok(ConvergeReport {
                mode: ConvergeMode::DryRun,
                tables_created: diff.tables_to_create.len(),
                tables_rebuilt: diff.tables_to_rebuild.len(),
                tables_dropped: diff.tables_to_drop.len(),
                columns_added: diff.columns_to_add.len(),
                indexes_changed: diff.indexes_to_create.len() + diff.fts_indexes_to_create.len(),
                views_changed: diff.views_to_create.len(),
                duration: start.elapsed(),
                plan_sql: all_stmts,
            });
        }

        tracing::info!(
            transactional = plan.transactional_stmts.len(),
            non_transactional = plan.non_transactional_stmts.len(),
            "converge: executing migration plan"
        );
        crate::execute::execute_plan_with_timeout(conn, &plan, options.busy_timeout).await?;
    }

    update_state_atomically(conn, &schema_hash, had_ddl).await?;

    let mode = if is_crash_recovery {
        ConvergeMode::CrashRecovery
    } else if had_ddl {
        ConvergeMode::SlowPath
    } else {
        ConvergeMode::NoOp
    };

    tracing::info!(mode = ?mode, elapsed_ms = start.elapsed().as_millis(), "converge: complete");

    Ok(ConvergeReport {
        mode,
        tables_created: diff.tables_to_create.len(),
        tables_rebuilt: diff.tables_to_rebuild.len(),
        tables_dropped: diff.tables_to_drop.len(),
        columns_added: diff.columns_to_add.len(),
        indexes_changed: diff.indexes_to_create.len() + diff.fts_indexes_to_create.len(),
        views_changed: diff.views_to_create.len(),
        duration: start.elapsed(),
        plan_sql: Vec::new(),
    })
}

fn check_policy(
    diff: &crate::diff::SchemaDiff,
    policy: &ConvergePolicy,
) -> Result<(), MigrateError> {
    if !policy.allow_table_drops && !diff.tables_to_drop.is_empty() {
        return Err(MigrateError::PolicyViolation {
            message: format!(
                "Would drop {} table(s): {}. Set allow_table_drops=true to permit.",
                diff.tables_to_drop.len(),
                diff.tables_to_drop.join(", ")
            ),
            blocked_operations: diff
                .tables_to_drop
                .iter()
                .map(|t| format!("DROP TABLE {t}"))
                .collect(),
        });
    }

    if let Some(max) = policy.max_tables_affected {
        let total =
            diff.tables_to_create.len() + diff.tables_to_drop.len() + diff.tables_to_rebuild.len();
        if total > max {
            return Err(MigrateError::PolicyViolation {
                message: format!(
                    "Would affect {total} table(s), exceeding max_tables_affected={max}."
                ),
                blocked_operations: vec![format!("{total} tables affected")],
            });
        }
    }

    Ok(())
}

async fn detect_drift(conn: &turso::Connection) -> Result<bool, MigrateError> {
    let result = conn.query("PRAGMA schema_version", ()).await;
    let current_sv = match result {
        Ok(mut rows) => {
            if let Ok(Some(row)) = rows.next().await {
                row.get::<i64>(0).unwrap_or(0)
            } else {
                return Ok(false);
            }
        }
        Err(_) => return Ok(false),
    };

    let stored_sv = get_meta(conn, "sqlite_schema_version").await?;
    if stored_sv.as_deref() == Some(&current_sv.to_string()) {
        return Ok(false);
    }

    tracing::info!(
        current = current_sv,
        stored = ?stored_sv,
        "converge: PRAGMA schema_version changed"
    );
    Ok(true)
}

pub async fn schema_fingerprint(conn: &turso::Connection) -> Result<String, MigrateError> {
    let mut rows = conn
        .query(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema \
             WHERE name NOT LIKE 'sqlite_%' \
               AND name NOT LIKE '_schema_meta%' \
               AND name NOT LIKE '_converge_new_%' \
             ORDER BY name",
            (),
        )
        .await?;

    let mut hasher = blake3::Hasher::new();
    while let Some(row) = rows.next().await? {
        let type_: String = row.get(0)?;
        let name: String = row.get(1)?;
        let sql: Option<String> = row.get(3)?;
        hasher.update(type_.as_bytes());
        hasher.update(name.as_bytes());
        if let Some(s) = sql {
            hasher.update(crate::diff::normalize_for_hash(&s).as_bytes());
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

async fn update_state_atomically(
    conn: &turso::Connection,
    schema_hash: &str,
    had_ddl: bool,
) -> Result<(), MigrateError> {
    conn.execute("BEGIN IMMEDIATE", ()).await?;

    if let Err(e) = async {
        set_meta(conn, "schema_hash", schema_hash).await?;
        delete_meta(conn, "migration_in_progress").await?;

        if had_ddl {
            increment_schema_version(conn).await?;
        }

        let sv_result = conn.query("PRAGMA schema_version", ()).await;
        if let Ok(mut rows) = sv_result {
            if let Ok(Some(row)) = rows.next().await {
                let sv: i64 = row.get(0).unwrap_or(0);
                set_meta(conn, "sqlite_schema_version", &sv.to_string()).await?;
            }
        }

        Ok::<(), MigrateError>(())
    }
    .await
    {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(e);
    }

    conn.execute("COMMIT", ()).await?;
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
