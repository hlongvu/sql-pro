//! End-to-end UI-flow tests against the local `dental_hub` PostgreSQL:
//! the exact code paths the app exercises — create_driver → list_tables →
//! list_columns (PK discovery) → execute (browse/query) → PendingChanges →
//! SqlStatementGenerator → DataWriteExecutor (parameterized, $1 style).
//!
//! Run with: cargo test -p sqlpro-drivers --test dental_hub_postgres -- --nocapture
//!
//! The write-path test uses a scratch table it creates and drops, so the
//! dental_hub data is never modified.

use std::collections::BTreeSet;

use sqlpro_core::change::{PendingChanges, RowID};
use sqlpro_core::connection::ConnectionConfig;
use sqlpro_core::database_type::DatabaseType;
use sqlpro_core::history::{QueryHistoryEntry, QueryHistoryStorage};
use sqlpro_core::sqlgen::{SqlStatementGenerator, DEFAULT_MARKER};
use sqlpro_core::value::CellValue;
use sqlpro_core::write::{
    engine_reports_real_counts, DataWriteExecutor, DataWritePlan, ParameterizedExecutor,
    TransactionOwner,
};

fn dental_hub_config() -> ConnectionConfig {
    let mut config = ConnectionConfig::new("dental_hub", DatabaseType::new(DatabaseType::POSTGRESQL));
    config.host = Some("127.0.0.1".into());
    config.port = Some(5432);
    config.username = Some("postgres".into());
    config.password = Some("postgres".into());
    config.database = Some("dental_hub".into());
    config
}

async fn driver() -> Option<Box<dyn ParameterizedExecutor>> {
    // Skip (don't fail) when the local database isn't running.
    if std::net::TcpStream::connect("127.0.0.1:5432").is_err() {
        eprintln!("skipping: no local postgres on 127.0.0.1:5432");
        return None;
    }
    let config = dental_hub_config();
    match sqlpro_drivers::create_driver(&config) {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipping: cannot connect to dental_hub: {e}");
            None
        }
    }
}

#[tokio::test]
async fn connects_and_browses_schema() {
    let Some(driver) = driver().await else { return };

    driver.ping().await.unwrap();

    let tables = driver.list_tables().await.unwrap();
    eprintln!("tables in dental_hub: {:?}", tables.iter().map(|t| (&t.name, t.is_view, t.row_estimate)).collect::<Vec<_>>());
    assert!(!tables.is_empty(), "dental_hub should have tables");

    // PK discovery per table, exactly as open_table does.
    for table in tables.iter().take(5) {
        let columns = driver.list_columns(&table.name).await.unwrap();
        let pks: Vec<&str> = columns.iter().filter(|c| c.is_primary_key).map(|c| c.name.as_str()).collect();
        eprintln!("  {} ({}) cols: {:?} pk: {:?}", table.name, if table.is_view { "view" } else { "table" },
            columns.iter().map(|c| format!("{} {}", c.name, c.data_type)).collect::<Vec<_>>(), pks);
        assert!(!columns.is_empty());
    }
}

#[tokio::test]
async fn browses_rows_like_clicking_a_table() {
    let Some(driver) = driver().await else { return };

    let tables = driver.list_tables().await.unwrap();
    let first = &tables[0].name;
    // The sidebar's click-to-browse SQL.
    let sql = format!("SELECT * FROM \"{first}\" LIMIT 500");
    let result = driver.execute(&sql).await.unwrap();
    assert!(result.is_query);
    eprintln!(
        "browse {}: {} rows, {} cols, duration {:?}",
        first,
        result.rows.len(),
        result.columns.len(),
        result.duration
    );
    for (ix, col) in result.columns.iter().enumerate().take(8) {
        let sample = result.rows.first().and_then(|r| r.get(ix)).map(|c| c.display());
        eprintln!("  {} [{}] = {:?}", col.name, col.data_type, sample);
    }
}

#[tokio::test]
async fn executes_analytical_queries() {
    let Some(driver) = driver().await else { return };

    // A query the editor would run with ⌘⏎.
    let result = driver
        .execute(
            "SELECT table_name, column_name, data_type \
             FROM information_schema.columns \
             WHERE table_schema = 'public' \
             ORDER BY table_name, ordinal_position",
        )
        .await
        .unwrap();
    assert!(result.is_query);
    assert!(!result.rows.is_empty());
    eprintln!("schema query returned {} rows", result.rows.len());

    // Multi-statement script (the SQL splitter path).
    let script = "SELECT 1 AS one; SELECT 2 AS two;";
    let result = driver.execute(script).await.unwrap();
    assert!(result.is_query);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], CellValue::Text("2".into()));
}

#[tokio::test]
async fn full_edit_pipeline_on_scratch_table() {
    let Some(driver) = driver().await else { return };
    assert!(engine_reports_real_counts(&DatabaseType::new(DatabaseType::POSTGRESQL)));

    // Scratch table so dental_hub data is untouched.
    driver
        .execute(
            "DROP TABLE IF EXISTS sqlpro_scratch; \
             CREATE TABLE sqlpro_scratch (id SERIAL PRIMARY KEY, name TEXT, note TEXT); \
             INSERT INTO sqlpro_scratch (name, note) VALUES ('alice', NULL), ('bob', 'keep');",
        )
        .await
        .unwrap();

    // What open_table recorded for the PK.
    let columns = driver.list_columns("sqlpro_scratch").await.unwrap();
    let pk: Vec<String> = columns.iter().filter(|c| c.is_primary_key).map(|c| c.name.clone()).collect();
    assert_eq!(pk, vec!["id".to_string()]);
    let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();

    // Browse rows (the grid's original_rows).
    let result = driver.execute("SELECT id, name, note FROM sqlpro_scratch ORDER BY id").await.unwrap();
    assert_eq!(result.rows.len(), 2);

    // Edit like the UI: update alice's name, edit the new row, delete bob.
    let mut pending = PendingChanges::new();

    // UPDATE: alice → alicia (keyed on id=1)
    let row0_id = RowID::Existing(0);
    pending.record_cell_change(
        row0_id,
        1,
        "name",
        CellValue::text("alice"),
        CellValue::text("alicia"),
        Some(result.rows[0].clone()),
        Default::default(),
    );

    // INSERT: a new row with a DEFAULT for the serial id
    let insert_id = RowID::Inserted(uuid::Uuid::new_v4());
    pending.record_row_insertion(
        insert_id,
        vec![
            CellValue::Text(DEFAULT_MARKER.into()),
            CellValue::text("carol"),
            CellValue::Null,
        ],
        Default::default(),
    );
    // Then edit the inserted row's note (goes into the INSERT, never an UPDATE).
    pending.record_cell_change(
        insert_id,
        2,
        "note",
        CellValue::Null,
        CellValue::text("inserted by sqlpro"),
        None,
        Default::default(),
    );

    // DELETE: bob (keyed on id=2) — recorded into the same pending set the
    // UI uses.
    pending.record_row_deletion(RowID::Existing(1), result.rows[1].clone(), Default::default());

    // The generated statements: $1 placeholders, qualified nothing (grid
    // edit), DELETE ordered by sequence.
    let generator = SqlStatementGenerator::new(
        "sqlpro_scratch",
        None,
        column_names,
        pk.clone(),
        DatabaseType::new(DatabaseType::POSTGRESQL),
        BTreeSet::new(),
        Default::default(),
        None,
        false,
        None,
    );
    let attributed = generator.generate_attributed_statements(
        &pending.changes,
        &pending.inserted_row_data,
        &pending.deleted_row_ids,
        &pending.inserted_row_ids,
    );
    for a in &attributed {
        eprintln!("plan: [{:?}] {}", a.kind, a.statement.sql);
    }
    assert_eq!(attributed.len(), 3);

    // Execute through the real parameterized write path.
    let real_counts = engine_reports_real_counts(&DatabaseType::new(DatabaseType::POSTGRESQL));
    let plan = DataWritePlan::from_attributed(attributed, true, TransactionOwner::App);
    let executor = DataWriteExecutor { driver: driver.as_ref(), real_counts };
    let outcome = executor.run(&plan).await;
    let result = match outcome {
        Ok(r) => r,
        Err(e) => {
            // Clean up the scratch table even when the save fails, so the
            // next run starts clean.
            let _ = driver.execute("DROP TABLE IF EXISTS sqlpro_scratch").await;
            panic!("save failed: {e}");
        }
    };
    for step in &result.executed {
        eprintln!("executed [{:?}] {} rows: {}", step.kind, step.rows_affected, step.sql);
    }
    assert_eq!(result.executed.len(), 3);

    // Verify server truth.
    let check = driver
        .execute("SELECT id, name, note FROM sqlpro_scratch ORDER BY id")
        .await
        .unwrap();
    eprintln!(
        "after save: {:?}",
        check.rows.iter().map(|r| r.iter().map(|c| c.display()).collect::<Vec<_>>()).collect::<Vec<_>>()
    );
    assert_eq!(check.rows.len(), 2); // bob deleted, carol inserted
    assert_eq!(check.rows[0][1], CellValue::Text("alicia".into()));
    assert_eq!(check.rows[1][1], CellValue::Text("carol".into()));
    assert_eq!(check.rows[1][2], CellValue::Text("inserted by sqlpro".into()));

    // Serial id: the server filled the DEFAULT — carol's id is 3.
    assert_eq!(check.rows[1][0], CellValue::Text("3".into()));

    driver.execute("DROP TABLE IF EXISTS sqlpro_scratch").await.unwrap();
}

#[tokio::test]
async fn keyless_update_verified_against_real_counts() {
    let Some(driver) = driver().await else { return };

    driver
        .execute(
            "DROP TABLE IF EXISTS sqlpro_scratch2; \
             CREATE TABLE sqlpro_scratch2 (a TEXT, b TEXT); \
             INSERT INTO sqlpro_scratch2 VALUES ('x', 'y');",
        )
        .await
        .unwrap();

    // Keyless update: WHERE matches every column, and the two-sided
    // real-count check must hold (matched exactly the 1 expected row).
    let generator = SqlStatementGenerator::new(
        "sqlpro_scratch2",
        None,
        vec!["a".into(), "b".into()],
        vec![],
        DatabaseType::new(DatabaseType::POSTGRESQL),
        BTreeSet::new(),
        Default::default(),
        None,
        false,
        None,
    );
    let mut change = sqlpro_core::change::RowChange::new(RowID::Existing(0), sqlpro_core::change::ChangeType::Update);
    change.cell_changes.push(sqlpro_core::change::CellChange::new(
        0,
        "a",
        CellValue::text("x"),
        CellValue::text("x2"),
    ));
    change.original_row = Some(vec![CellValue::text("x"), CellValue::text("y")]);
    let attributed = generator.generate_attributed_statements(
        std::slice::from_ref(&change),
        &Default::default(),
        &Default::default(),
        &Default::default(),
    );
    eprintln!("keyless plan: {}", attributed[0].statement.sql);
    assert!(attributed[0].statement.sql.contains("WHERE"));

    let plan = DataWritePlan::from_attributed(attributed, false, TransactionOwner::App);
    let executor = DataWriteExecutor { driver: driver.as_ref(), real_counts: true };
    let result = executor.run(&plan).await.unwrap();
    assert_eq!(result.executed[0].rows_affected, 1);

    let check = driver.execute("SELECT a FROM sqlpro_scratch2").await.unwrap();
    assert_eq!(check.rows[0][0], CellValue::Text("x2".into()));

    driver.execute("DROP TABLE IF EXISTS sqlpro_scratch2").await.unwrap();
}

#[tokio::test]
async fn query_history_records_dental_hub_queries() {
    let Some(driver) = driver().await else { return };
    let config = dental_hub_config();
    let sql = "SELECT current_database()";
    let started = std::time::Instant::now();
    let result = driver.execute(sql).await.unwrap();
    assert_eq!(result.rows[0][0], CellValue::Text("dental_hub".into()));

    // The UI records every run through this path.
    let history = QueryHistoryStorage::open_in_memory().unwrap();
    let mut entry = QueryHistoryEntry::new(sql, config.id.to_string(), &config.database_type);
    entry.execution_time = started.elapsed().as_secs_f64();
    entry.was_successful = true;
    entry.database_name = "dental_hub".into();
    history.record(&entry).unwrap();

    let filter = sqlpro_core::history::QueryHistoryFilter {
        connection_id: Some(config.id.to_string()),
        ..Default::default()
    };
    let page = history.fetch(&filter, None, 10).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].database_name, "dental_hub");
    assert_eq!(page.entries[0].statement_type, "select");
}
