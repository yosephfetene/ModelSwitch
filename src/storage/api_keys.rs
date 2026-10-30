//! The `client_api_keys` table. Stores hashes and display prefixes only;
//! no function here accepts or returns a plaintext key.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::api_key::{KeyHash, KeyName, KeyStatus, NewApiKey};
use crate::storage::BoxFuture;

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

/// What authentication needs to know about a presented key's hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyLookup {
    pub id: Uuid,
    pub revoked: bool,
}

/// Why a key lookup failed. For logs only, never for responses.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct KeyStoreError(pub String);

/// The test seam between authentication and the database, so tests can
/// simulate a database that is down or hanging.
pub trait KeyStore: Send + Sync {
    fn find_by_hash(
        &self,
        hash: KeyHash,
    ) -> BoxFuture<'_, Result<Option<KeyLookup>, KeyStoreError>>;
}

/// `KeyStore` backed by PostgreSQL. One indexed lookup per request (the
/// `key_hash` UNIQUE constraint), so revocation applies immediately.
pub struct PgKeyStore {
    pool: PgPool,
}

impl PgKeyStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl KeyStore for PgKeyStore {
    fn find_by_hash(
        &self,
        hash: KeyHash,
    ) -> BoxFuture<'_, Result<Option<KeyLookup>, KeyStoreError>> {
        Box::pin(async move {
            sqlx::query_as::<_, (Uuid, bool)>(
                "SELECT id, revoked_at IS NOT NULL FROM client_api_keys WHERE key_hash = $1",
            )
            .bind(hash.as_bytes().as_slice())
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.map(|(id, revoked)| KeyLookup { id, revoked }))
            .map_err(|error| KeyStoreError(error.to_string()))
        })
    }
}
