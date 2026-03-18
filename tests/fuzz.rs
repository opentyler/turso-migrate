use turso_converge::{SchemaSnapshot, compute_diff, converge};

async fn empty_db() -> (turso::Database, turso::Connection) {
    let db = turso::Builder::new_local(":memory:")
        .experimental_index_method(true)
        .experimental_materialized_views(true)
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    (db, conn)
}

fn next_lcg(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    *seed
}

fn schema_from_seed(seed: u64) -> String {
    let mut s = seed;
    let mut cols = vec!["id TEXT PRIMARY KEY".to_string()];
    for name in ["title", "status", "owner", "tag", "note"] {
        if next_lcg(&mut s) % 2 == 0 {
            cols.push(format!("{name} TEXT"));
        }
    }

    if cols.len() == 1 {
        cols.push("payload TEXT".to_string());
    }

    let mut sql = format!(
        "CREATE TABLE fuzz ({});\nCREATE TABLE schema_version (version INTEGER NOT NULL, updated_at TEXT NOT NULL);",
        cols.join(", ")
    );

    let candidates: Vec<String> = cols
        .iter()
        .filter_map(|c| c.split_whitespace().next())
        .filter(|name| !name.eq_ignore_ascii_case("id"))
        .map(ToString::to_string)
        .collect();

    if !candidates.is_empty() && next_lcg(&mut s) % 2 == 0 {
        let idx = (next_lcg(&mut s) as usize) % candidates.len();
        let col = &candidates[idx];
        sql.push_str(&format!("\nCREATE INDEX idx_fuzz_{col} ON fuzz({col});"));
    }

    sql
}

#[tokio::test(flavor = "multi_thread")]
async fn deterministic_fuzz_schemas_converge_and_stabilize() {
    let (_db, conn) = empty_db().await;

    for seed in 1u64..=40 {
        let schema = schema_from_seed(seed);

        converge(&conn, &schema)
            .await
            .unwrap_or_else(|e| panic!("converge failed for seed {seed}: {e}"));
        converge(&conn, &schema)
            .await
            .unwrap_or_else(|e| panic!("idempotent converge failed for seed {seed}: {e}"));

        let desired = SchemaSnapshot::from_schema_sql(&schema).await.unwrap();
        let actual = SchemaSnapshot::from_connection(&conn).await.unwrap();
        let diff = compute_diff(&desired, &actual);
        assert!(
            diff.is_empty(),
            "schema drift after deterministic fuzz seed {seed}: {diff}"
        );
    }
}
