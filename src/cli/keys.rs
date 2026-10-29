//! `modelswitch keys create|list|revoke` (ADR-012).
//!
//! Output goes to the given writer (stdout in production, a buffer in
//! tests). The plaintext key is written exactly once, by `create`, and
//! never logged; audit log lines carry only the ID and display prefix.

use std::io::Write;

use anyhow::{Context, bail};
use secrecy::ExposeSecret;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::api_key::{KeyName, NewApiKey};
use crate::storage::api_keys::{self, RevokeOutcome};

pub async fn create(pool: &PgPool, name: &str, out: &mut impl Write) -> anyhow::Result<()> {
    let name = KeyName::parse(name)?;
    let key = NewApiKey::generate().context("secure random source unavailable")?;
    let record = api_keys::insert(pool, &name, &key)
        .await
        .context("failed to store the key")?;
    tracing::info!(key_id = %record.id, key_prefix = %record.key_prefix, "api key created");

    writeln!(
        out,
        "Created key {:?} (id {}, prefix {}).",
        record.name, record.id, record.key_prefix
    )?;
    writeln!(out, "Copy it now; it will not be shown again:")?;
    writeln!(out)?;
    writeln!(out, "{}", key.plaintext().expose_secret())?;
    Ok(())
}

pub async fn list(pool: &PgPool, out: &mut impl Write) -> anyhow::Result<()> {
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
        "{:<36}  {:<12}  {:<8}  {:<20}  NAME",
        "ID", "PREFIX", "STATUS", "CREATED (UTC)"
    )?;
    for record in records {
        writeln!(
            out,
            "{:<36}  {:<12}  {:<8}  {:<20}  {}",
            record.id,
            record.key_prefix,
            record.status(),
            record.created_at.format("%Y-%m-%d %H:%M:%S"),
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
