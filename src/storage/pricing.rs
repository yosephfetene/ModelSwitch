//! The `pricing_versions` table (ADR-034). Rows are only ever closed,
//! never edited, so old estimates stay explainable.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::Target;
use crate::pricing::{PriceSource, UsdAmount};

/// A stored price as shown to operators. Prices are text so they print
/// exactly as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PriceRecord {
    pub id: Uuid,
    pub provider: String,
    pub model: String,
    pub input_usd_per_million: String,
    pub output_usd_per_million: String,
    pub effective_from: DateTime<Utc>,
    /// `None` while this is the current price.
    pub effective_to: Option<DateTime<Utc>>,
    pub source_reference: String,
}

/// Makes this the current price for `target`, closing any previous one at
/// the same moment, so exactly one price applies at any time.
pub async fn set(
    pool: &PgPool,
    target: &Target,
    input: &UsdAmount,
    output: &UsdAmount,
    source: &PriceSource,
) -> Result<PriceRecord, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "UPDATE pricing_versions SET effective_to = now() \
         WHERE provider = $1 AND model = $2 AND effective_to IS NULL",
    )
    .bind(&target.provider)
    .bind(&target.model)
    .execute(&mut *transaction)
    .await?;
    let record = sqlx::query_as(
        "INSERT INTO pricing_versions (\
             provider, model, input_usd_per_million, output_usd_per_million, source_reference\
         ) VALUES ($1, $2, $3::text::numeric, $4::text::numeric, $5) \
         RETURNING id, provider, model, input_usd_per_million::text AS input_usd_per_million, \
                   output_usd_per_million::text AS output_usd_per_million, effective_from, \
                   effective_to, source_reference",
    )
    .bind(&target.provider)
    .bind(&target.model)
    .bind(input.as_str())
    .bind(output.as_str())
    .bind(source.as_str())
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(record)
}

/// Every price, current and past, by model and then oldest first.
pub async fn list(pool: &PgPool) -> Result<Vec<PriceRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, provider, model, input_usd_per_million::text AS input_usd_per_million, \
                output_usd_per_million::text AS output_usd_per_million, effective_from, \
                effective_to, source_reference \
         FROM pricing_versions ORDER BY provider, model, effective_from",
    )
    .fetch_all(pool)
    .await
}
