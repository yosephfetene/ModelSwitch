//! Adaptive ordering of an alias's candidates (Stage 5, ADR-039).
//!
//! An alias with an `adaptive` policy is ordered on each request from
//! data loaded in the background, never from the database on the request
//! path:
//!
//! 1. The alias's latest benchmark decides who is eligible (ADR-038).
//! 2. A candidate failing live (enough recent attempts, success rate
//!    under the bar) is not eligible either, whatever its benchmark said.
//! 3. Eligible candidates go first, fastest first: by live median latency
//!    when enough recent requests succeeded, otherwise by benchmark
//!    median. A slowed-down first choice therefore loses its place, and
//!    the next candidate starts collecting live data of its own.
//! 4. The rest follow in configured order, so failover still has every
//!    candidate.
//!
//! With no data, stale data, no benchmark, or no eligible candidate, the
//! configured order applies. Every decision carries a one-line reason
//! that is stored with the request.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::time::Instant;

use crate::bench::{Bar, CandidateSummary, Verdict, judge};
use crate::domain::{BoxFuture, Target};
use crate::pricing::UsdAmount;

/// How often routing data is reloaded.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
/// Routing data older than this is not used: the configured order is.
pub const STALE_AFTER: Duration = Duration::from_secs(120);
/// Longest stored reason, in characters.
pub const REASON_MAX_CHARS: usize = 256;

/// How an alias is ordered, from `[aliases.<name>.adaptive]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdaptivePolicy {
    /// The benchmark bar, also applied to live success rates.
    pub bar: Bar,
    /// Most mean cost per answered benchmark call, in USD.
    pub max_mean_cost: Option<UsdAmount>,
    /// How far back live attempts count.
    pub window: Duration,
    /// Fewest live attempts a live judgement may rest on.
    pub min_live_requests: u32,
}

/// One target's live attempts within the window, from any request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveStat {
    pub target: Target,
    pub attempts: u32,
    pub succeeded: u32,
    /// Median latency of the successful attempts.
    pub p50_ms: Option<u64>,
}

/// What an alias's ordering is decided from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingData {
    /// The latest benchmark run's summaries, with the policy's ceiling
    /// applied. `None` if the alias was never benchmarked.
    pub bench: Option<Vec<CandidateSummary>>,
    pub live: Vec<LiveStat>,
}

/// The order to try, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub targets: Vec<Target>,
    pub reason: String,
}

/// Orders `candidates` (in configured order) by `policy` and `data`.
pub fn decide(
    candidates: &[Target],
    policy: &AdaptivePolicy,
    data: Option<&RoutingData>,
) -> Decision {
    let configured = |why: &str| Decision {
        targets: candidates.to_vec(),
        reason: format!("configured order: {why}"),
    };
    let Some(data) = data else {
        return configured("no current routing data");
    };
    let Some(bench) = &data.bench else {
        return configured("alias not benchmarked");
    };

    let mut eligible = Vec::new();
    let mut others = Vec::new();
    for target in candidates {
        let live = data.live.iter().find(|stat| stat.target == *target);
        let verdict = bench
            .iter()
            .find(|summary| summary.target == *target)
            .map(|summary| (judge(summary, &policy.bar), summary));
        let failing_live = live.is_some_and(|live| {
            live.attempts >= policy.min_live_requests.max(1)
                && u64::from(live.succeeded) * 100
                    < u64::from(policy.bar.min_success_percent) * u64::from(live.attempts)
        });
        match verdict {
            Some((Verdict::Eligible, _)) if failing_live => {
                others.push((target, "failing live".to_owned()));
            }
            Some((Verdict::Eligible, summary)) => {
                let speed = match live {
                    Some(live) if live.succeeded >= policy.min_live_requests.max(1) => {
                        live.p50_ms.map(|ms| {
                            (
                                ms,
                                format!("live p50 {ms} ms over {} requests", live.succeeded),
                            )
                        })
                    }
                    _ => None,
                }
                .or_else(|| {
                    summary
                        .p50_ms
                        .map(|ms| (ms, format!("benchmark p50 {ms} ms")))
                });
                eligible.push((target, speed));
            }
            Some((verdict, _)) => others.push((target, verdict.as_str().to_owned())),
            None => others.push((target, "not in the latest benchmark".to_owned())),
        }
    }
    if eligible.is_empty() {
        return configured("no eligible candidate");
    }
    // Stable, so equal speeds keep the configured order.
    eligible.sort_by_key(|(_, speed)| speed.as_ref().map_or(u64::MAX, |(ms, _)| *ms));

    let (first, speed) = &eligible[0];
    let mut reason = format!(
        "{first} first: fastest of {} eligible by {}",
        eligible.len(),
        speed
            .as_ref()
            .map_or("no latency data", |(_, how)| how.as_str())
    );
    if !others.is_empty() {
        let skipped: Vec<_> = others
            .iter()
            .map(|(target, why)| format!("{target} ({why})"))
            .collect();
        reason.push_str("; not eligible: ");
        reason.push_str(&skipped.join(", "));
    }
    Decision {
        targets: eligible
            .into_iter()
            .map(|(target, _)| target.clone())
            .chain(others.into_iter().map(|(target, _)| target.clone()))
            .collect(),
        reason: truncate(reason),
    }
}

fn truncate(mut text: String) -> String {
    if let Some((index, _)) = text.char_indices().nth(REASON_MAX_CHARS) {
        text.truncate(index);
    }
    text
}

/// Why routing data could not be loaded. For logs only.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct RoutingSourceError(pub String);

/// The test seam between adaptive routing and the database.
pub trait RoutingSource: Send + Sync {
    fn load<'a>(
        &'a self,
        candidates: &'a [Target],
        alias: &'a str,
        policy: &'a AdaptivePolicy,
    ) -> BoxFuture<'a, Result<RoutingData, RoutingSourceError>>;
}

/// The adaptive aliases and their latest routing data.
#[derive(Default)]
pub struct AdaptiveRouter {
    aliases: BTreeMap<String, (Vec<Target>, AdaptivePolicy)>,
    data: RwLock<BTreeMap<String, (Instant, Arc<RoutingData>)>>,
}

impl AdaptiveRouter {
    /// `aliases` maps each adaptive alias to its candidates, in
    /// configured order, and its policy.
    pub fn new(aliases: impl IntoIterator<Item = (String, (Vec<Target>, AdaptivePolicy))>) -> Self {
        Self {
            aliases: aliases.into_iter().collect(),
            data: RwLock::default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.aliases.is_empty()
    }

    /// The decision for `model`, or `None` if it is not an adaptive alias.
    pub fn route(&self, model: &str) -> Option<Decision> {
        let (candidates, policy) = self.aliases.get(model)?;
        let data = self
            .data
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(model)
            .filter(|(loaded, _)| loaded.elapsed() <= STALE_AFTER)
            .map(|(_, data)| Arc::clone(data));
        Some(decide(candidates, policy, data.as_deref()))
    }

    /// Reloads every adaptive alias, each under `timeout`. A failed load
    /// keeps the previous data, which goes stale on its own.
    pub async fn refresh(&self, source: &dyn RoutingSource, timeout: Duration) {
        for (alias, (candidates, policy)) in &self.aliases {
            let load = source.load(candidates, alias, policy);
            match tokio::time::timeout(timeout, load).await {
                Ok(Ok(data)) => {
                    self.data
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(alias.clone(), (Instant::now(), Arc::new(data)));
                }
                Ok(Err(error)) => {
                    tracing::warn!(%alias, %error, "adaptive routing: could not load routing data");
                }
                Err(_) => {
                    tracing::warn!(%alias, "adaptive routing: loading routing data timed out");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(value: &str) -> Target {
        Target::parse(value).unwrap()
    }

    fn policy() -> AdaptivePolicy {
        AdaptivePolicy {
            bar: Bar::default(),
            max_mean_cost: None,
            window: Duration::from_secs(900),
            min_live_requests: 10,
        }
    }

    fn bench(name: &str, passed: u32, p50: u64) -> CandidateSummary {
        CandidateSummary {
            target: target(name),
            samples: 120,
            passed,
            p50_ms: Some(p50),
            p95_ms: Some(p50 * 2),
            mean_cost_usd: Some("0.00001".to_owned()),
            unpriced: 0,
            within_ceiling: None,
        }
    }

    fn live(name: &str, attempts: u32, succeeded: u32, p50: u64) -> LiveStat {
        LiveStat {
            target: target(name),
            attempts,
            succeeded,
            p50_ms: Some(p50),
        }
    }

    fn candidates() -> Vec<Target> {
        vec![target("a/1"), target("b/2"), target("c/3")]
    }

    fn order(decision: &Decision) -> Vec<String> {
        decision.targets.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn without_data_the_configured_order_applies() {
        let none = decide(&candidates(), &policy(), None);
        assert_eq!(order(&none), ["a/1", "b/2", "c/3"]);
        assert_eq!(none.reason, "configured order: no current routing data");

        let unbenchmarked = decide(&candidates(), &policy(), Some(&RoutingData::default()));
        assert_eq!(order(&unbenchmarked), ["a/1", "b/2", "c/3"]);
        assert_eq!(
            unbenchmarked.reason,
            "configured order: alias not benchmarked"
        );
    }

    #[test]
    fn eligible_candidates_go_first_by_benchmark_speed() {
        let data = RoutingData {
            bench: Some(vec![
                bench("a/1", 120, 800),
                bench("b/2", 50, 100),
                bench("c/3", 118, 300),
            ]),
            live: vec![],
        };

        let decision = decide(&candidates(), &policy(), Some(&data));

        assert_eq!(order(&decision), ["c/3", "a/1", "b/2"]);
        assert_eq!(
            decision.reason,
            "c/3 first: fastest of 2 eligible by benchmark p50 300 ms; \
             not eligible: b/2 (below success bar)"
        );
    }

    #[test]
    fn enough_live_data_overrides_the_benchmark_speed() {
        // a/1 was fastest when benchmarked but has slowed down live.
        let data = RoutingData {
            bench: Some(vec![
                bench("a/1", 120, 200),
                bench("b/2", 120, 400),
                bench("c/3", 120, 600),
            ]),
            live: vec![live("a/1", 40, 40, 2500), live("c/3", 9, 9, 50)],
        };

        let decision = decide(&candidates(), &policy(), Some(&data));

        // c/3's 9 live requests are too few, so its benchmark speed counts.
        assert_eq!(order(&decision), ["b/2", "c/3", "a/1"]);
        assert_eq!(
            decision.reason,
            "b/2 first: fastest of 3 eligible by benchmark p50 400 ms"
        );

        let fast_live = RoutingData {
            live: vec![live("c/3", 10, 10, 50)],
            ..data
        };
        let decision = decide(&candidates(), &policy(), Some(&fast_live));
        assert_eq!(order(&decision)[0], "c/3");
        assert_eq!(
            decision.reason,
            "c/3 first: fastest of 3 eligible by live p50 50 ms over 10 requests"
        );
    }

    #[test]
    fn a_candidate_failing_live_loses_eligibility() {
        let data = RoutingData {
            bench: Some(vec![bench("a/1", 120, 100), bench("b/2", 120, 300)]),
            live: vec![live("a/1", 20, 17, 90), live("b/2", 9, 0, 1)],
        };

        let decision = decide(&candidates()[..2], &policy(), Some(&data));

        // a/1: 85% live < 90%. b/2: 9 attempts are too few to judge.
        assert_eq!(order(&decision), ["b/2", "a/1"]);
        assert_eq!(
            decision.reason,
            "b/2 first: fastest of 1 eligible by benchmark p50 300 ms; \
             not eligible: a/1 (failing live)"
        );

        let barely = RoutingData {
            live: vec![live("a/1", 20, 18, 90)],
            ..data
        };
        assert_eq!(
            order(&decide(&candidates()[..2], &policy(), Some(&barely)))[0],
            "a/1"
        );
    }

    #[test]
    fn with_no_eligible_candidate_the_configured_order_applies() {
        let data = RoutingData {
            bench: Some(vec![bench("a/1", 10, 100), bench("c/3", 10, 100)]),
            live: vec![],
        };

        let decision = decide(&candidates(), &policy(), Some(&data));

        assert_eq!(order(&decision), ["a/1", "b/2", "c/3"]);
        assert_eq!(decision.reason, "configured order: no eligible candidate");
    }

    #[test]
    fn candidates_missing_from_the_benchmark_are_not_eligible() {
        let data = RoutingData {
            bench: Some(vec![bench("c/3", 120, 500)]),
            live: vec![],
        };

        let decision = decide(&candidates(), &policy(), Some(&data));

        assert_eq!(order(&decision), ["c/3", "a/1", "b/2"]);
        assert!(
            decision
                .reason
                .ends_with("a/1 (not in the latest benchmark), b/2 (not in the latest benchmark)"),
            "{}",
            decision.reason
        );
    }

    #[test]
    fn the_cost_ceiling_comes_through_the_benchmark() {
        let mut dear = bench("a/1", 120, 100);
        dear.within_ceiling = Some(false);
        let mut cheap = bench("b/2", 120, 900);
        cheap.within_ceiling = Some(true);
        let data = RoutingData {
            bench: Some(vec![dear, cheap]),
            live: vec![],
        };

        let decision = decide(&candidates()[..2], &policy(), Some(&data));

        assert_eq!(order(&decision), ["b/2", "a/1"]);
        assert!(decision.reason.ends_with("a/1 (over cost ceiling)"));
    }

    #[test]
    fn long_reasons_are_cut_to_the_limit() {
        let many: Vec<Target> = (0..40)
            .map(|n| target(&format!("provider-{n}/a-rather-long-model-name-{n}")))
            .collect();
        let data = RoutingData {
            bench: Some(
                many.iter()
                    .enumerate()
                    .map(|(n, t)| bench(&t.to_string(), if n == 0 { 120 } else { 0 }, 100))
                    .collect(),
            ),
            live: vec![],
        };

        let decision = decide(&many, &policy(), Some(&data));

        assert_eq!(decision.reason.chars().count(), REASON_MAX_CHARS);
        assert_eq!(decision.targets.len(), 40, "every candidate is kept");
    }

    struct Source(std::sync::Mutex<Vec<Result<RoutingData, RoutingSourceError>>>);

    impl RoutingSource for Source {
        fn load<'a>(
            &'a self,
            _: &'a [Target],
            _: &'a str,
            _: &'a AdaptivePolicy,
        ) -> BoxFuture<'a, Result<RoutingData, RoutingSourceError>> {
            let next = self.0.lock().unwrap().remove(0);
            Box::pin(async move { next })
        }
    }

    fn router() -> AdaptiveRouter {
        AdaptiveRouter::new([("smart".to_owned(), (candidates(), policy()))])
    }

    fn benchmarked() -> RoutingData {
        RoutingData {
            bench: Some(vec![bench("c/3", 120, 100)]),
            live: vec![],
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_router_uses_fresh_data_and_forgets_stale_data() {
        let router = router();
        let source = Source(std::sync::Mutex::new(vec![
            Ok(benchmarked()),
            Err(RoutingSourceError("database down".to_owned())),
        ]));

        assert_eq!(router.route("other"), None, "not an adaptive alias");
        assert_eq!(
            router.route("smart").unwrap().reason,
            "configured order: no current routing data"
        );

        router.refresh(&source, Duration::from_secs(1)).await;
        assert_eq!(order(&router.route("smart").unwrap())[0], "c/3");

        // A failed reload keeps the old data until it goes stale.
        tokio::time::advance(Duration::from_secs(60)).await;
        router.refresh(&source, Duration::from_secs(1)).await;
        assert_eq!(order(&router.route("smart").unwrap())[0], "c/3");

        tokio::time::advance(STALE_AFTER - Duration::from_secs(60)).await;
        assert_eq!(
            order(&router.route("smart").unwrap())[0],
            "c/3",
            "at the limit"
        );
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(
            router.route("smart").unwrap().reason,
            "configured order: no current routing data"
        );
    }

    struct Hanging;

    impl RoutingSource for Hanging {
        fn load<'a>(
            &'a self,
            _: &'a [Target],
            _: &'a str,
            _: &'a AdaptivePolicy,
        ) -> BoxFuture<'a, Result<RoutingData, RoutingSourceError>> {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_load_gives_up_at_the_deadline() {
        let router = router();
        let started = Instant::now();

        router.refresh(&Hanging, Duration::from_secs(3)).await;

        assert_eq!(started.elapsed(), Duration::from_secs(3));
        assert_eq!(
            router.route("smart").unwrap().reason,
            "configured order: no current routing data"
        );
    }
}
