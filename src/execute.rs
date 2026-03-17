use std::time::Duration;

use crate::error::MigrateError;
use crate::plan::MigrationPlan;

pub async fn execute_plan(
    conn: &turso::Connection,
    plan: &MigrationPlan,
) -> Result<(), MigrateError> {
    execute_plan_with_timeout(conn, plan, Duration::from_secs(5)).await
}

pub async fn execute_plan_with_timeout(
    conn: &turso::Connection,
    plan: &MigrationPlan,
    busy_timeout: Duration,
) -> Result<(), MigrateError> {
    if plan.is_empty() {
        return Ok(());
    }

    let timeout_ms = busy_timeout.as_millis();
    let _ = conn
        .execute(&format!("PRAGMA busy_timeout = {timeout_ms}"), ())
        .await;

    let has_rebuilds = !plan.rebuilt_tables.is_empty();

    if !plan.transactional_stmts.is_empty() {
        let (phase1, phase2): (Vec<String>, Vec<String>) = plan
            .transactional_stmts
            .iter()
            .cloned()
            .partition(|stmt| !is_create_view_or_trigger(stmt));

        run_ddl_transaction(conn, &phase1, has_rebuilds, "DDL").await?;
        run_non_transactional(conn, &plan.non_transactional_stmts, "FTS").await?;
        if !phase2.is_empty() {
            run_transaction(conn, &phase2, "views_triggers").await?;
        }
    } else {
        run_non_transactional(conn, &plan.non_transactional_stmts, "FTS").await?;
    }

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
        let _ = conn.execute("PRAGMA defer_foreign_keys = ON", ()).await;
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
    let result = conn.query("PRAGMA foreign_key_check", ()).await;
    let mut rows = match result {
        Ok(rows) => rows,
        Err(_) => return Ok(()),
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

fn is_create_view_or_trigger(stmt: &str) -> bool {
    let normalized = stmt.trim_start().to_lowercase();
    normalized.starts_with("create view")
        || normalized.starts_with("create materialized view")
        || normalized.starts_with("create trigger")
}
