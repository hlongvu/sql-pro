//! Repro: the app's save path against a dental_hub-shaped table — UUID
//! primary key plus timestamp, edited the way the UI stages changes
//! (CellValue::Text through PendingChanges::record_cell_change).

use sqlpro_core::change::{PendingChanges, RowID};
use sqlpro_core::connection::ConnectionConfig;
use sqlpro_core::database_type::DatabaseType;
use sqlpro_core::sqlgen::SqlStatementGenerator;
use sqlpro_core::value::CellValue;
use sqlpro_core::write::{
    engine_reports_real_counts, DataWriteExecutor, DataWritePlan, ParameterizedExecutor,
    TransactionOwner,
};

fn config() -> ConnectionConfig {
    let mut config = ConnectionConfig::new(
        "dental_hub (local)".to_string(),
        DatabaseType::new(DatabaseType::POSTGRESQL),
    );
    config.host = Some("127.0.0.1".into());
    config.port = Some(5432);
    config.username = Some("postgres".into());
    config.password = Some("postgres".into());
    config.database = Some("dental_hub".into());
    config
}

#[tokio::test(flavor = "multi_thread")]
async fn dental_hub_type_inventory() {
    let config = config();
    let Ok(driver) = sqlpro_drivers::create_driver(&config) else {
        eprintln!("dental_hub not reachable; skipping");
        return;
    };
    let result = driver
        .execute(
            "SELECT data_type, count(*)::text AS n FROM information_schema.columns \
             WHERE table_schema = 'public' GROUP BY 1 ORDER BY 2 DESC",
        )
        .await
        .unwrap();
    for row in &result.rows {
        println!(
            "{:>28} {}",
            row[0].display(),
            row[1].display()
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_dental_hub_column_type_writes_through() {
    let config = config();
    let Ok(driver) = sqlpro_drivers::create_driver(&config) else {
        eprintln!("dental_hub not reachable; skipping");
        return;
    };
    driver
        .execute(
            "DROP TABLE IF EXISTS sqlpro_types_scratch; \
             DROP TYPE IF EXISTS sqlpro_visit_status; \
             CREATE TYPE sqlpro_visit_status AS ENUM ('planned', 'done'); \
             CREATE TABLE sqlpro_types_scratch ( \
                 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), \
                 name CHARACTER VARYING, \
                 visit_at TIMESTAMP WITH TIME ZONE, \
                 created_at TIMESTAMP, \
                 on_date DATE, \
                 balance NUMERIC, \
                 quantity INTEGER, \
                 active BOOLEAN, \
                 status sqlpro_visit_status, \
                 meta JSONB \
             ); \
             INSERT INTO sqlpro_types_scratch (name, visit_at, created_at, on_date, balance, quantity, active, status, meta) \
             VALUES ('alice', '2026-01-15 09:30:00.123+00:00', '2026-01-15 09:30:00', '2026-01-15', \
                     25760.0000, 3, true, 'planned', '{\"a\": 1}');",
        )
        .await
        .unwrap();

    let browse = driver
        .execute("SELECT * FROM sqlpro_types_scratch")
        .await
        .unwrap();
    let columns: Vec<String> = browse.columns.iter().map(|c| c.name.clone()).collect();
    let original = browse.rows[0].clone();
    println!("row decodes as: {original:?}");

    // Edit EVERY column the way the UI does: staged text values.
    let edits: Vec<(&str, String)> = vec![
        ("name", "bob".to_string()),
        ("visit_at", "2026-02-20 14:45:30.5 UTC".to_string()),
        ("created_at", "2026-02-20 14:45:30".to_string()),
        ("on_date", "2026-03-01".to_string()),
        ("balance", "12345.6700".to_string()),
        ("quantity", "9".to_string()),
        ("active", "false".to_string()),
        ("status", "done".to_string()),
        ("meta", "{\"b\": [2]}".to_string()),
    ];

    let mut pending = PendingChanges::new();
    for (name, value) in &edits {
        let col_ix = columns.iter().position(|c| c == name).unwrap();
        let changed = pending.record_cell_change(
            RowID::Existing(0),
            col_ix,
            name,
            original.get(col_ix).cloned().unwrap_or(CellValue::Null),
            CellValue::Text(value.clone()),
            Some(original.clone()),
            Default::default(),
        );
        assert!(changed, "staging {name} should record a change");
    }

    let snapshot = pending.snapshot();
    let generator = SqlStatementGenerator::new(
        "sqlpro_types_scratch".to_string(),
        None,
        columns.clone(),
        vec!["id".to_string()],
        DatabaseType::new(DatabaseType::POSTGRESQL),
        Default::default(),
        Default::default(),
        None,
        false,
        None,
    );
    let attributed = generator.generate_attributed_statements(
        &snapshot.changes,
        &snapshot.inserted_row_data,
        &snapshot.deleted_row_ids,
        &snapshot.inserted_row_ids,
    );
    assert!(!attributed.is_empty(), "statements must generate");
    let plan = DataWritePlan::from_attributed(attributed, true, TransactionOwner::App);
    let executor = DataWriteExecutor {
        driver: driver.as_ref(),
        real_counts: engine_reports_real_counts(&DatabaseType::new(DatabaseType::POSTGRESQL)),
    };
    executor
        .run(&plan)
        .await
        .unwrap_or_else(|e| panic!("multi-type write failed: {e}"));

    let check = driver
        .execute("SELECT name, visit_at, balance, active, status FROM sqlpro_types_scratch")
        .await
        .unwrap();
    println!("after write: {:?}", check.rows[0]);
    assert_eq!(check.rows[0][0], CellValue::Text("bob".to_string()));
    assert_eq!(check.rows[0][2], CellValue::Text("12345.6700".to_string()));
    assert_eq!(check.rows[0][3], CellValue::Text("false".to_string()));
    assert_eq!(check.rows[0][4], CellValue::Text("done".to_string()));

    driver
        .execute("DROP TABLE IF EXISTS sqlpro_types_scratch; DROP TYPE IF EXISTS sqlpro_visit_status;")
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn enum_and_array_columns_decode_as_text() {
    let config = config();
    let Ok(driver) = sqlpro_drivers::create_driver(&config) else {
        eprintln!("dental_hub not reachable; skipping");
        return;
    };
    driver
        .execute(
            "DROP TABLE IF EXISTS sqlpro_enum_scratch; \
             DROP TYPE IF EXISTS sqlpro_tooth_status; \
             CREATE TYPE sqlpro_tooth_status AS ENUM ('healthy', 'treated'); \
             CREATE TABLE sqlpro_enum_scratch ( \
                 id SERIAL PRIMARY KEY, \
                 status sqlpro_tooth_status, \
                 tags TEXT[] \
             ); \
             INSERT INTO sqlpro_enum_scratch (status, tags) \
             VALUES ('treated', ARRAY['molar', 'left']);",
        )
        .await
        .unwrap();

    let result = driver
        .execute("SELECT status, tags FROM sqlpro_enum_scratch")
        .await
        .unwrap();
    println!("enum: {:?}", result.rows[0][0]);
    println!("array: {:?}", result.rows[0][1]);
    assert_eq!(result.rows[0][0], CellValue::Text("treated".to_string()));
    assert_eq!(result.rows[0][1], CellValue::Text("{molar,left}".to_string()));

    driver
        .execute("DROP TABLE IF EXISTS sqlpro_enum_scratch; DROP TYPE IF EXISTS sqlpro_tooth_status;")
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn uuid_pk_update_staged_like_the_ui() {
    let config = config();
    if sqlpro_drivers::create_driver(&config).is_err() {
        eprintln!("dental_hub not reachable; skipping");
        return;
    }
    let driver = sqlpro_drivers::create_driver(&config).unwrap();

    driver
        .execute(
            "DROP TABLE IF EXISTS sqlpro_uuid_scratch; \
             CREATE TABLE sqlpro_uuid_scratch (id UUID PRIMARY KEY, name TEXT, visit_at TIMESTAMP); \
             INSERT INTO sqlpro_uuid_scratch (id, name, visit_at) VALUES \
             (gen_random_uuid(), 'alice', '2026-01-15 09:30:00');",
        )
        .await
        .unwrap();

    let result = driver
        .execute("SELECT id, name, visit_at FROM sqlpro_uuid_scratch")
        .await
        .unwrap();
    let columns: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
    let original = result.rows[0].clone();
    println!("columns: {columns:?}");
    println!("original row: {original:?}");

    // The UI stages the new value as CellValue::Text.
    let mut pending = PendingChanges::new();
    let changed = pending.record_cell_change(
        RowID::Existing(0),
        1,
        &columns[1],
        original.get(1).cloned().unwrap_or(CellValue::Null),
        CellValue::Text("bob".to_string()),
        Some(original.clone()),
        Default::default(),
    );
    assert!(changed, "edit should stage");

    let snapshot = pending.snapshot();
    let generator = SqlStatementGenerator::new(
        "sqlpro_uuid_scratch".to_string(),
        None,
        columns.clone(),
        vec!["id".to_string()],
        DatabaseType::new(DatabaseType::POSTGRESQL),
        Default::default(),
        Default::default(),
        None,
        false,
        None,
    );
    let attributed = generator.generate_attributed_statements(
        &snapshot.changes,
        &snapshot.inserted_row_data,
        &snapshot.deleted_row_ids,
        &snapshot.inserted_row_ids,
    );
    for statement in &attributed {
        println!("SQL: {}", statement.statement.sql);
        println!("  params: {:?}", statement.statement.parameters);
    }

    let plan = DataWritePlan::from_attributed(attributed, true, TransactionOwner::App);
    let executor = DataWriteExecutor {
        driver: driver.as_ref(),
        real_counts: engine_reports_real_counts(&DatabaseType::new(DatabaseType::POSTGRESQL)),
    };
    match executor.run(&plan).await {
        Ok(result) => {
            println!("executed {} statement(s)", result.executed.len());
            assert!(
                !result.executed.is_empty(),
                "expected at least one UPDATE statement"
            );
        }
        Err(e) => {
            panic!("write failed: {e}");
        }
    }

    let check = driver
        .execute("SELECT name FROM sqlpro_uuid_scratch")
        .await
        .unwrap();
    assert_eq!(check.rows[0][0], CellValue::Text("bob".to_string()), "row should be updated");
    driver.execute("DROP TABLE IF EXISTS sqlpro_uuid_scratch").await.unwrap();
}
