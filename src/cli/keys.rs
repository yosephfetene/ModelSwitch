//! `modelswitch keys create|list|revoke` (ADR-012).
//!
//! Output goes to the given writer (stdout in production, a buffer in
//! tests). The plaintext key is written exactly once, by `create`, and
//! never logged; audit log lines carry only the ID and display prefix.

use std::io::Write;

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use secrecy::ExposeSecret;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::api_key::{ExpiryDays, KeyEnvironment, KeyName, NewApiKey};
use crate::storage::api_keys::{self, RevokeOutcome};

const TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// How `keys create` makes a key. By default: a dev key that never expires.
#[derive(Debug, Clone, Copy, Default)]
pub struct CreateOptions {
    pub environment: KeyEnvironment,
    pub expires_in_days: Option<u32>,
}

pub async fn create(
    pool: &PgPool,
    name: &str,
    options: CreateOptions,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    let name = KeyName::parse(name)?;
    let expires_in = options.expires_in_days.map(ExpiryDays::parse).transpose()?;
    let key =
        NewApiKey::generate(options.environment).context("secure random source unavailable")?;
    let record = api_keys::insert(pool, &name, &key, expires_in)
        .await
        .context("failed to store the key")?;
    tracing::info!(key_id = %record.id, key_prefix = %record.key_prefix, "api key created");

    writeln!(
        out,
        "Created key {:?} (id {}, prefix {}).",
        record.name, record.id, record.key_prefix
    )?;
    if let Some(expires_at) = record.expires_at {
        writeln!(out, "It expires {} UTC.", expires_at.format(TIME_FORMAT))?;
    }
    writeln!(out, "Copy it now; it will not be shown again:")?;
    writeln!(out)?;
    writeln!(out, "{}", key.plaintext().expose_secret())?;
    Ok(())
}

/// Lists every key, with its status as of `now`.
pub async fn list(pool: &PgPool, now: DateTime<Utc>, out: &mut impl Write) -> anyhow::Result<()> {
    let records = api_keys::list(pool).await.context("failed to list keys")?;
    if records.is_empty() {
        writeln!(
            out,
            "No keys yet. Create one with: modelswitch keys create --name <NAME>"
        )?;
        return Ok(());
    }

    writeln!(
        out,
        "{:<36}  {:<12}  {:<8}  {:<20}  {:<20}  NAME",
        "ID", "PREFIX", "STATUS", "CREATED (UTC)", "EXPIRES (UTC)"
    )?;
    for record in records {
        writeln!(
            out,
            "{:<36}  {:<12}  {:<8}  {:<20}  {:<20}  {}",
            record.id,
            record.key_prefix,
            record.status(now),
            record.created_at.format(TIME_FORMAT).to_string(),
            record.expires_at.map_or("never".to_owned(), |expires_at| {
                expires_at.format(TIME_FORMAT).to_string()
            }),
            record.name
        )?;
    }
    Ok(())
}

pub async fn revoke(pool: &PgPool, id: Uuid, out: &mut impl Write) -> anyhow::Result<()> {
    match api_keys::revoke(pool, id)
        .await
        .context("failed to revoke the key")?
    {
        RevokeOutcome::Revoked(record) => {
            tracing::info!(key_id = %record.id, key_prefix = %record.key_prefix, "api key revoked");
            writeln!(out, "Revoked key {} ({}).", record.id, record.key_prefix)?;
        }
        RevokeOutcome::AlreadyRevoked(record) => {
            writeln!(
                out,
                "Key {} ({}) was already revoked.",
                record.id, record.key_prefix
            )?;
        }
        RevokeOutcome::NotFound => bail!("no key with id {id}"),
    }
    Ok(())
}
