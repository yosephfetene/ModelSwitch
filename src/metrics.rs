//! Running totals for monitoring, in the Prometheus text format
//! (ADR-030).
//!
//! Labels are provider names and outcomes only: both come from a small
//! fixed set, never from anything a client sends, so a client can neither
//! grow the series without limit nor put text into this page. Provider
//! names are validated as `a-z 0-9 - _`, so they need no escaping.
//!
//! Counters live in memory and restart from zero with the process, which
//! monitoring tools expect. Three counters do not justify a dependency.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::accounting::RequestRecord;

#[derive(Debug, Default)]
struct Counters {
    /// By final outcome.
    requests: BTreeMap<&'static str, u64>,
    /// By provider and outcome.
    attempts: BTreeMap<(String, &'static str), u64>,
    accounting_failures: u64,
    rate_limited: u64,
}

/// Totals since the process started. Shared by the request path, which
/// counts, and the metrics listener, which reads.
#[derive(Debug, Default)]
pub struct Metrics {
    counters: Mutex<Counters>,
}

impl Metrics {
    /// A panic elsewhere must not stop the counting, and a counter cannot
    /// be left half-updated, so a poisoned lock is still usable.
    fn counters(&self) -> MutexGuard<'_, Counters> {
        self.counters.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts a request that reached a provider, and each of its attempts.
    pub fn record_request(&self, record: &RequestRecord) {
        let mut counters = self.counters();
        *counters
            .requests
            .entry(record.outcome.as_str())
            .or_default() += 1;
        for attempt in &record.attempts {
            let key = (attempt.provider.clone(), attempt.outcome.as_str());
            *counters.attempts.entry(key).or_default() += 1;
        }
    }

    /// Counts a request record that could not be written (ADR-009). There
    /// is no row to look at, so this counter is how operators find out.
    pub fn record_accounting_failure(&self) {
        self.counters().accounting_failures += 1;
    }

    /// Counts a request refused by its key's rate limit (ADR-031).
    pub fn record_rate_limited(&self) {
        self.counters().rate_limited += 1;
    }

    /// The Prometheus text exposition format.
    pub fn render(&self) -> String {
        let counters = self.counters();
        let mut page = String::new();

        header(
            &mut page,
            "requests_total",
            "Requests that reached a provider, by final outcome.",
        );
        for (outcome, count) in &counters.requests {
            let _ = writeln!(page, "requests_total{{outcome=\"{outcome}\"}} {count}");
        }

        header(
            &mut page,
            "provider_attempts_total",
            "Provider calls, by provider and outcome.",
        );
        for ((provider, outcome), count) in &counters.attempts {
            let _ = writeln!(
                page,
                "provider_attempts_total{{provider=\"{provider}\",outcome=\"{outcome}\"}} {count}"
            );
        }

        header(
            &mut page,
            "accounting_failures_total",
            "Request records that could not be written.",
        );
        let _ = writeln!(
            page,
            "accounting_failures_total {}",
            counters.accounting_failures
        );

        header(
            &mut page,
            "rate_limited_total",
            "Requests refused by a rate limit.",
        );
        let _ = writeln!(page, "rate_limited_total {}", counters.rate_limited);
        page
    }
}

fn header(page: &mut String, name: &str, help: &str) {
    let _ = writeln!(page, "# HELP {name} {help}");
    let _ = writeln!(page, "# TYPE {name} counter");
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::accounting::{AttemptRecord, Failure, Outcome, RetryDecision};
    use crate::domain::Usage;

    fn attempt(provider: &str, outcome: Outcome) -> AttemptRecord {
        AttemptRecord {
            provider: provider.to_owned(),
            model: "m".to_owned(),
            outcome,
            failure: Failure::default(),
            class: None,
            decision: RetryDecision::NotRetried,
            latency_ms: 1,
        }
    }

    fn record(outcome: Outcome, attempts: Vec<AttemptRecord>) -> RequestRecord {
        RequestRecord {
            request_id: "req_x".to_owned(),
            client_request_id: None,
            client_api_key_id: Uuid::nil(),
            requested_model: "fast".to_owned(),
            provider: "p".to_owned(),
            model: "m".to_owned(),
            outcome,
            failure: Failure::default(),
            latency_ms: 1,
            usage: Usage::unavailable(),
            attempts,
        }
    }

    #[test]
    fn a_new_page_still_reports_accounting_failures() {
        let page = Metrics::default().render();

        assert!(page.contains("accounting_failures_total 0\n"), "{page}");
        assert!(page.contains("# TYPE requests_total counter\n"), "{page}");
    }

    #[test]
    fn counts_requests_and_each_attempt() {
        let metrics = Metrics::default();
        let attempts = vec![
            attempt("groq", Outcome::Failed),
            attempt("groq", Outcome::Failed),
            attempt("gemini", Outcome::Succeeded),
        ];

        metrics.record_request(&record(Outcome::Succeeded, attempts));
        metrics.record_request(&record(
            Outcome::TimedOut,
            vec![attempt("gemini", Outcome::TimedOut)],
        ));
        metrics.record_accounting_failure();

        let page = metrics.render();
        for line in [
            "requests_total{outcome=\"succeeded\"} 1",
            "requests_total{outcome=\"timed_out\"} 1",
            "provider_attempts_total{provider=\"groq\",outcome=\"failed\"} 2",
            "provider_attempts_total{provider=\"gemini\",outcome=\"succeeded\"} 1",
            "provider_attempts_total{provider=\"gemini\",outcome=\"timed_out\"} 1",
            "accounting_failures_total 1",
        ] {
            assert!(page.lines().any(|l| l == line), "missing {line}:\n{page}");
        }
    }
}
