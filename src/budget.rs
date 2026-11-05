//! Per-key monthly budgets (ADR-035).
//!
//! A rate limit is about speed; a budget is about money. The check runs
//! before each provider call, against the spending recorded so far, and
//! the cost of a request is only known once it finishes. So a budget is
//! firm, not exact: requests already running can take a key slightly over
//! it, bounded by the concurrency limit (ADR-032).
//!
//! A budget tracks known spending. Failed attempts and answers without
//! usage have no cost to count, so a real bill can run higher.

use uuid::Uuid;

use crate::domain::{BoxFuture, Target};

/// Whether a request may go ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDecision {
    Allowed,
    /// This month's spending has reached the budget.
    Exceeded {
        /// Until the next month starts, when spending counts from zero.
        retry_after_secs: u64,
    },
    /// The key has a budget, but a candidate has no current price, so its
    /// spending could not be counted.
    Unpriced,
}

/// Why a budget could not be checked. For logs only.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BudgetStoreError(pub String);

/// The test seam between the request path and the database, so tests can
/// simulate a database that is down or hanging. The caller bounds each
/// check with its own deadline.
pub trait BudgetStore: Send + Sync {
    /// Decides for `key`, whose request may use any of `targets`.
    fn check<'a>(
        &'a self,
        key: Uuid,
        targets: &'a [Target],
    ) -> BoxFuture<'a, Result<BudgetDecision, BudgetStoreError>>;
}
