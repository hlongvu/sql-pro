//! Data write execution: run a save plan against a driver with the same
//! verification semantics as TablePro's `DataWriteExecutor`.
//!
//! Key rules ported from `KeyedWriteVerification` / `DataWriteRowCounts`:
//! - Keyed writes: `rows_affected > expected` is a fault; `< expected` is
//!   NOT (MySQL reports 0 affected for an UPDATE that writes a value the
//!   row already holds).
//! - Keyless writes: two-sided (`< expected` is also a fault) — but only on
//!   engines that report real affected counts.
//! - Prologue/epilogue run OUTSIDE the transaction (SQLite `PRAGMA
//!   foreign_keys` is a no-op inside one); a required prologue failure
//!   stops the run before any step.

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

/// Trait for executing a statement with bound parameters, used by the
/// write path. Implemented per engine.
#[async_trait]
pub trait ParameterizedExecutor: DatabaseDriver {
    async fn execute_parameterized(
        &self,
        sql: &str,
        parameters: &[Option<BindValue>],
    ) -> Result<u64>;
}

pub struct DataWriteExecutor<'a> {
    pub driver: &'a dyn ParameterizedExecutor,
    /// Whether the engine reports real affected counts.
    pub real_counts: bool,
}

impl<'a> DataWriteExecutor<'a> {
    pub async fn run(&self, plan: &DataWritePlan) -> std::result::Result<DataWriteResult, DataWriteError> {
        let mut result = DataWriteResult::default();

        let active_steps: Vec<&DataWriteStep> = plan
            .steps
            .iter()
            .filter(|s| !s.statement.sql.trim().is_empty())
            .collect();
        if active_steps.is_empty() {
            return Ok(result);
        }

        // Prologue, outside the transaction. A required prologue failure
        // stops the run before any step; an optional one (e.g. FK toggles)
        // is a courtesy that can be skipped.
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

        // Open the transaction when we own one.
        let wrapped = plan.owner == TransactionOwner::App;
        if wrapped {
            if let Err(e) = self.driver.execute("BEGIN").await {
                return Err(DataWriteError::Query(e.to_string()));
            }
        }

        for step in active_steps {
            let actual = match self
                .driver
                .execute_parameterized(&step.statement.sql, &step.statement.parameters)
                .await
            {
                Ok(n) => n,
                Err(e) => {
                    self.handle_failure(plan, wrapped).await;
                    return Err(DataWriteError::Query(e.to_string()));
                }
            };

            result.executed.push(StepOutcome {
                kind: step.kind,
                sql: step.statement.sql.clone(),
                rows_affected: actual,
            });

            // Verification (KeyedWriteVerification semantics).
            if let Some(expected) = step.expected_row_count {
                let keyless = step.matches_rows_without_key && self.real_counts;
                if actual > expected as u64 {
                    self.handle_failure(plan, wrapped).await;
                    return Err(DataWriteError::TooManyRows {
                        sql: step.statement.sql.clone(),
                        expected,
                        actual,
                    });
                }
                if keyless && actual < expected as u64 {
                    self.handle_failure(plan, wrapped).await;
                    return Err(DataWriteError::KeylessCountMismatch {
                        sql: step.statement.sql.clone(),
                        expected,
                        actual,
                    });
                }
            }
        }

        if wrapped {
            if let Err(e) = self.driver.execute("COMMIT").await {
                // The commit failed: attempt rollback, and if that fails
                // too, what ran is partially committed.
                let rollback_ok = self.driver.execute("ROLLBACK").await.is_ok();
                if !rollback_ok {
                    result.partial_commit = true;
                }
                return Err(DataWriteError::Query(e.to_string()));
            }
        }

        // Epilogue, after commit/rollback either way.
        for statement in &plan.epilogue {
            let _ = self
                .driver
                .execute_parameterized(&statement.sql, &statement.parameters)
                .await;
        }

        Ok(result)
    }

    async fn handle_failure(&self, plan: &DataWritePlan, wrapped: bool) {
        match plan.owner {
            TransactionOwner::App if wrapped => {
                let _ = self.driver.execute("ROLLBACK").await;
            }
            _ => {
                // Without a transaction we own, nothing to roll back — the
                // caller sees the error and the already-run statements.
            }
        }
    }
}

/// Conveniences so any `Error` converts into a write error uniformly.
impl From<Error> for DataWriteError {
    fn from(e: Error) -> Self {
        DataWriteError::Query(e.to_string())
    }
}
