use std::time::Duration;

use crate::error::MigrateError;
use crate::plan::MigrationPlan;

pub async fn execute_plan(
    conn: &turso::Connection,
    plan: &MigrationPlan,
) -> Result<(), MigrateError> {
    execute_plan_with_timeout(conn, plan, Duration::from_secs(5), "").await
}

/// Set PRAGMA busy_timeout before acquiring lease or executing DDL.
pub async fn set_busy_timeout(
    conn: &turso::Connection,
    timeout: Duration,
) -> Result<(), MigrateError> {
    let ms = timeout.as_millis();
    conn.execute(&format!("PRAGMA busy_timeout = {ms}"), ())
        .await
        .map_err(|e| MigrateError::Statement {
            stmt: format!("PRAGMA busy_timeout = {ms}"),
            source: e,
            phase: "setup".to_string(),
        })?;
    Ok(())
}

pub async fn execute_plan_with_timeout(
    conn: &turso::Connection,
    plan: &MigrationPlan,
    busy_timeout: Duration,
    lease_id: &str,
) -> Result<(), MigrateError> {
    if plan.is_empty() {
        return Ok(());
    }

    set_busy_timeout(conn, busy_timeout).await?;

    let has_rebuilds = !plan.rebuilt_tables.is_empty();

    if !plan.transactional_stmts.is_empty() {
        let (phase1, phase2): (Vec<String>, Vec<String>) = plan
            .transactional_stmts
            .iter()
            .cloned()
            .partition(|stmt| !is_create_view_or_trigger(stmt));

        run_ddl_transaction(conn, &phase1, has_rebuilds, "DDL").await?;

        verify_and_refresh_lease(conn, lease_id).await?;
        run_non_transactional(conn, &plan.non_transactional_stmts, "FTS").await?;

        if !phase2.is_empty() {
            verify_and_refresh_lease(conn, lease_id).await?;
            run_views_and_triggers(conn, &phase2).await?;
        }
    } else {
        verify_and_refresh_lease(conn, lease_id).await?;
        run_non_transactional(conn, &plan.non_transactional_stmts, "FTS").await?;
    }

    Ok(())
}

async fn verify_and_refresh_lease(
    conn: &turso::Connection,
    lease_id: &str,
) -> Result<(), MigrateError> {
    if lease_id.is_empty() {
        return Ok(());
    }

    let owner = conn
        .query(
            "SELECT value FROM _schema_meta WHERE key = 'migration_owner'",
            (),
        )
        .await;

    let current_owner = match owner {
        Ok(mut rows) => match rows.next().await {
            Ok(Some(row)) => row.get::<String>(0).ok(),
            _ => None,
        },
        Err(_) => None,
    };

    if current_owner.as_deref() != Some(lease_id) {
        return Err(MigrateError::Schema(
            "Migration lease was stolen by another process between execution phases. \
             Aborting to prevent concurrent DDL corruption."
                .to_string(),
        ));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let new_expiry = now + 300;
    let _ = conn
        .execute(
            "INSERT OR REPLACE INTO _schema_meta (key, value) VALUES ('migration_lease_until', ?1)",
            [new_expiry.to_string().as_str()],
        )
        .await;

    Ok(())
}

async fn run_ddl_transaction(
    conn: &turso::Connection,
    stmts: &[String],
    has_rebuilds: bool,
    phase: &str,
) -> Result<(), MigrateError> {
    if stmts.is_empty() {
        return Ok(());
    }

    if has_rebuilds {
        if let Err(e) = conn.execute("PRAGMA defer_foreign_keys = ON", ()).await {
            tracing::warn!(
                error = %e,
                "defer_foreign_keys not supported; self-referential FK rebuilds may fail"
            );
        }
    }

    conn.execute("BEGIN IMMEDIATE", ()).await?;

    for stmt in stmts {
        if let Err(err) = conn.execute(stmt, ()).await {
            rollback(conn).await;
            return Err(MigrateError::Statement {
                stmt: stmt.clone(),
                source: err,
                phase: phase.to_string(),
            });
        }
    }

    if has_rebuilds {
        if let Err(violation) = check_foreign_keys(conn).await {
            rollback(conn).await;
            return Err(violation);
        }
    }

    if let Err(err) = conn.execute("COMMIT", ()).await {
        rollback(conn).await;
        return Err(err.into());
    }

    Ok(())
}

async fn check_foreign_keys(conn: &turso::Connection) -> Result<(), MigrateError> {
    let mut rows = match conn.query("PRAGMA foreign_key_check", ()).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "foreign_key_check not supported; skipping FK validation");
            return Ok(());
        }
    };

    if let Ok(Some(row)) = rows.next().await {
        let table: String = row.get(0).unwrap_or_default();
        let rowid: i64 = row.get(1).unwrap_or(0);
        let parent: String = row.get(2).unwrap_or_default();
        return Err(MigrateError::ForeignKeyViolation {
            table,
            rowid,
            parent,
        });
    }

    Ok(())
}

async fn run_transaction(
    conn: &turso::Connection,
    stmts: &[String],
    phase: &str,
) -> Result<(), MigrateError> {
    if stmts.is_empty() {
        return Ok(());
    }

    conn.execute("BEGIN IMMEDIATE", ()).await?;

    for stmt in stmts {
        if let Err(err) = conn.execute(stmt, ()).await {
            rollback(conn).await;
            return Err(MigrateError::Statement {
                stmt: stmt.clone(),
                source: err,
                phase: phase.to_string(),
            });
        }
    }

    if let Err(err) = conn.execute("COMMIT", ()).await {
        rollback(conn).await;
        return Err(err.into());
    }

    Ok(())
}

async fn rollback(conn: &turso::Connection) {
    let _ = conn.execute("ROLLBACK", ()).await;
}

async fn run_non_transactional(
    conn: &turso::Connection,
    stmts: &[String],
    phase: &str,
) -> Result<(), MigrateError> {
    for stmt in stmts {
        let batched = if stmt.trim_end().ends_with(';') {
            stmt.clone()
        } else {
            format!("{stmt};")
        };
        conn.execute_batch(&batched)
            .await
            .map_err(|source| MigrateError::Statement {
                stmt: stmt.clone(),
                source,
                phase: phase.to_string(),
            })?;
    }
    Ok(())
}

async fn run_views_and_triggers(
    conn: &turso::Connection,
    stmts: &[String],
) -> Result<(), MigrateError> {
    if stmts.is_empty() {
        return Ok(());
    }

    let mut view_stmts = Vec::new();
    let mut trigger_stmts = Vec::new();
    let mut passthrough = Vec::new();

    for stmt in stmts {
        if is_create_view(stmt) {
            view_stmts.push(stmt.clone());
        } else if is_create_trigger(stmt) {
            trigger_stmts.push(stmt.clone());
        } else {
            passthrough.push(stmt.clone());
        }
    }

    run_views_fixed_point(conn, &view_stmts).await?;
    run_transaction(conn, &trigger_stmts, "triggers").await?;
    run_transaction(conn, &passthrough, "views_triggers").await?;
    Ok(())
}

async fn run_views_fixed_point(
    conn: &turso::Connection,
    views: &[String],
) -> Result<(), MigrateError> {
    if views.is_empty() {
        return Ok(());
    }

    let mut remaining = views.to_vec();
    let max_rounds = views.len().saturating_add(1);

    for _ in 0..max_rounds {
        if remaining.is_empty() {
            return Ok(());
        }

        conn.execute("BEGIN IMMEDIATE", ()).await?;
        let mut next_round = Vec::new();
        let mut progressed = false;

        for stmt in remaining {
            match conn.execute(&stmt, ()).await {
                Ok(_) => {
                    progressed = true;
                }
                Err(source) => {
                    if is_missing_dependency_error(&source) {
                        next_round.push(stmt);
                    } else {
                        rollback(conn).await;
                        return Err(MigrateError::Statement {
                            stmt,
                            source,
                            phase: "views".to_string(),
                        });
                    }
                }
            }
        }

        if !progressed {
            rollback(conn).await;
            return Err(MigrateError::Schema(
                "Unable to resolve view creation order due to unresolved dependencies".to_string(),
            ));
        }

        if let Err(err) = conn.execute("COMMIT", ()).await {
            rollback(conn).await;
            return Err(err.into());
        }

        remaining = next_round;
    }

    Err(MigrateError::Schema(
        "Exceeded maximum rounds while resolving view dependencies".to_string(),
    ))
}

fn is_missing_dependency_error(err: &turso::Error) -> bool {
    let lower = err.to_string().to_ascii_lowercase();
    lower.contains("no such table") || lower.contains("no such view")
}

fn is_create_view_or_trigger(stmt: &str) -> bool {
    is_create_view(stmt) || is_create_trigger(stmt)
}

fn is_create_view(stmt: &str) -> bool {
    classify_create_stmt(stmt)
        .map(|kind| kind == "view")
        .unwrap_or(false)
}

fn is_create_trigger(stmt: &str) -> bool {
    classify_create_stmt(stmt)
        .map(|kind| kind == "trigger")
        .unwrap_or(false)
}

fn classify_create_stmt(stmt: &str) -> Option<&'static str> {
    let normalized = strip_leading_sql_comments(stmt);
    if normalized.is_empty() {
        return None;
    }

    let lower = normalized.to_ascii_lowercase();
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    if tokens.first() != Some(&"create") {
        return None;
    }

    let mut idx = 1;
    if matches!(tokens.get(idx), Some(&"temp") | Some(&"temporary")) {
        idx += 1;
    }

    match tokens.get(idx) {
        Some(&"view") => Some("view"),
        Some(&"materialized") if tokens.get(idx + 1) == Some(&"view") => Some("view"),
        Some(&"trigger") => Some("trigger"),
        _ => None,
    }
}

fn strip_leading_sql_comments(input: &str) -> &str {
    let mut rest = input;
    loop {
        let trimmed = rest.trim_start();
        if let Some(after) = trimmed.strip_prefix("--") {
            if let Some(pos) = after.find('\n') {
                rest = &after[pos + 1..];
                continue;
            }
            return "";
        }

        if let Some(after) = trimmed.strip_prefix("/*") {
            if let Some(pos) = after.find("*/") {
                rest = &after[pos + 2..];
                continue;
            }
            return "";
        }

        return trimmed;
    }
}

#[cfg(test)]
mod tests {
    use super::{is_create_trigger, is_create_view};

    #[test]
    fn classify_temp_view() {
        assert!(is_create_view("CREATE TEMP VIEW v AS SELECT 1"));
    }

    #[test]
    fn classify_temporary_trigger() {
        assert!(is_create_trigger(
            "CREATE TEMPORARY TRIGGER trg AFTER INSERT ON t BEGIN SELECT 1; END"
        ));
    }

    #[test]
    fn classify_with_if_not_exists() {
        assert!(is_create_view("CREATE VIEW IF NOT EXISTS v AS SELECT 1"));
        assert!(is_create_trigger(
            "CREATE TRIGGER IF NOT EXISTS trg AFTER INSERT ON t BEGIN SELECT 1; END"
        ));
    }

    #[test]
    fn classify_with_leading_comments() {
        let view_stmt = "-- comment\nCREATE TEMP VIEW v AS SELECT 1";
        let trigger_stmt = "/* block */\nCREATE TRIGGER trg AFTER INSERT ON t BEGIN SELECT 1; END";
        assert!(is_create_view(view_stmt));
        assert!(is_create_trigger(trigger_stmt));
    }

    #[test]
    fn classify_does_not_false_positive_on_create_table() {
        assert!(!is_create_view(
            "CREATE TABLE view_metadata (id TEXT PRIMARY KEY)"
        ));
        assert!(!is_create_trigger(
            "CREATE TABLE trigger_log (id TEXT PRIMARY KEY)"
        ));
    }
}
