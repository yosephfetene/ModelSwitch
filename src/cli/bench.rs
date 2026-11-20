//! `modelswitch bench run|report` (ADR-038).
//!
//! `run` asks every candidate of an alias the whole workload, stores the
//! graded samples, and prints the report. `report` shows an alias's
//! latest run: per candidate, the success rate, latency, mean cost, and
//! whether it is eligible. Routing does not use the results yet.

use std::io::Write;

use anyhow::Context;
use sqlx::PgPool;

use super::utc_to_the_second;
use crate::bench::{self, Bar, CandidateSummary, RunOptions, Verdict, Workload};
use crate::domain::Target;
use crate::pricing::UsdAmount;
use crate::providers::Registry;
use crate::storage::bench as store;

/// Runs the workload against `candidates`, stores the samples, and
/// prints the report with the default bar.
pub async fn run(
    pool: &PgPool,
    registry: &Registry,
    alias: &str,
    candidates: &[Target],
    workload: &Workload,
    options: RunOptions,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    let calls = candidates.len() * workload.items.len() * options.repeats as usize;
    writeln!(
        out,
        "Benchmarking alias {alias:?}: {} candidates x {} items x {} repeats = {calls} calls.",
        candidates.len(),
        workload.items.len(),
        options.repeats
    )?;
    let samples = bench::run(registry, candidates, workload, &options).await?;
    let stored = store::save(pool, alias, workload, options.repeats, &samples)
        .await
        .context("failed to store the benchmark run")?;
    writeln!(out, "Stored run {}.", stored.id)?;
    writeln!(out)?;
    report(pool, alias, candidates, &Bar::default(), None, out).await
}

/// Prints the alias's latest run against `bar` and an optional mean cost
/// ceiling in USD per answered call.
pub async fn report(
    pool: &PgPool,
    alias: &str,
    candidates: &[Target],
    bar: &Bar,
    ceiling: Option<&UsdAmount>,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    let Some((run, mut summaries)) = store::latest(pool, alias, ceiling)
        .await
        .context("failed to read benchmark results")?
    else {
        writeln!(
            out,
            "No benchmark runs for alias {alias:?} yet. Start one with: \
             modelswitch bench run --alias {alias}"
        )?;
        return Ok(());
    };
    // Configured order first, so ties and the ineligible keep it;
    // candidates no longer in the alias come last.
    summaries.sort_by_key(|summary| {
        candidates
            .iter()
            .position(|target| *target == summary.target)
            .unwrap_or(usize::MAX)
    });

    writeln!(
        out,
        "Latest run of alias {alias:?}: {} at {} UTC, {} (sha256 {}), {} repeats.",
        run.id,
        utc_to_the_second(run.created_at)?,
        run.workload_name,
        &run.workload_sha256[..12],
        run.repeats
    )?;
    writeln!(
        out,
        "Bar: at least {}% success over at least {} samples; mean cost ceiling: {}.",
        bar.min_success_percent,
        bar.min_samples,
        ceiling.map_or("none".to_owned(), |c| format!("{} USD", c.as_str()))
    )?;
    writeln!(out)?;
    writeln!(
        out,
        "{:<4}  {:<40}  {:<18}  {:>7}  {:>7}  {:<16}  VERDICT",
        "RANK", "CANDIDATE", "SUCCESS", "P50 MS", "P95 MS", "MEAN COST (USD)"
    )?;
    let mut rank = 0;
    for (summary, verdict) in bench::rank(summaries, bar) {
        let position = if verdict == Verdict::Eligible {
            rank += 1;
            rank.to_string()
        } else {
            "-".to_owned()
        };
        writeln!(
            out,
            "{:<4}  {:<40}  {:<18}  {:>7}  {:>7}  {:<16}  {}",
            position,
            summary.target.to_string(),
            success(&summary),
            millis(summary.p50_ms),
            millis(summary.p95_ms),
            cost(&summary),
            verdict.as_str()
        )?;
    }
    Ok(())
}

/// Such as `117/120 (97.5%)`, rounded down so 89.99% never shows as 90%.
fn success(summary: &CandidateSummary) -> String {
    let permille = if summary.samples == 0 {
        0
    } else {
        u64::from(summary.passed) * 1000 / u64::from(summary.samples)
    };
    format!(
        "{}/{} ({}.{}%)",
        summary.passed,
        summary.samples,
        permille / 10,
        permille % 10
    )
}

fn millis(value: Option<u64>) -> String {
    value.map_or("-".to_owned(), |ms| ms.to_string())
}

fn cost(summary: &CandidateSummary) -> String {
    match (&summary.mean_cost_usd, summary.unpriced) {
        (None, _) => "unknown".to_owned(),
        (Some(mean), 0) => mean.clone(),
        (Some(mean), _) => format!("{mean} (partial)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(samples: u32, passed: u32) -> CandidateSummary {
        CandidateSummary {
            target: Target::parse("p/m").unwrap(),
            samples,
            passed,
            p50_ms: None,
            p95_ms: None,
            mean_cost_usd: None,
            unpriced: 0,
            within_ceiling: None,
        }
    }

    #[test]
    fn success_rounds_down_to_a_tenth_of_a_percent() {
        assert_eq!(success(&summary(120, 117)), "117/120 (97.5%)");
        assert_eq!(success(&summary(10000, 8999)), "8999/10000 (89.9%)");
        assert_eq!(success(&summary(3, 3)), "3/3 (100.0%)");
        assert_eq!(success(&summary(0, 0)), "0/0 (0.0%)");
    }

    #[test]
    fn a_partly_priced_mean_says_so() {
        let mut partly = summary(10, 10);
        partly.mean_cost_usd = Some("0.00001".to_owned());
        partly.unpriced = 2;

        assert_eq!(cost(&partly), "0.00001 (partial)");
        partly.unpriced = 0;
        assert_eq!(cost(&partly), "0.00001");
        assert_eq!(cost(&summary(10, 10)), "unknown");
    }
}
