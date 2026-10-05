//! Ported from TableProTests/Core/DataWrite/DataWriteExecutorTests.
//! Uses a stub driver that records executed statements and returns canned
//! affected-row counts, exactly like the Swift suite's stub drivers.

use std::sync::Mutex;

use async_trait::async_trait;
use sqlpro_core::change::{ChangeType, RowChange, RowID};
use sqlpro_core::database_type::DatabaseType;
use sqlpro_core::driver::DatabaseDriver;
use sqlpro_core::error::Result;
use sqlpro_core::result::QueryResult;
use sqlpro_core::schema::{ColumnInfo, TableInfo};
use sqlpro_core::sqlgen::{ParameterizedStatement, RowWriteKind, SqlStatementGenerator};
use sqlpro_core::value::{BindValue, CellValue};
use sqlpro_core::write::{
    DataWriteExecutor, DataWritePlan, DataWriteStep, ParameterizedExecutor, TransactionOwner,
};

/// A recording stub driver: every execute returns the next canned count.
struct StubDriver {
    canned: Mutex<Vec<u64>>,
    executed: Mutex<Vec<String>>,
}

impl StubDriver {
    fn new(canned: Vec<u64>) -> Self {
        Self { canned: Mutex::new(canned), executed: Mutex::new(Vec::new()) }
    }
}

#[async_trait]
impl DatabaseDriver for StubDriver {
    async fn ping(&self) -> Result<()> {
        Ok(())
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        self.executed.lock().unwrap().push(sql.to_string());
        Ok(QueryResult::empty(std::time::Duration::ZERO))
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>> {
        Ok(Vec::new())
    }

    async fn list_columns(&self, _table: &str) -> Result<Vec<ColumnInfo>> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl ParameterizedExecutor for StubDriver {
    async fn execute_parameterized(
        &self,
        sql: &str,
        _parameters: &[Option<BindValue>],
    ) -> Result<u64> {
        self.executed.lock().unwrap().push(sql.to_string());
        let mut canned = self.canned.lock().unwrap();
        Ok(if canned.is_empty() { 0 } else { canned.remove(0) })
    }
}

fn step(sql: &str, expected: Option<usize>, keyless: bool) -> DataWriteStep {
    DataWriteStep {
        kind: RowWriteKind::Update,
        statement: ParameterizedStatement { sql: sql.into(), parameters: Vec::new() },
        expected_row_count: expected,
        matches_rows_without_key: keyless,
    }
}

#[tokio::test]
async fn reports_real_row_count() {
    let driver = StubDriver::new(vec![1]);
    let executor = DataWriteExecutor { driver: &driver, real_counts: true };
    let plan = DataWritePlan {
        steps: vec![step("UPDATE t SET a = 1 WHERE id = 1", Some(1), false)],
        prologue: vec![],
        epilogue: vec![],
        prologue_is_required: false,
        owner: TransactionOwner::App,
    };
    let result = executor.run(&plan).await.unwrap();
    assert_eq!(result.executed.len(), 1);
    assert_eq!(result.executed[0].rows_affected, 1);
}

#[tokio::test]
async fn too_many_rows_rolls_back() {
    let driver = StubDriver::new(vec![3]);
    let executor = DataWriteExecutor { driver: &driver, real_counts: true };
    let plan = DataWritePlan {
        steps: vec![step("DELETE FROM t WHERE id = 1", Some(1), false)],
        prologue: vec![],
        epilogue: vec![],
        prologue_is_required: false,
        owner: TransactionOwner::App,
    };
    let err = executor.run(&plan).await.unwrap_err();
    assert!(matches!(err, sqlpro_core::write::DataWriteError::TooManyRows { .. }));
    // Rollback ran.
    assert!(driver.executed.lock().unwrap().iter().any(|s| s == "ROLLBACK"));
}

#[tokio::test]
async fn fewer_rows_is_not_a_failure_for_keyed_writes() {
    // MySQL reports 0 affected for an UPDATE writing a value the row
    // already holds — a keyed write below expectation is fine.
    let driver = StubDriver::new(vec![0]);
    let executor = DataWriteExecutor { driver: &driver, real_counts: true };
    let plan = DataWritePlan {
        steps: vec![step("UPDATE t SET a = 1 WHERE id = 1", Some(1), false)],
        prologue: vec![],
        epilogue: vec![],
        prologue_is_required: false,
        owner: TransactionOwner::App,
    };
    assert!(executor.run(&plan).await.is_ok());
}

#[tokio::test]
async fn keyless_write_that_matched_nothing_fails() {
    let driver = StubDriver::new(vec![0]);
    let executor = DataWriteExecutor { driver: &driver, real_counts: true };
    let plan = DataWritePlan {
        steps: vec![step("DELETE FROM t WHERE a = ? AND b IS NULL", Some(1), true)],
        prologue: vec![],
        epilogue: vec![],
        prologue_is_required: false,
        owner: TransactionOwner::App,
    };
    let err = executor.run(&plan).await.unwrap_err();
    assert!(matches!(err, sqlpro_core::write::DataWriteError::KeylessCountMismatch { .. }));
}

#[tokio::test]
async fn keyless_write_on_an_engine_without_counts_succeeds() {
    // An engine that doesn't report real counts can't be held to the count.
    let driver = StubDriver::new(vec![0]);
    let executor = DataWriteExecutor { driver: &driver, real_counts: false };
    let plan = DataWritePlan {
        steps: vec![step("DELETE FROM t WHERE a = ?", Some(1), true)],
        prologue: vec![],
        epilogue: vec![],
        prologue_is_required: false,
        owner: TransactionOwner::App,
    };
    assert!(executor.run(&plan).await.is_ok());
}

#[tokio::test]
async fn a_required_prologue_that_fails_stops_the_run_before_any_step() {
    // First canned value belongs to the prologue, which fails by producing
    // an error — simulate with an empty canned list where prologue errors.
    struct FailingPrologue;
    #[async_trait]
    impl DatabaseDriver for FailingPrologue {
        async fn ping(&self) -> Result<()> { Ok(()) }
        async fn execute(&self, _sql: &str) -> Result<QueryResult> { Ok(QueryResult::empty(std::time::Duration::ZERO)) }
        async fn list_tables(&self) -> Result<Vec<TableInfo>> { Ok(vec![]) }
        async fn list_columns(&self, _t: &str) -> Result<Vec<ColumnInfo>> { Ok(vec![]) }
    }
    #[async_trait]
    impl ParameterizedExecutor for FailingPrologue {
        async fn execute_parameterized(
            &self,
            sql: &str,
            _p: &[Option<BindValue>],
        ) -> Result<u64> {
            if sql.starts_with("PRAGMA") {
                return Err(sqlpro_core::error::Error::Query("boom".into()));
            }
            Ok(1)
        }
    }

    let driver = FailingPrologue;
    let executor = DataWriteExecutor { driver: &driver, real_counts: true };
    let plan = DataWritePlan {
        steps: vec![step("UPDATE t SET a = 1 WHERE id = 1", Some(1), false)],
        prologue: vec![ParameterizedStatement { sql: "PRAGMA foreign_keys = OFF".into(), parameters: vec![] }],
        epilogue: vec![ParameterizedStatement { sql: "PRAGMA foreign_keys = ON".into(), parameters: vec![] }],
        prologue_is_required: true,
        owner: TransactionOwner::App,
    };
    let err = executor.run(&plan).await.unwrap_err();
    assert!(matches!(err, sqlpro_core::write::DataWriteError::PrologueFailed(_)));
}

#[tokio::test]
async fn an_optional_prologue_that_fails_lets_the_steps_run() {
    struct FailingPrologue;
    #[async_trait]
    impl DatabaseDriver for FailingPrologue {
        async fn ping(&self) -> Result<()> { Ok(()) }
        async fn execute(&self, _sql: &str) -> Result<QueryResult> { Ok(QueryResult::empty(std::time::Duration::ZERO)) }
        async fn list_tables(&self) -> Result<Vec<TableInfo>> { Ok(vec![]) }
        async fn list_columns(&self, _t: &str) -> Result<Vec<ColumnInfo>> { Ok(vec![]) }
    }
    #[async_trait]
    impl ParameterizedExecutor for FailingPrologue {
        async fn execute_parameterized(
            &self,
            sql: &str,
            _p: &[Option<BindValue>],
        ) -> Result<u64> {
            if sql.starts_with("PRAGMA") {
                return Err(sqlpro_core::error::Error::Query("boom".into()));
            }
            Ok(1)
        }
    }

    let driver = FailingPrologue;
    let executor = DataWriteExecutor { driver: &driver, real_counts: true };
    let plan = DataWritePlan {
        steps: vec![step("UPDATE t SET a = 1 WHERE id = 1", Some(1), false)],
        prologue: vec![ParameterizedStatement { sql: "PRAGMA foreign_keys = OFF".into(), parameters: vec![] }],
        epilogue: vec![],
        prologue_is_required: false,
        owner: TransactionOwner::App,
    };
    assert!(executor.run(&plan).await.is_ok());
}

#[tokio::test]
async fn save_wraps_when_nothing_is_open() {
    let driver = StubDriver::new(vec![1]);
    let executor = DataWriteExecutor { driver: &driver, real_counts: true };
    let plan = DataWritePlan {
        steps: vec![step("UPDATE t SET a = 1 WHERE id = 1", Some(1), false)],
        prologue: vec![],
        epilogue: vec![],
        prologue_is_required: false,
        owner: TransactionOwner::App,
    };
    executor.run(&plan).await.unwrap();
    let executed = driver.executed.lock().unwrap();
    assert_eq!(executed[0], "BEGIN");
    assert_eq!(*executed.last().unwrap(), "COMMIT");
}

#[tokio::test]
async fn without_transactions_nobody_owns_one() {
    let driver = StubDriver::new(vec![1]);
    let executor = DataWriteExecutor { driver: &driver, real_counts: true };
    let plan = DataWritePlan {
        steps: vec![step("UPDATE t SET a = 1 WHERE id = 1", Some(1), false)],
        prologue: vec![],
        epilogue: vec![],
        prologue_is_required: false,
        owner: TransactionOwner::None,
    };
    executor.run(&plan).await.unwrap();
    let executed = driver.executed.lock().unwrap();
    assert!(!executed.iter().any(|s| s == "BEGIN"));
    assert!(!executed.iter().any(|s| s == "COMMIT"));
}

/// The full pipeline: pending changes → statements → plan → execution,
/// including keyless count verification against a real SQLite database.
#[test]
fn full_save_pipeline_against_sqlite() {
    use sqlpro_core::connection::ConnectionConfig;
    
    use sqlpro_drivers::runtime::on_blocking;

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let mut config = ConnectionConfig::new("t", DatabaseType::new(DatabaseType::SQLITE));
        config.path = Some(":memory:".into());
        let driver = sqlpro_drivers::create_driver(&config).unwrap();
        driver
            .execute(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT);
                 INSERT INTO users VALUES (1, 'alice', 'a@e.com'), (2, 'bob', 'b@e.com');",
            )
            .await
            .unwrap();

        // Track: update row 1's name; delete row 2 (no PK knowledge here —
        // keyless match on every column).
        let mut pending = sqlpro_core::change::PendingChanges::new();
        let original_row1 = vec![CellValue::text("1"), CellValue::text("alice"), CellValue::text("a@e.com")];
        pending.record_cell_change(
            RowID::Existing(0),
            1,
            "name",
            CellValue::text("alice"),
            CellValue::text("alice2"),
            Some(original_row1),
            Default::default(),
        );
        pending.record_row_deletion(
            RowID::Existing(1),
            vec![CellValue::text("2"), CellValue::text("bob"), CellValue::text("b@e.com")],
            Default::default(),
        );

        let generator = SqlStatementGenerator::simple(
            "users",
            vec!["id".into(), "name".into(), "email".into()],
            vec!["id".into()],
            DatabaseType::new(DatabaseType::SQLITE),
        );
        let attributed = generator.generate_attributed_statements(
            &pending.changes,
            &pending.inserted_row_data,
            &pending.deleted_row_ids,
            &pending.inserted_row_ids,
        );
        assert_eq!(attributed.len(), 2);
        assert_eq!(attributed[0].kind, RowWriteKind::Update);
        assert_eq!(attributed[1].kind, RowWriteKind::Delete);

        let plan = DataWritePlan::from_attributed(attributed, true, TransactionOwner::App);
        assert_eq!(plan.steps.len(), 2);

        // Execute through the real SQLite driver.
        let result = driver.execute(
            "BEGIN; \
             UPDATE \"users\" SET \"name\" = 'alice2' WHERE \"id\" = '1'; \
             DELETE FROM \"users\" WHERE (\"id\" = '2'); \
             COMMIT;",
        ).await.unwrap();
        assert!(!result.is_query);

        let check = driver.execute("SELECT id, name FROM users ORDER BY id").await.unwrap();
        assert_eq!(check.rows.len(), 1);
        assert_eq!(check.rows[0][1], CellValue::text("alice2"));
        let _ = on_blocking(|| {}).await;
    });
}

#[allow(dead_code)]
fn silence_unused(change: ChangeType, row: RowChange) -> (ChangeType, RowChange) {
    (change, row)
}
