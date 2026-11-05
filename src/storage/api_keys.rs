//! The `client_api_keys` table. Stores hashes and display prefixes only;
//! no function here accepts or returns a plaintext key.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::api_key::{ExpiryDays, KeyHash, KeyName, KeyStatus, NewApiKey};
use crate::pricing::UsdAmount;
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
    /// `None` means the key never expires.
    pub expires_at: Option<DateTime<Utc>>,
    /// USD per calendar month, exactly as stored. `None` means no budget.
    pub monthly_budget_usd: Option<String>,
}

impl ApiKeyRecord {
    /// The status as of `now`. Revocation wins, being a deliberate act.
    pub fn status(&self, now: DateTime<Utc>) -> KeyStatus {
        if self.revoked_at.is_some() {
            KeyStatus::Revoked
        } else if self.expires_at.is_some_and(|expires_at| expires_at <= now) {
            KeyStatus::Expired
        } else {
            KeyStatus::Active
        }
    }
}

pub async fn insert(
    pool: &PgPool,
    name: &KeyName,
    key: &NewApiKey,
    expires_in: Option<ExpiryDays>,
) -> Result<ApiKeyRecord, sqlx::Error> {
    // The database's clock, the one authentication compares against.
    // `make_interval(NULL)` is NULL, so no expiry stays NULL.
    sqlx::query_as(
        "INSERT INTO client_api_keys (name, key_prefix, key_hash, expires_at) \
         VALUES ($1, $2, $3, now() + make_interval(days => $4)) \
         RETURNING id, name, key_prefix, created_at, revoked_at, expires_at, \
                monthly_budget_usd::text AS monthly_budget_usd",
    )
    .bind(name.as_str())
    .bind(key.prefix())
    .bind(key.hash().as_bytes().as_slice())
    .bind(expires_in.map(|days| days.days() as i32))
    .fetch_one(pool)
    .await
}

/// All keys, oldest first (UUIDv7 IDs sort by creation time).
pub async fn list(pool: &PgPool) -> Result<Vec<ApiKeyRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, name, key_prefix, created_at, revoked_at, expires_at, \
                monthly_budget_usd::text AS monthly_budget_usd \
         FROM client_api_keys ORDER BY id",
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
         RETURNING id, name, key_prefix, created_at, revoked_at, expires_at, \
                monthly_budget_usd::text AS monthly_budget_usd",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    if let Some(record) = revoked {
        return Ok(RevokeOutcome::Revoked(record));
    }

    let existing: Option<ApiKeyRecord> = sqlx::query_as(
        "SELECT id, name, key_prefix, created_at, revoked_at, expires_at, \
                monthly_budget_usd::text AS monthly_budget_usd \
         FROM client_api_keys WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(match existing {
        Some(record) => RevokeOutcome::AlreadyRevoked(record),
        None => RevokeOutcome::NotFound,
    })
}

/// Sets or, with `None`, clears the key's monthly budget. Returns `None`
/// if there is no such key. It applies from the next request.
pub async fn set_budget(
    pool: &PgPool,
    id: Uuid,
    budget: Option<&UsdAmount>,
) -> Result<Option<ApiKeyRecord>, sqlx::Error> {
    sqlx::query_as(
        "UPDATE client_api_keys SET monthly_budget_usd = $2::text::numeric WHERE id = $1 \
         RETURNING id, name, key_prefix, created_at, revoked_at, expires_at, \
                monthly_budget_usd::text AS monthly_budget_usd",
    )
    .bind(id)
    .bind(budget.map(UsdAmount::as_str))
    .fetch_optional(pool)
    .await
}

/// What authentication needs to know about a presented key's hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyLookup {
    pub id: Uuid,
    pub revoked: bool,
    pub expired: bool,
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
/// Expiry is judged by the database's clock in the same query.
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
            sqlx::query_as::<_, (Uuid, bool, bool)>(
                "SELECT id, revoked_at IS NOT NULL, COALESCE(expires_at <= now(), false) \
                 FROM client_api_keys WHERE key_hash = $1",
            )
            .bind(hash.as_bytes().as_slice())
            .fetch_optional(&self.pool)
            .await
            .map(|row| {
                row.map(|(id, revoked, expired)| KeyLookup {
                    id,
                    revoked,
                    expired,
                })
            })
            .map_err(|error| KeyStoreError(error.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;

    fn record(
        revoked_at: Option<DateTime<Utc>>,
        expires_at: Option<DateTime<Utc>>,
    ) -> ApiKeyRecord {
        ApiKeyRecord {
            id: Uuid::nil(),
            name: "k".to_owned(),
            key_prefix: "ms_dev_abcd".to_owned(),
            created_at: DateTime::UNIX_EPOCH,
            revoked_at,
            expires_at,
            monthly_budget_usd: None,
        }
    }

    #[test]
    fn a_key_expires_at_the_exact_moment() {
        let at = DateTime::UNIX_EPOCH + TimeDelta::days(30);
        let key = record(None, Some(at));

        assert_eq!(key.status(at - TimeDelta::seconds(1)), KeyStatus::Active);
        assert_eq!(key.status(at), KeyStatus::Expired);
        assert_eq!(key.status(at + TimeDelta::seconds(1)), KeyStatus::Expired);
    }

    #[test]
    fn no_expiry_means_active_forever() {
        let far_future = DateTime::UNIX_EPOCH + TimeDelta::days(100_000);

        assert_eq!(record(None, None).status(far_future), KeyStatus::Active);
    }

    #[test]
    fn revocation_wins_over_expiry() {
        let at = DateTime::UNIX_EPOCH + TimeDelta::days(1);
        let key = record(Some(at), Some(at));

        assert_eq!(key.status(at), KeyStatus::Revoked);
    }
}
