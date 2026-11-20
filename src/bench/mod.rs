//! Benchmarks for benchmark-informed routing (Stage 5, ADR-038).
//!
//! A benchmark runs a fixed, versioned workload against every candidate
//! of an alias, one candidate at a time (pinned, no retries, no
//! fallback), and grades each answer against its answer key. The results
//! decide which candidates are eligible: enough samples, a success rate
//! at or above the bar, and, when a ceiling is set, a known mean cost
//! within it. Live routing does not use them yet.
//!
//! Like request records, results hold identifiers, verdicts, numbers, and
//! times, never a prompt or an output. Nothing here depends on HTTP or
//! the database.

pub mod check;
pub mod eligibility;
pub mod run;
pub mod workload;

pub use check::Expect;
pub use eligibility::{Bar, CandidateSummary, Verdict, judge, rank};
pub use run::{RunError, RunOptions, Sample, run};
pub use workload::{Item, Task, Workload, WorkloadError};
