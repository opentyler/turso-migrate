use std::path::Path;
use std::time::Instant;

use crate::diff::{compute_diff_with_hints, normalize_for_hash};
use crate::options::{
    ConvergeMode, ConvergeOptions, ConvergePolicy, ConvergeReport, DataMigration,
    DestructiveChangeSet, Failpoint,
};
use crate::schema::{Capabilities, SchemaSnapshot};
use crate::{MigrateError, generate_plan};

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

    if is_read_only(conn).await? {
        return Err(MigrateError::ReadOnly);
    }

    let normalized = normalize_for_hash(schema_sql);
    let schema_hash = blake3::hash(normalized.as_bytes()).to_hex().to_string();

    if let Err(err) = bootstrap_schema_meta(conn).await {
        if is_read_only(conn).await.unwrap_or(false) || is_read_only_error(&err) {
            return Err(MigrateError::ReadOnly);
        }
        return Err(err);
    }

    let stored_hash = get_meta(conn, "schema_hash").await?;
    let in_progress = get_meta(conn, "migration_in_progress").await?;

    let is_crash_recovery = in_progress.as_deref() == Some("1");
    let has_pending_data_migrations =
        pending_data_migrations(conn, &options.data_migrations).await?;

    if stored_hash.as_deref() == Some(schema_hash.as_str()) && !is_crash_recovery {
        if !has_pending_data_migrations && !detect_drift(conn).await? {
            tracing::debug!(hash = %schema_hash, "converge: fast-path, schema unchanged");
            return Ok(ConvergeReport::fast_path(start.elapsed()));
        }
        if has_pending_data_migrations {
            tracing::info!(
                pending = options.data_migrations.len(),
                "converge: pending data migrations, forcing execution path"
            );
        } else {
            tracing::warn!("converge: schema drift detected, forcing slow-path");
        }
    }

    if is_crash_recovery {
        tracing::warn!("converge: crash recovery detected, forcing slow-path");
    }

    let lease_id = acquire_lease(conn).await?;

    let result = run_slow_path(
        conn,
        schema_sql,
        &schema_hash,
        options,
        is_crash_recovery,
        start,
    )
    .await;

    if let Err(err) = &result {
        maybe_cleanup_pre_ddl_failure(conn, &lease_id, err).await;
    }

    release_lease(conn, &lease_id).await;

    result
}

async fn run_slow_path(
    conn: &turso::Connection,
    schema_sql: &str,
    schema_hash: &str,
    options: &ConvergeOptions,
    is_crash_recovery: bool,
    start: Instant,
) -> Result<ConvergeReport, MigrateError> {
    check_failpoint(options.failpoint, Failpoint::BeforeIntrospect)?;
    set_meta(conn, "migration_phase", "introspect").await?;

    tracing::info!("converge: slow-path, computing diff");
    let desired = SchemaSnapshot::from_schema_sql(schema_sql).await?;
    let actual = SchemaSnapshot::from_connection(conn).await?;

    let diff = compute_diff_with_hints(&desired, &actual, &options.rename_hints);
    let had_ddl = !diff.is_empty();

    let mut data_migrations_applied = 0usize;

    if had_ddl {
        check_policy(&diff, &options.policy)?;

        let destructive = extract_destructive_changes(&diff);

        if destructive.has_changes()
            && let Some(target) = &options.backup_before_destructive
        {
            write_schema_backup(target, &actual.to_sql()).await?;
        }

        if let Some(hook) = &options.pre_destructive_hook {
            if destructive.has_changes() {
                hook(&destructive).map_err(|message| MigrateError::PreDestructiveHookRejected {
                    message,
                    blocked_operations: destructive.blocked_operations(),
                })?;
            }
        }

        let caps = Capabilities::detect(conn).await.unwrap_or_default();
        validate_features(&desired, &caps)?;

        tracing::info!(
            tables_create = diff.tables_to_create.len(),
            tables_drop = diff.tables_to_drop.len(),
            tables_rebuild = diff.tables_to_rebuild.len(),
            columns_add = diff.columns_to_add.len(),
            columns_drop = diff.columns_to_drop.len(),
            "converge: generating migration plan"
        );

        let plan = generate_plan(&diff, &desired, &actual)?;

        if options.dry_run {
            tracing::info!("converge: dry-run mode, skipping execution");
            clear_migration_markers(conn).await?;
            let mut all_stmts = plan.transactional_stmts.clone();
            all_stmts.extend(plan.non_transactional_stmts.clone());
            return Ok(ConvergeReport {
                mode: ConvergeMode::DryRun,
                tables_created: diff.tables_to_create.len(),
                tables_rebuilt: diff.tables_to_rebuild.len(),
                tables_dropped: diff.tables_to_drop.len(),
                columns_added: diff.columns_to_add.len(),
                columns_dropped: diff.columns_to_drop.len(),
                columns_renamed: diff.columns_to_rename.len(),
                indexes_changed: diff.indexes_to_create.len() + diff.fts_indexes_to_create.len(),
                views_changed: diff.views_to_create.len(),
                data_migrations_applied: 0,
                duration: start.elapsed(),
                plan_sql: all_stmts,
            });
        }

        let prev_schema_sql = actual.to_sql();
        set_meta(conn, "previous_schema_sql", &prev_schema_sql).await?;

        set_meta(conn, "migration_phase", "ddl").await?;

        check_failpoint(options.failpoint, Failpoint::BeforeExecute)?;

        tracing::info!(
            transactional = plan.transactional_stmts.len(),
            non_transactional = plan.non_transactional_stmts.len(),
            "converge: executing migration plan"
        );
        crate::execute::execute_plan_with_timeout(conn, &plan, options.busy_timeout).await?;

        check_failpoint(options.failpoint, Failpoint::AfterExecuteBeforeState)?;

        set_meta(conn, "migration_phase", "complete").await?;
    }

    if !options.data_migrations.is_empty() {
        data_migrations_applied = apply_data_migrations(conn, &options.data_migrations).await?;
    }

    update_state_atomically(conn, schema_hash, had_ddl).await?;

    let mode = if is_crash_recovery {
        ConvergeMode::CrashRecovery
    } else if had_ddl || data_migrations_applied > 0 {
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
        columns_dropped: diff.columns_to_drop.len(),
        columns_renamed: diff.columns_to_rename.len(),
        indexes_changed: diff.indexes_to_create.len() + diff.fts_indexes_to_create.len(),
        views_changed: diff.views_to_create.len(),
        data_migrations_applied,
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

    if !policy.allow_column_drops && !diff.columns_to_drop.is_empty() {
        return Err(MigrateError::PolicyViolation {
            message: format!(
                "Would drop {} column(s). Set allow_column_drops=true to permit.",
                diff.columns_to_drop.len(),
            ),
            blocked_operations: diff
                .columns_to_drop
                .iter()
                .map(|(t, c)| format!("DROP COLUMN {t}.{c}"))
                .collect(),
        });
    }

    if !policy.allow_table_rebuilds && !diff.tables_to_rebuild.is_empty() {
        return Err(MigrateError::PolicyViolation {
            message: format!(
                "Would rebuild {} table(s). Set allow_table_rebuilds=true to permit.",
                diff.tables_to_rebuild.len()
            ),
            blocked_operations: diff
                .tables_to_rebuild
                .iter()
                .map(|t| format!("REBUILD TABLE {t}"))
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

fn validate_features(desired: &SchemaSnapshot, caps: &Capabilities) -> Result<(), MigrateError> {
    let has_fts = desired.indexes.values().any(|i| i.is_fts);
    let has_materialized = desired.views.values().any(|v| v.is_materialized);
    let has_vector = desired.tables.values().any(|t| {
        t.columns
            .iter()
            .any(|c| c.col_type.to_ascii_lowercase().starts_with("vector"))
    });

    if has_fts && !caps.has_fts_module {
        return Err(MigrateError::UnsupportedFeature(
            "Schema uses FTS indexes but the target connection lacks the FTS module. \
             Ensure .experimental_index_method(true) is set on your turso::Builder."
                .into(),
        ));
    }
    if has_materialized && !caps.has_materialized_views {
        return Err(MigrateError::UnsupportedFeature(
            "Schema uses materialized views but the target connection doesn't support them. \
             Ensure .experimental_materialized_views(true) is set on your turso::Builder."
                .into(),
        ));
    }
    if has_vector && !caps.has_vector_module {
        return Err(MigrateError::UnsupportedFeature(
            "Schema uses vector columns but the target connection lacks the vector module.".into(),
        ));
    }
    Ok(())
}

const LEASE_TTL_SECS: u64 = 300;

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn acquire_lease(conn: &turso::Connection) -> Result<String, MigrateError> {
    let lease_id = format!("{}_{}", std::process::id(), epoch_secs());
    let now = epoch_secs();
    let expiry = now + LEASE_TTL_SECS;

    conn.execute("BEGIN IMMEDIATE", ()).await?;

    let existing_owner = get_meta(conn, "migration_owner").await?;
    let existing_expiry = get_meta(conn, "migration_lease_until").await?;

    let lease_active = if let (Some(_owner), Some(exp_str)) = (&existing_owner, &existing_expiry) {
        let exp: u64 = exp_str.parse().unwrap_or(0);
        exp > now
    } else {
        false
    };

    if lease_active {
        let exp: u64 = existing_expiry
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let remaining = exp.saturating_sub(now);
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(MigrateError::MigrationBusy {
            owner: existing_owner.unwrap_or_default(),
            remaining_secs: remaining,
        });
    }

    set_meta(conn, "migration_owner", &lease_id).await?;
    set_meta(conn, "migration_lease_until", &expiry.to_string()).await?;
    set_meta(conn, "migration_in_progress", "1").await?;
    conn.execute("COMMIT", ()).await?;

    Ok(lease_id)
}

async fn release_lease(conn: &turso::Connection, lease_id: &str) {
    if let Ok(Some(current)) = get_meta(conn, "migration_owner").await {
        if current == lease_id {
            let _ = delete_meta(conn, "migration_owner").await;
            let _ = delete_meta(conn, "migration_lease_until").await;
        }
    }
}

async fn clear_migration_markers(conn: &turso::Connection) -> Result<(), MigrateError> {
    delete_meta(conn, "migration_in_progress").await?;
    delete_meta(conn, "migration_phase").await?;
    Ok(())
}

async fn maybe_cleanup_pre_ddl_failure(
    conn: &turso::Connection,
    lease_id: &str,
    err: &MigrateError,
) {
    let phase = get_meta(conn, "migration_phase").await.ok().flatten();
    if !is_pre_ddl_failure(err, phase.as_deref()) {
        return;
    }

    let owner = get_meta(conn, "migration_owner").await.ok().flatten();
    if owner.as_deref() != Some(lease_id) {
        return;
    }

    if conn.execute("BEGIN IMMEDIATE", ()).await.is_err() {
        return;
    }

    let cleanup_result = clear_migration_markers(conn).await;

    if cleanup_result.is_err() {
        let _ = conn.execute("ROLLBACK", ()).await;
        return;
    }

    let _ = conn.execute("COMMIT", ()).await;
}

fn is_pre_ddl_failure(err: &MigrateError, phase: Option<&str>) -> bool {
    match phase {
        Some("complete") => false,
        Some("ddl") => {
            matches!(err, MigrateError::InjectedFailure { failpoint } if failpoint == Failpoint::BeforeExecute.as_str())
        }
        _ => true,
    }
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
        clear_migration_markers(conn).await?;

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

pub async fn rollback_to_previous(conn: &turso::Connection) -> Result<(), MigrateError> {
    bootstrap_schema_meta(conn).await?;
    let prev = get_meta(conn, "previous_schema_sql").await?;
    match prev {
        Some(sql) if !sql.trim().is_empty() => converge(conn, &sql).await,
        _ => Err(MigrateError::Schema(
            "No previous schema stored. Rollback requires at least one prior migration.".into(),
        )),
    }
}

pub async fn validate_schema(schema_sql: &str) -> Result<(), MigrateError> {
    if schema_sql.trim().is_empty() {
        return Err(MigrateError::Schema("empty schema SQL".into()));
    }
    SchemaSnapshot::validate(schema_sql)
        .await
        .map_err(|e| MigrateError::Schema(format!("Schema validation failed: {e}")))
}

pub async fn converge_multi(
    conn: &turso::Connection,
    schema_parts: &[&str],
) -> Result<(), MigrateError> {
    let combined = schema_parts.join("\n");
    converge(conn, &combined).await
}

pub async fn converge_multi_with_options(
    conn: &turso::Connection,
    schema_parts: &[&str],
    options: &ConvergeOptions,
) -> Result<ConvergeReport, MigrateError> {
    let combined = schema_parts.join("\n");
    converge_with_options(conn, &combined, options).await
}

pub async fn is_read_only(conn: &turso::Connection) -> Result<bool, MigrateError> {
    let mut rows = conn.query("PRAGMA query_only", ()).await?;
    if let Some(row) = rows.next().await? {
        let value: i64 = row.get(0).unwrap_or(0);
        Ok(value != 0)
    } else {
        Ok(false)
    }
}

pub async fn converge_from_path(
    conn: &turso::Connection,
    path: impl AsRef<Path>,
) -> Result<(), MigrateError> {
    let path = path.as_ref();
    let contents = tokio::fs::read_to_string(path)
        .await
        .map_err(|source| MigrateError::Io {
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
        let version: i64 = row.get(0)?;
        u32::try_from(version)
            .map_err(|_| MigrateError::Schema(format!("invalid schema_version value: {version}")))
    } else {
        Ok(0)
    }
}

async fn increment_schema_version(conn: &turso::Connection) -> Result<(), MigrateError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL)",
        (),
    )
    .await?;

    let current = schema_version(conn).await?;
    let next = current
        .checked_add(1)
        .ok_or_else(|| MigrateError::Schema("schema_version overflow".to_string()))?;
    let now = epoch_secs().to_string();

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

fn extract_destructive_changes(diff: &crate::diff::SchemaDiff) -> DestructiveChangeSet {
    DestructiveChangeSet {
        tables_to_drop: diff.tables_to_drop.clone(),
        columns_to_drop: diff.columns_to_drop.clone(),
        tables_to_rebuild: diff.tables_to_rebuild.clone(),
    }
}

fn check_failpoint(configured: Option<Failpoint>, target: Failpoint) -> Result<(), MigrateError> {
    if configured == Some(target) {
        return Err(MigrateError::InjectedFailure {
            failpoint: target.as_str().to_string(),
        });
    }
    Ok(())
}

fn is_read_only_error(err: &MigrateError) -> bool {
    let lower = err.to_string().to_ascii_lowercase();
    lower.contains("readonly")
        || lower.contains("read-only")
        || lower.contains("attempt to write a readonly database")
}

async fn write_schema_backup(path_hint: &Path, schema_sql: &str) -> Result<(), MigrateError> {
    let meta = tokio::fs::metadata(path_hint).await;
    let treat_as_dir = meta
        .as_ref()
        .map(|m| m.is_dir())
        .unwrap_or_else(|_| path_hint.extension().is_none());

    let output_path = if treat_as_dir {
        tokio::fs::create_dir_all(path_hint)
            .await
            .map_err(|source| MigrateError::Io {
                path: path_hint.to_path_buf(),
                source,
            })?;
        path_hint.join(format!("turso_migrate_backup_{}.sql", epoch_secs()))
    } else {
        if let Some(parent) = path_hint.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| MigrateError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }
        path_hint.to_path_buf()
    };

    tokio::fs::write(&output_path, schema_sql)
        .await
        .map_err(|source| MigrateError::Io {
            path: output_path,
            source,
        })
}

async fn apply_data_migrations(
    conn: &turso::Connection,
    migrations: &[DataMigration],
) -> Result<usize, MigrateError> {
    let mut applied = 0usize;

    for migration in migrations {
        if migration.id.trim().is_empty() {
            return Err(MigrateError::Schema(
                "data migration id must not be empty".to_string(),
            ));
        }
        let key = format!("data_migration:{}", migration.id);
        if get_meta(conn, &key).await?.is_some() {
            continue;
        }

        conn.execute("BEGIN IMMEDIATE", ()).await?;
        let mut failed = None;

        for stmt in &migration.statements {
            if let Err(source) = conn.execute(stmt, ()).await {
                failed = Some((stmt.clone(), source));
                break;
            }
        }

        if let Some((stmt, source)) = failed {
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(MigrateError::Statement {
                stmt,
                source,
                phase: "data_migration".to_string(),
            });
        }

        if let Err(err) = async {
            set_meta(conn, &key, &epoch_secs().to_string()).await?;
            conn.execute("COMMIT", ()).await?;
            Ok::<(), MigrateError>(())
        }
        .await
        {
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(err);
        }
        applied += 1;
    }

    Ok(applied)
}

async fn pending_data_migrations(
    conn: &turso::Connection,
    migrations: &[DataMigration],
) -> Result<bool, MigrateError> {
    for migration in migrations {
        if migration.id.trim().is_empty() {
            return Err(MigrateError::Schema(
                "data migration id must not be empty".to_string(),
            ));
        }
        let key = format!("data_migration:{}", migration.id);
        if get_meta(conn, &key).await?.is_none() {
            return Ok(true);
        }
    }
    Ok(false)
}
