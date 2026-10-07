//! Read-only diagnostic: what the app sees for real dental_hub tables —
//! discovered PKs, decoded key values, and whether an UPDATE can be built.

use sqlpro_core::change::{PendingChanges, RowID};
use sqlpro_core::connection::ConnectionConfig;
use sqlpro_core::database_type::DatabaseType;
use sqlpro_core::sqlgen::SqlStatementGenerator;
use sqlpro_core::value::CellValue;
use sqlpro_core::write::{engine_reports_real_counts, DataWriteError};
use sqlpro_drivers::create_driver;

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
async fn app_view_of_real_tables() {
    let Ok(driver) = create_driver(&config()) else {
        eprintln!("dental_hub not reachable; skipping");
        return;
    };

    let tables = driver.list_tables().await.unwrap();
    println!("{} tables; checking the first 6:", tables.len());
    for table in tables.iter().take(6) {
        let columns = driver.list_columns(&table.name).await.unwrap();
        let pks: Vec<String> = columns
            .iter()
            .filter(|c| c.is_primary_key)
            .map(|c| c.name.clone())
            .collect();
        println!(
            "  table {:?} (view={}): {} columns, pk={:?}",
            table.name,
            table.is_view,
            columns.len(),
            pks
        );
        for column in columns.iter().take(4) {
            println!(
    "      column {:?} type={} nullable={} pk={}",
                column.name, column.data_type, column.nullable, column.is_primary_key
            );
        }

        // Browse like the app does and show how the key decodes.
        if let Ok(result) = driver
            .execute(&format!(
                "SELECT * FROM \"{}\" LIMIT 2",
                table.name.replace('"', "\"\"")
            ))
            .await
        {
            println!(
                "      browse: {} cols, {} rows",
                result.columns.len(),
                result.rows.len()
            );
            if let Some(row) = result.rows.first() {
                for (ix, cell) in row.iter().enumerate().take(4) {
                    println!("        [{}] = {:?}", result.columns[ix].name, cell);
                }
            }
        }
    }

    // Full app-shaped write attempt on a scratch clone of the first table
    // with a uuid pk: create scratch, stage an edit on a copy of one row,
    // generate statements, DON'T execute — just print what would run.
    let Some(target) = tables.iter().find(|t| !t.is_view) else { return };
    let columns = driver.list_columns(&target.name).await.unwrap();
    let pk: Vec<String> = columns
        .iter()
        .filter(|c| c.is_primary_key)
        .map(|c| c.name.clone())
        .collect();
    let browse = driver
        .execute(&format!(
            "SELECT * FROM \"{}\" LIMIT 1",
            target.name.replace('"', "\"\"")
        ))
        .await
        .unwrap();
    let col_names: Vec<String> = browse.columns.iter().map(|c| c.name.clone()).collect();
    let Some(original) = browse.rows.first().cloned() else { return };

    let mut pending = PendingChanges::new();
    let text_col = columns
        .iter()
        .position(|c| {
            !c.is_primary_key
                && matches!(c.data_type.as_str(), "text" | "character varying")
        })
        .unwrap_or(1.min(columns.len() - 1));
    pending.record_cell_change(
        RowID::Existing(0),
        text_col,
        &col_names[text_col],
        original.get(text_col).cloned().unwrap_or(CellValue::Null),
        CellValue::Text("sqlpro-diagnostic".to_string()),
        Some(original.clone()),
        Default::default(),
    );
    let snapshot = pending.snapshot();
    let generator = SqlStatementGenerator::new(
        target.name.clone(),
        None,
        col_names,
        pk.clone(),
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
    println!(
        "write check on {:?} pk={:?}: {} statement(s)",
        target.name,
        pk,
        attributed.len()
    );
    for statement in &attributed {
        println!("  SQL: {}", statement.statement.sql);
    }
    let _ = (engine_reports_real_counts, DataWriteError::Query("unused".into()));
}
