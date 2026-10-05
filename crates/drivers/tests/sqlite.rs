//! End-to-end driver tests against a real bundled SQLite.

use sqlpro_core::value::CellValue;
use sqlpro_core::connection::ConnectionConfig;
use sqlpro_core::database_type::DatabaseType;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn sqlite_execute_and_browse() {
    let rt = runtime();
    rt.block_on(async {
        let mut config = ConnectionConfig::new("test", DatabaseType::new(DatabaseType::SQLITE));
        config.path = Some(":memory:".into());
        let driver = sqlpro_drivers::create_driver(&config).unwrap();

        driver
            .execute(
                "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, score REAL, note TEXT);
                 INSERT INTO t (name, score, note) VALUES
                     ('a', 1.5, NULL), ('b', 2.0, 'hello');",
            )
            .await
            .unwrap();

        let result = driver.execute("SELECT id, name, score, note FROM t ORDER BY id").await.unwrap();
        assert!(result.is_query);
        assert_eq!(result.columns.len(), 4);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0][1], CellValue::Text("a".into()));
        assert_eq!(result.rows[0][2], CellValue::Text("1.5".into()));
        assert!(result.rows[0][3].is_null());
        assert_eq!(result.rows[1][3], CellValue::Text("hello".into()));

        let tables = driver.list_tables().await.unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].name, "t");

        let columns = driver.list_columns("t").await.unwrap();
        assert_eq!(columns.len(), 4);
        assert!(columns[0].is_primary_key);

        // Ping and multi-statement write accounting
        driver.ping().await.unwrap();
        let write = driver
            .execute("INSERT INTO t (name) VALUES ('c'); INSERT INTO t (name) VALUES ('d'); UPDATE t SET score = 3 WHERE name = 'c'; DELETE FROM t WHERE name = 'd';")
            .await
            .unwrap();
        assert!(!write.is_query);
        assert_eq!(write.rows_affected, 4);
    });
}

#[cfg(feature = "postgres")]
#[test]
fn postgres_config_validation() {
    use sqlpro_core::error::Error;

    let rt = runtime();
    rt.block_on(async {
        let mut config = ConnectionConfig::new("bad", DatabaseType::new(DatabaseType::POSTGRESQL));
        config.host = Some("127.0.0.1".into());
        config.port = Some(1); // nothing listens here
        let driver = sqlpro_drivers::create_driver(&config);
        match driver {
            Err(Error::Connection(_)) => {}
            Err(e) => panic!("expected connection error, got: {e}"),
            Ok(_) => panic!("expected connection error, got Ok"),
        }
    });
}
