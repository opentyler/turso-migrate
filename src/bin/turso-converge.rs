use std::env;
use std::path::Path;

use turso_converge::{
    ConvergeOptions, ConvergePolicy, SchemaSnapshot, compute_diff, converge, converge_with_options,
    validate_schema,
};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let Some(cmd) = args.next() else {
        print_usage();
        return Err("missing command".to_string());
    };

    match cmd.as_str() {
        "validate" => {
            let schema_path = required_arg(args.next(), "<schema.sql>")?;
            let schema_sql = read_file(&schema_path)?;
            validate_schema(&schema_sql)
                .await
                .map_err(|e| e.to_string())?;
            println!("schema is valid");
        }
        "diff" => {
            let db_path = required_arg(args.next(), "<db-path>")?;
            let schema_path = required_arg(args.next(), "<schema.sql>")?;
            let schema_sql = read_file(&schema_path)?;
            let conn = open_local_connection(&db_path).await?;
            let desired = SchemaSnapshot::from_schema_sql(&schema_sql)
                .await
                .map_err(|e| e.to_string())?;
            let actual = SchemaSnapshot::from_connection(&conn)
                .await
                .map_err(|e| e.to_string())?;
            let diff = compute_diff(&desired, &actual);
            println!("{diff}");
        }
        "plan" => {
            let db_path = required_arg(args.next(), "<db-path>")?;
            let schema_path = required_arg(args.next(), "<schema.sql>")?;
            let schema_sql = read_file(&schema_path)?;
            let conn = open_local_connection(&db_path).await?;
            let options = ConvergeOptions {
                policy: ConvergePolicy::permissive(),
                dry_run: true,
                ..Default::default()
            };
            let report = converge_with_options(&conn, &schema_sql, &options)
                .await
                .map_err(|e| e.to_string())?;
            if report.plan_sql.is_empty() {
                println!("(no changes)");
            } else {
                for stmt in report.plan_sql {
                    println!("{};", stmt.trim_end_matches(';'));
                }
            }
        }
        "check" => {
            let db_path = required_arg(args.next(), "<db-path>")?;
            let schema_path = required_arg(args.next(), "<schema.sql>")?;
            let schema_sql = read_file(&schema_path)?;
            let conn = open_local_connection(&db_path).await?;
            let options = ConvergeOptions {
                policy: ConvergePolicy::permissive(),
                dry_run: true,
                ..Default::default()
            };
            let report = converge_with_options(&conn, &schema_sql, &options)
                .await
                .map_err(|e| e.to_string())?;
            if report.had_changes() {
                for stmt in report.plan_sql {
                    println!("{};", stmt.trim_end_matches(';'));
                }
                return Err("schema is not converged".to_string());
            }
            println!("schema is converged");
        }
        "dump" => {
            let db_path = required_arg(args.next(), "<db-path>")?;
            let conn = open_local_connection(&db_path).await?;
            let snapshot = SchemaSnapshot::from_connection(&conn)
                .await
                .map_err(|e| e.to_string())?;
            let sql = snapshot.to_sql();
            if sql.is_empty() {
                return Err("database has no user tables".to_string());
            }
            println!("{sql}");
        }
        "apply" => {
            let db_path = required_arg(args.next(), "<db-path>")?;
            let schema_path = required_arg(args.next(), "<schema.sql>")?;
            let schema_sql = read_file(&schema_path)?;
            let conn = open_local_connection(&db_path).await?;
            converge(&conn, &schema_sql)
                .await
                .map_err(|e| e.to_string())?;
            println!("schema converged");
        }
        "help" | "--help" | "-h" => {
            print_usage();
        }
        _ => {
            print_usage();
            return Err(format!("unknown command: {cmd}"));
        }
    }

    Ok(())
}

fn required_arg(arg: Option<String>, label: &str) -> Result<String, String> {
    arg.ok_or_else(|| format!("missing required argument {label}"))
}

fn read_file(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))
}

async fn open_local_connection(path: &str) -> Result<turso::Connection, String> {
    let db = turso::Builder::new_local(path)
        .experimental_index_method(true)
        .experimental_materialized_views(true)
        .experimental_triggers(true)
        .build()
        .await
        .map_err(|e| format!("failed to open database {}: {e}", Path::new(path).display()))?;

    db.connect().map_err(|e| format!("failed to connect: {e}"))
}

fn print_usage() {
    println!(
        "turso-converge <command> [args]\n\nCommands:\n  dump <db-path>\n  validate <schema.sql>\n  diff <db-path> <schema.sql>\n  plan <db-path> <schema.sql>\n  check <db-path> <schema.sql>\n  apply <db-path> <schema.sql>"
    );
}

#[cfg(test)]
mod tests {
    use super::open_local_connection;

    #[tokio::test(flavor = "multi_thread")]
    async fn open_local_connection_supports_trigger_ddl() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("cli_trigger_test.db");
        let db_path_str = db_path.to_string_lossy().to_string();

        let conn = open_local_connection(&db_path_str).await.unwrap();
        conn.execute("CREATE TABLE t (id TEXT PRIMARY KEY)", ())
            .await
            .unwrap();
        conn.execute(
            "CREATE TRIGGER trg_t AFTER INSERT ON t BEGIN SELECT 1; END",
            (),
        )
        .await
        .unwrap();
    }
}
