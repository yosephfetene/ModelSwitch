//! Which candidates a benchmark run makes eligible, and in what order
//! (ADR-038).
//!
//! A candidate is eligible when its run has enough samples, its success
//! rate meets the bar, and, if a cost ceiling is set, its mean cost per
//! answered call is known and within the ceiling. Eligible candidates are
//! ranked fastest first by median latency; the rest follow, in their
//! configured order, with the reason they were left out.

use crate::domain::Target;

/// One candidate's results from one run. Latencies and costs cover
/// answered calls only; failures and timeouts count in the success rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateSummary {
    pub target: Target,
    pub samples: u32,
    /// Answered and passed the check.
    pub passed: u32,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    /// Mean estimated cost in USD, as exact decimal text. `None` when no
    /// answered call could be priced.
    pub mean_cost_usd: Option<String>,
    /// Answered calls with no estimated cost: no price, or usage the
    /// provider did not report.
    pub unpriced: u32,
    /// Whether the mean cost is at most the ceiling. `None` when no
    /// ceiling was given.
    pub within_ceiling: Option<bool>,
}

/// The bar a candidate must clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    /// Least success rate, in percent.
    pub min_success_percent: u8,
    /// Fewest samples a verdict may rest on.
    pub min_samples: u32,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            min_success_percent: 90,
            min_samples: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Eligible,
    TooFewSamples,
    BelowSuccessBar,
    /// A ceiling is set but some answered calls have no cost, so the
    /// mean is unknown. Treated like an unpriced model under a budget
    /// (ADR-035): not assumed to be cheap.
    CostUnknown,
    OverCostCeiling,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::TooFewSamples => "too few samples",
            Self::BelowSuccessBar => "below success bar",
            Self::CostUnknown => "cost unknown",
            Self::OverCostCeiling => "over cost ceiling",
        }
    }
}

pub fn judge(summary: &CandidateSummary, bar: &Bar) -> Verdict {
    if summary.samples < bar.min_samples.max(1) {
        return Verdict::TooFewSamples;
    }
    if u64::from(summary.passed) * 100
        < u64::from(bar.min_success_percent) * u64::from(summary.samples)
    {
        return Verdict::BelowSuccessBar;
    }
    match summary.within_ceiling {
        None => Verdict::Eligible,
        Some(_) if summary.unpriced > 0 || summary.mean_cost_usd.is_none() => Verdict::CostUnknown,
        Some(true) => Verdict::Eligible,
        Some(false) => Verdict::OverCostCeiling,
    }
}

/// The candidates with their verdicts: eligible ones fastest first (by
/// median, then 95th percentile), then the others in the given order.
pub fn rank(summaries: Vec<CandidateSummary>, bar: &Bar) -> Vec<(CandidateSummary, Verdict)> {
    let (mut eligible, others): (Vec<_>, Vec<_>) = summaries
        .into_iter()
        .map(|summary| {
            let verdict = judge(&summary, bar);
            (summary, verdict)
        })
        .partition(|(_, verdict)| *verdict == Verdict::Eligible);
    // Stable, so ties keep the given order.
    eligible.sort_by_key(|(summary, _)| {
        (
            summary.p50_ms.unwrap_or(u64::MAX),
            summary.p95_ms.unwrap_or(u64::MAX),
        )
    });
    eligible.extend(others);
    eligible
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(name: &str, samples: u32, passed: u32, p50: u64) -> CandidateSummary {
        CandidateSummary {
            target: Target::parse(name).unwrap(),
            samples,
            passed,
            p50_ms: Some(p50),
            p95_ms: Some(p50 * 2),
            mean_cost_usd: Some("0.00001".to_owned()),
            unpriced: 0,
            within_ceiling: None,
        }
    }

    #[test]
    fn the_success_bar_is_inclusive() {
        let bar = Bar::default();

        assert_eq!(
            judge(&summary("p/a", 120, 108, 100), &bar),
            Verdict::Eligible
        );
        assert_eq!(
            judge(&summary("p/a", 120, 107, 100), &bar),
            Verdict::BelowSuccessBar
        );
        assert_eq!(judge(&summary("p/a", 30, 30, 100), &bar), Verdict::Eligible);
    }

    #[test]
    fn too_few_samples_decide_nothing() {
        let bar = Bar::default();

        assert_eq!(
            judge(&summary("p/a", 29, 29, 100), &bar),
            Verdict::TooFewSamples
        );
        let none = Bar {
            min_samples: 0,
            ..Bar::default()
        };
        assert_eq!(
            judge(&summary("p/a", 0, 0, 100), &none),
            Verdict::TooFewSamples
        );
    }

    #[test]
    fn a_ceiling_needs_a_known_cost_within_it() {
        let bar = Bar::default();
        let within = |within_ceiling, unpriced, mean: Option<&str>| CandidateSummary {
            within_ceiling,
            unpriced,
            mean_cost_usd: mean.map(str::to_owned),
            ..summary("p/a", 120, 120, 100)
        };

        assert_eq!(judge(&within(None, 5, None), &bar), Verdict::Eligible);
        assert_eq!(
            judge(&within(Some(true), 0, Some("0.1")), &bar),
            Verdict::Eligible
        );
        assert_eq!(
            judge(&within(Some(false), 0, Some("0.9")), &bar),
            Verdict::OverCostCeiling
        );
        assert_eq!(
            judge(&within(Some(true), 1, Some("0.1")), &bar),
            Verdict::CostUnknown
        );
        assert_eq!(
            judge(&within(Some(true), 0, None), &bar),
            Verdict::CostUnknown
        );
    }

    #[test]
    fn eligible_candidates_come_fastest_first_then_the_rest_in_order() {
        let ranked = rank(
            vec![
                summary("slow/a", 120, 120, 900),
                summary("failing/b", 120, 50, 100),
                summary("fast/c", 120, 115, 300),
                summary("few/d", 10, 10, 50),
                summary("tied/e", 120, 120, 300),
            ],
            &Bar::default(),
        );

        let order: Vec<_> = ranked
            .iter()
            .map(|(summary, verdict)| (summary.target.to_string(), *verdict))
            .collect();
        assert_eq!(
            order,
            [
                ("fast/c".to_owned(), Verdict::Eligible),
                ("tied/e".to_owned(), Verdict::Eligible),
                ("slow/a".to_owned(), Verdict::Eligible),
                ("failing/b".to_owned(), Verdict::BelowSuccessBar),
                ("few/d".to_owned(), Verdict::TooFewSamples),
            ]
        );
    }

    #[test]
    fn the_95th_percentile_breaks_a_median_tie() {
        let mut steady = summary("steady/a", 120, 120, 300);
        steady.p95_ms = Some(400);
        let mut spiky = summary("spiky/b", 120, 120, 300);
        spiky.p95_ms = Some(2000);

        let ranked = rank(vec![spiky, steady], &Bar::default());

        assert_eq!(ranked[0].0.target.provider, "steady");
    }
}
