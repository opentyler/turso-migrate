use crate::error::MigrateError;
use crate::plan::MigrationPlan;

pub async fn execute_plan(
    conn: &turso::Connection,
    plan: &MigrationPlan,
) -> Result<(), MigrateError> {
    if plan.is_empty() {
        return Ok(());
    }

    if !plan.transactional_stmts.is_empty() {
        let (phase1, phase2): (Vec<String>, Vec<String>) = plan
            .transactional_stmts
            .iter()
            .cloned()
            .partition(|stmt| !is_create_view_or_trigger(stmt));

        run_transaction(conn, &phase1).await?;
        run_non_transactional(conn, &plan.non_transactional_stmts).await?;
        if !phase2.is_empty() {
            run_transaction(conn, &phase2).await?;
        }
    } else {
        run_non_transactional(conn, &plan.non_transactional_stmts).await?;
    }

    Ok(())
}

async fn run_transaction(conn: &turso::Connection, stmts: &[String]) -> Result<(), MigrateError> {
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
