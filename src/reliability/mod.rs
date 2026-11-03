//! Retry and fallback (Stage 2, ADR-028).
//!
//! Slice 2a: classifying a failed attempt. Slice 2b: the executor that
//! retries and falls back within the attempt and time limits.

mod classify;
mod executor;

pub use classify::{FailureClass, classify};
pub use executor::{Attempt, AttemptFailure, Execution, Limits, execute};
