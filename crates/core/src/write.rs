//! Data write execution: run a save plan against a driver with the same
//! verification semantics as TablePro's `DataWriteExecutor`.
//!
//! Key rules ported from `KeyedWriteVerification` / `DataWriteRowCounts`:
//! - Keyed writes require the expected count on engines with reliable counts.
//!   The sole exception is a MySQL no-op UPDATE, which reports zero changed
//!   rows even though the keyed row matched.
//! - Keyless writes are also verified two-sided on reliable-count engines.
//! - Prologue/epilogue run OUTSIDE the transaction (SQLite `PRAGMA
//!   foreign_keys` is a no-op inside one); the epilogue is attempted after
//!   success and every failure path.

use async_trait::async_trait;

use crate::driver::DatabaseDriver;
use crate::error::{Error, Result};
use crate::sqlgen::{ParameterizedStatement, RowWriteKind};
use crate::value::BindValue;

/// Engines whose `rows_affected` reflects reality (TablePro's
/// `DataWriteRowCounts.enginesReportingRealCounts`).
pub fn engine_reports_real_counts(database_type: &crate::database_type::DatabaseType) -> bool {
    matches!(
        database_type.driver_id(),
        "mysql" | "postgresql" | "sqlite" | "mssql" | "oracle"
    )
}

#[derive(Debug, Clone)]
pub struct DataWriteStep {
    pub kind: RowWriteKind,
    pub statement: ParameterizedStatement,
    /// The row count the user asked this statement to touch, when known.
    pub expected_row_count: Option<usize>,
    /// True when the statement matches rows by every column instead of a
    /// key, so verification is two-sided on real-count engines.
    pub matches_rows_without_key: bool,
}

/// Who owns the write transaction (TablePro's `WriteTransactionOwner`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionOwner {
    /// The executor opens BEGIN/COMMIT itself.
    App,
    /// The user's session already opened one — never commit or roll back.
    Session,
    /// The engine has no transaction support.
    None,
}

#[derive(Debug, Clone)]
pub struct DataWritePlan {
    pub steps: Vec<DataWriteStep>,
    /// Statements that must run before the transaction opens.
    pub prologue: Vec<ParameterizedStatement>,
    /// Statements that run after commit/rollback either way.
    pub epilogue: Vec<ParameterizedStatement>,
    /// A required prologue failure stops the run; an optional one (e.g. FK
    /// toggles) is a courtesy that can be skipped.
    pub prologue_is_required: bool,
    pub owner: TransactionOwner,
}

impl DataWritePlan {
    pub fn from_attributed(
        statements: Vec<crate::sqlgen::AttributedStatement>,
        has_primary_key: bool,
        owner: TransactionOwner,
    ) -> Self {
        let steps = statements
            .into_iter()
            .map(|a| {
                let row_count = a.row_count();
                let kind = a.kind;
                let matches_rows_without_key = !has_primary_key;
                DataWriteStep {
                    kind,
                    statement: a.statement,
                    expected_row_count: Some(row_count),
                    matches_rows_without_key,
                }
            })
            .collect();
        Self {
            steps,
            prologue: Vec::new(),
            epilogue: Vec::new(),
            prologue_is_required: false,
            owner,
        }
    }
}

/// Outcome of one executed step.
#[derive(Debug, Clone)]
pub struct StepOutcome {
    pub kind: RowWriteKind,
    pub sql: String,
    pub rows_affected: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DataWriteResult {
    pub executed: Vec<StepOutcome>,
    /// Set when some statements ran but the save failed (no transaction
    /// wrapped them, or the rollback itself failed).
    pub partial_commit: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum DataWriteError {
    #[error("statement would affect more rows than requested: {sql} (expected {expected}, got {actual})")]
    TooManyRows {
        sql: String,
        expected: usize,
        actual: u64,
    },

    #[error("keyed write matched {actual} of {expected} expected rows: {sql}")]
    KeyedCountMismatch {
        sql: String,
        expected: usize,
        actual: u64,
    },

    #[error("keyless write matched {actual} of {expected} expected rows: {sql}")]
    KeylessCountMismatch {
        sql: String,
        expected: usize,
        actual: u64,
    },

    #[error("required prologue failed: {0}")]
    PrologueFailed(String),

    #[error("query failed: {0}")]
    Query(String),
}

/// A write-scoped physical database session.
///
/// The default implementation delegates to the driver, preserving existing
/// engines. Drivers backed by a pool override session creation so BEGIN,
/// every parameterized step, and COMMIT/ROLLBACK cannot hop connections.
#[async_trait]
pub trait ParameterizedSession: Send {
    async fn execute(&mut self, sql: &str) -> Result<()>;

    async fn execute_parameterized(
        &mut self,
        sql: &str,
        parameters: &[Option<BindValue>],
    ) -> Result<u64>;
}

struct DriverSession<'a> {
    driver: &'a dyn ParameterizedExecutor,
}

#[async_trait]
impl ParameterizedSession for DriverSession<'_> {
    async fn execute(&mut self, sql: &str) -> Result<()> {
        self.driver.execute(sql).await.map(|_| ())
    }

    async fn execute_parameterized(
        &mut self,
        sql: &str,
        parameters: &[Option<BindValue>],
    ) -> Result<u64> {
        self.driver.execute_parameterized(sql, parameters).await
    }
}

/// Trait for executing statements with bound parameters, used by the write
/// path. Implemented per engine.
#[async_trait]
pub trait ParameterizedExecutor: DatabaseDriver {
    async fn execute_parameterized(
        &self,
        sql: &str,
        parameters: &[Option<BindValue>],
    ) -> Result<u64>;

    async fn write_session(&self) -> Result<Box<dyn ParameterizedSession + '_>>;
}

pub fn default_write_session<'a>(
    driver: &'a dyn ParameterizedExecutor,
) -> Box<dyn ParameterizedSession + 'a> {
    Box::new(DriverSession { driver })
}

pub struct DataWriteExecutor<'a> {
    pub driver: &'a dyn ParameterizedExecutor,
    /// Whether the engine reports real affected counts.
    pub real_counts: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CountMismatch {
    TooMany,
    KeyedTooFew,
    KeylessTooFew,
}

impl<'a> DataWriteExecutor<'a> {
    pub async fn run(
        &self,
        plan: &DataWritePlan,
    ) -> std::result::Result<DataWriteResult, DataWriteError> {
        let mut result = DataWriteResult::default();

        let active_steps: Vec<&DataWriteStep> = plan
            .steps
            .iter()
            .filter(|s| !s.statement.sql.trim().is_empty())
            .collect();
        if active_steps.is_empty() {
            return Ok(result);
        }

        let outcome = self.run_active_steps(plan, &active_steps, &mut result).await;
        self.run_epilogue(plan).await;
        outcome.map(|_| result)
    }

    async fn run_active_steps(
        &self,
        plan: &DataWritePlan,
        active_steps: &[&DataWriteStep],
        result: &mut DataWriteResult,
    ) -> std::result::Result<(), DataWriteError> {
        // Prologue and epilogue intentionally use the general driver session:
        // SQLite foreign-key PRAGMAs are ineffective inside a transaction.
        for statement in &plan.prologue {
            if let Err(e) = self
                .driver
                .execute_parameterized(&statement.sql, &statement.parameters)
                .await
            {
                if plan.prologue_is_required {
                    return Err(DataWriteError::PrologueFailed(e.to_string()));
                }
            }
        }

        let mut session = self
            .driver
            .write_session()
            .await
            .map_err(|e| DataWriteError::Query(e.to_string()))?;
        let wrapped = plan.owner == TransactionOwner::App;
        if wrapped {
            session
                .execute("BEGIN")
                .await
                .map_err(|e| DataWriteError::Query(e.to_string()))?;
        }

        for step in active_steps {
            let actual = match session
                .execute_parameterized(&step.statement.sql, &step.statement.parameters)
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    self.handle_failure(session.as_mut(), wrapped).await;
                    return Err(DataWriteError::Query(e.to_string()));
                }
            };

            result.executed.push(StepOutcome {
                kind: step.kind,
                sql: step.statement.sql.clone(),
                rows_affected: actual,
            });

            if let Some((expected, mismatch)) = self.count_mismatch(step, actual) {
                self.handle_failure(session.as_mut(), wrapped).await;
                return Err(match mismatch {
                    CountMismatch::TooMany => DataWriteError::TooManyRows {
                        sql: step.statement.sql.clone(),
                        expected,
                        actual,
                    },
                    CountMismatch::KeyedTooFew => DataWriteError::KeyedCountMismatch {
                        sql: step.statement.sql.clone(),
                        expected,
                        actual,
                    },
                    CountMismatch::KeylessTooFew => DataWriteError::KeylessCountMismatch {
                        sql: step.statement.sql.clone(),
                        expected,
                        actual,
                    },
                });
            }
        }

        if wrapped {
            if let Err(e) = session.execute("COMMIT").await {
                // A failed COMMIT has an unknown outcome. Try to close the
                // transaction and retain the existing partial-commit signal
                // when even that cannot be confirmed.
                if session.execute("ROLLBACK").await.is_err() {
                    result.partial_commit = true;
                }
                return Err(DataWriteError::Query(e.to_string()));
            }
        }

        Ok(())
    }

    fn count_mismatch(&self, step: &DataWriteStep, actual: u64) -> Option<(usize, CountMismatch)> {
        let expected = step.expected_row_count?;
        if actual > expected as u64 {
            return Some((expected, CountMismatch::TooMany));
        }
        if !self.real_counts || actual >= expected as u64 {
            return None;
        }
        if step.matches_rows_without_key {
            return Some((expected, CountMismatch::KeylessTooFew));
        }

        // MySQL reports changed rows, not matched rows, so zero is valid only
        // for a no-op UPDATE. DELETE counts remain reliable and INSERT cannot
        // legitimately affect fewer rows without an error.
        let mysql_noop_update = self.driver.label() == "MySQL"
            && step.kind == RowWriteKind::Update
            && actual == 0;
        (!mysql_noop_update).then_some((expected, CountMismatch::KeyedTooFew))
    }

    async fn handle_failure(&self, session: &mut dyn ParameterizedSession, wrapped: bool) {
        if wrapped {
            let _ = session.execute("ROLLBACK").await;
        }
    }

    async fn run_epilogue(&self, plan: &DataWritePlan) {
        for statement in &plan.epilogue {
            let _ = self
                .driver
                .execute_parameterized(&statement.sql, &statement.parameters)
                .await;
        }
    }
}

/// Conveniences so any `Error` converts into a write error uniformly.
impl From<Error> for DataWriteError {
    fn from(e: Error) -> Self {
        DataWriteError::Query(e.to_string())
    }
}
