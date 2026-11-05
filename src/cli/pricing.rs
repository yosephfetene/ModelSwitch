//! `modelswitch pricing set|list` (ADR-034).

use std::io::Write;

use anyhow::{Context, bail};
use sqlx::PgPool;

use crate::domain::Target;
use crate::pricing::{PriceSource, UsdAmount};
use crate::routing::Catalog;
use crate::storage::pricing;

const TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// What `pricing set` was asked to record, as typed.
#[derive(Debug, Clone, Copy)]
pub struct SetPrice<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub input_per_million: &'a str,
    pub output_per_million: &'a str,
    pub source: &'a str,
}

/// Records a new current price. The model must be configured: a price
/// under a mistyped name would never apply, and nothing would say so.
pub async fn set(
    pool: &PgPool,
    catalog: &Catalog,
    price: SetPrice<'_>,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    let input = UsdAmount::parse(price.input_per_million)?;
    let output = UsdAmount::parse(price.output_per_million)?;
    let source = PriceSource::parse(price.source)?;
    let target = Target {
        provider: price.provider.to_owned(),
        model: price.model.to_owned(),
    };
    if catalog.resolve(&target.to_string(), false).is_err() {
        bail!("{target} is not a configured model");
    }

    let record = pricing::set(pool, &target, &input, &output, &source)
        .await
        .context("failed to store the price")?;
    tracing::info!(pricing_version = %record.id, %target, "price set");
    writeln!(
        out,
        "{target} now costs ${} input and ${} output per million tokens (version {}).",
        record.input_usd_per_million, record.output_usd_per_million, record.id
    )?;
    Ok(())
}

pub async fn list(pool: &PgPool, out: &mut impl Write) -> anyhow::Result<()> {
    let records = pricing::list(pool).await.context("failed to list prices")?;
    if records.is_empty() {
        writeln!(
            out,
            "No prices yet. Set one with: modelswitch pricing set --help"
        )?;
        return Ok(());
    }

    writeln!(
        out,
        "{:<36}  {:<12}  {:<12}  {:<20}  {:<20}  {:<40}  SOURCE",
        "VERSION", "INPUT $/M", "OUTPUT $/M", "FROM (UTC)", "TO (UTC)", "MODEL"
    )?;
    for record in records {
        writeln!(
            out,
            "{:<36}  {:<12}  {:<12}  {:<20}  {:<20}  {:<40}  {}",
            record.id,
            record.input_usd_per_million,
            record.output_usd_per_million,
            record.effective_from.format(TIME_FORMAT).to_string(),
            record.effective_to.map_or("current".to_owned(), |to| {
                to.format(TIME_FORMAT).to_string()
            }),
            format!("{}/{}", record.provider, record.model),
            record.source_reference
        )?;
    }
    Ok(())
}
