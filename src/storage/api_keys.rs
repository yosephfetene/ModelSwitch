//! The `client_api_keys` table. Stores hashes and display prefixes only;
//! no function here accepts or returns a plaintext key.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::api_key::{KeyName, KeyStatus, NewApiKey};

/// A stored key as shown to operators. Deliberately has no hash field:
/// listings have no reason to read it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApiKeyRecord {
    pub id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ApiKeyRecord {
    pub fn status(&self) -> KeyStatus {
        match self.revoked_at {
            Some(_) => KeyStatus::Revoked,
            None => KeyStatus::Active,
        }
    }
}

pub async fn insert(
    pool: &PgPool,
    name: &KeyName,
    key: &NewApiKey,
) -> Result<ApiKeyRecord, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO client_api_keys (name, key_prefix, key_hash) VALUES ($1, $2, $3) \
         RETURNING id, name, key_prefix, created_at, revoked_at",
    )
    .bind(name.as_str())
    .bind(key.prefix())
    .bind(key.hash().as_bytes().as_slice())
    .fetch_one(pool)
    .await
}

/// All keys, oldest first (UUIDv7 IDs sort by creation time).
pub async fn list(pool: &PgPool) -> Result<Vec<ApiKeyRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, name, key_prefix, created_at, revoked_at FROM client_api_keys ORDER BY id",
    )
    .fetch_all(pool)
    .await
}

#[derive(Debug)]
pub enum RevokeOutcome {
    Revoked(ApiKeyRecord),
    /// Revoking twice is harmless; the original `revoked_at` is kept.
    AlreadyRevoked(ApiKeyRecord),
    NotFound,
}

pub async fn revoke(pool: &PgPool, id: Uuid) -> Result<RevokeOutcome, sqlx::Error> {
    let revoked: Option<ApiKeyRecord> = sqlx::query_as(
        "UPDATE client_api_keys SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL \
         RETURNING id, name, key_prefix, created_at, revoked_at",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    if let Some(record) = revoked {
        return Ok(RevokeOutcome::Revoked(record));
    }

    let existing: Option<ApiKeyRecord> = sqlx::query_as(
        "SELECT id, name, key_prefix, created_at, revoked_at FROM client_api_keys WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(match existing {
        Some(record) => RevokeOutcome::AlreadyRevoked(record),
        None => RevokeOutcome::NotFound,
    })
}
