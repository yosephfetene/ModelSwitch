//! Budget checks against `client_api_keys`, `request_records`, and
//! `pricing_versions` (ADR-035).

use sqlx::PgPool;
use uuid::Uuid;

use crate::budget::{BudgetDecision, BudgetStore, BudgetStoreError};
use crate::domain::Target;
use crate::storage::BoxFuture;

/// `BudgetStore` backed by PostgreSQL. One query per request, on the
/// existing key-and-date index; keys without a budget skip the sums.
pub struct PgBudgetStore {
    pool: PgPool,
}

impl PgBudgetStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn decide(&self, key: Uuid, targets: &[Target]) -> Result<BudgetDecision, sqlx::Error> {
        let providers: Vec<&str> = targets.iter().map(|t| t.provider.as_str()).collect();
        let models: Vec<&str> = targets.iter().map(|t| t.model.as_str()).collect();
        // Months are UTC whatever the session's time zone: both the
        // truncation and the month added take it explicitly.
        let (exceeded, unpriced, retry_after_secs): (bool, bool, i64) = sqlx::query_as(
            "SELECT \
                 COALESCE(\
                     (SELECT COALESCE(sum(r.estimated_cost), 0) \
                      FROM request_records r \
                      WHERE k.monthly_budget_usd IS NOT NULL \
                        AND r.client_api_key_id = k.id \
                        AND r.created_at >= date_trunc('month', now(), 'UTC')) \
                     >= k.monthly_budget_usd, \
                     false), \
                 k.monthly_budget_usd IS NOT NULL AND EXISTS (\
                     SELECT 1 FROM unnest($2::text[], $3::text[]) AS t (provider, model) \
                     WHERE NOT EXISTS (\
                         SELECT 1 FROM pricing_versions p \
                         WHERE p.provider = t.provider AND p.model = t.model \
                           AND p.effective_to IS NULL)), \
                 ceil(extract(epoch FROM \
                     date_add(date_trunc('month', now(), 'UTC'), interval '1 month', 'UTC') \
                     - now()))::bigint \
             FROM client_api_keys k WHERE k.id = $1",
        )
        .bind(key)
        .bind(providers)
        .bind(models)
        .fetch_one(&self.pool)
        .await?;

        Ok(if exceeded {
            BudgetDecision::Exceeded {
                retry_after_secs: retry_after_secs.max(1) as u64,
            }
        } else if unpriced {
            BudgetDecision::Unpriced
        } else {
            BudgetDecision::Allowed
        })
    }
}

impl BudgetStore for PgBudgetStore {
    fn check<'a>(
        &'a self,
        key: Uuid,
        targets: &'a [Target],
    ) -> BoxFuture<'a, Result<BudgetDecision, BudgetStoreError>> {
        Box::pin(async move {
            self.decide(key, targets)
                .await
                .map_err(|error| BudgetStoreError(error.to_string()))
        })
    }
}
