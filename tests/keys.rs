//! `modelswitch keys` against a real PostgreSQL. `#[sqlx::test]` gives
//! each test a fresh database with `migrations/` already applied.

mod common;

use chrono::{TimeDelta, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use common::CapturedLogs;
use modelswitch::auth::api_key::{KeyEnvironment, KeyHash, KeyName, NewApiKey};
use modelswitch::cli::keys::{self, CreateOptions};
use modelswitch::storage::{self, api_keys};

/// Runs `keys create` and returns (output, plaintext key).
async fn create(pool: &PgPool, name: &str) -> (String, String) {
    create_with(pool, name, CreateOptions::default()).await
}

async fn create_with(pool: &PgPool, name: &str, options: CreateOptions) -> (String, String) {
    let mut out = Vec::new();
    keys::create(pool, name, options, &mut out).await.unwrap();
    let out = String::from_utf8(out).unwrap();
    let key = out.lines().last().unwrap().to_owned();
    (out, key)
}

async fn list(pool: &PgPool) -> String {
    let mut out = Vec::new();
    keys::list(pool, Utc::now(), &mut out).await.unwrap();
    String::from_utf8(out).unwrap()
}

async fn revoke(pool: &PgPool, id: Uuid) -> anyhow::Result<String> {
    let mut out = Vec::new();
    keys::revoke(pool, id, &mut out).await?;
    Ok(String::from_utf8(out).unwrap())
}

/// Every stored column of every key, as text.
async fn table_dump(pool: &PgPool) -> String {
    sqlx::query_scalar::<_, String>("SELECT row_to_json(k)::text FROM client_api_keys k")
        .fetch_all(pool)
        .await
        .unwrap()
        .join("\n")
}

async fn only_key_id(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM client_api_keys")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn create_prints_the_key_once_with_id_and_prefix(pool: PgPool) {
    let (out, key) = create(&pool, "laptop").await;
    let id = only_key_id(&pool).await;

    assert!(key.starts_with("ms_dev_"), "{out}");
    assert_eq!(out.matches(&key).count(), 1, "{out}");
    assert!(out.contains(&id.to_string()), "{out}");
    assert!(out.contains(&key[..11]), "{out}");
}

#[sqlx::test]
async fn create_stores_hash_and_prefix_never_plaintext(pool: PgPool) {
    let (_, key) = create(&pool, "laptop").await;

    let (prefix, hash): (String, Vec<u8>) =
        sqlx::query_as("SELECT key_prefix, key_hash FROM client_api_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(prefix, key[..11]);
    assert_eq!(hash, KeyHash::of(&key).as_bytes());
    let body = key.strip_prefix("ms_dev_").unwrap();
    let dump = table_dump(&pool).await;
    assert!(!dump.contains(body), "plaintext stored: {dump}");
}

#[sqlx::test]
async fn create_rejects_invalid_name_before_writing(pool: PgPool) {
    for name in ["   ", &"x".repeat(65), "evil\nname"] {
        let result = keys::create(&pool, name, CreateOptions::default(), &mut Vec::new()).await;

        assert!(result.is_err(), "{name:?} accepted");
    }
    assert_eq!(table_dump(&pool).await, "");
}

#[sqlx::test]
async fn create_never_logs_the_key(pool: PgPool) {
    let logs = CapturedLogs::global();

    let (_, key) = create(&pool, "laptop").await;

    let all = logs.text();
    let id = only_key_id(&pool).await.to_string();
    // Canary: the audit line for this key was captured...
    assert!(
        all.lines()
            .any(|line| line.contains("api key created") && line.contains(&id)),
        "{all}"
    );
    // ...and the key itself is nowhere in the logs.
    assert!(!all.contains(key.strip_prefix("ms_dev_").unwrap()), "{all}");
}

#[sqlx::test]
async fn list_shows_prefix_and_status_never_full_key(pool: PgPool) {
    let (_, laptop) = create(&pool, "laptop").await;
    let (_, ci) = create(&pool, "ci runner").await;

    let out = list(&pool).await;

    for (name, key) in [("laptop", &laptop), ("ci runner", &ci)] {
        let line = out
            .lines()
            .find(|line| line.ends_with(name))
            .unwrap_or_else(|| panic!("{name} not listed:\n{out}"));
        assert!(line.contains(&key[..11]), "{line}");
        assert!(line.contains("active"), "{line}");
        assert!(!out.contains(key.as_str()), "full key listed:\n{out}");
    }
}

#[sqlx::test]
async fn list_without_keys_explains_how_to_create_one(pool: PgPool) {
    let out = list(&pool).await;

    assert!(out.contains("modelswitch keys create"), "{out}");
}

#[sqlx::test]
async fn revoke_marks_the_key_revoked(pool: PgPool) {
    create(&pool, "laptop").await;
    let id = only_key_id(&pool).await;

    let out = revoke(&pool, id).await.unwrap();

    assert!(out.starts_with("Revoked key"), "{out}");
    let listed = list(&pool).await;
    assert!(listed.contains("revoked"), "{listed}");
}

#[sqlx::test]
async fn revoking_twice_keeps_the_original_time(pool: PgPool) {
    create(&pool, "laptop").await;
    let id = only_key_id(&pool).await;
    let revoked_at = || async {
        sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
            "SELECT revoked_at FROM client_api_keys",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };

    revoke(&pool, id).await.unwrap();
    let first = revoked_at().await;
    let out = revoke(&pool, id).await.unwrap();

    assert!(out.contains("already revoked"), "{out}");
    assert_eq!(revoked_at().await, first);
}

#[sqlx::test]
async fn revoking_an_unknown_id_fails(pool: PgPool) {
    let error = revoke(&pool, Uuid::now_v7()).await.unwrap_err();

    assert!(error.to_string().contains("no key with id"), "{error}");
}

#[sqlx::test]
async fn database_rejects_duplicate_key_hashes(pool: PgPool) {
    let key = NewApiKey::from_random_bytes(KeyEnvironment::Dev, [9; 32]);
    let name = KeyName::parse("dup").unwrap();

    api_keys::insert(&pool, &name, &key, None).await.unwrap();
    let second = api_keys::insert(&pool, &name, &key, None).await;

    assert!(second.is_err(), "duplicate hash accepted");
}

fn expiring_in(days: u32) -> CreateOptions {
    CreateOptions {
        expires_in_days: Some(days),
        ..Default::default()
    }
}

#[sqlx::test]
async fn create_makes_a_live_key_on_request(pool: PgPool) {
    let options = CreateOptions {
        environment: KeyEnvironment::Live,
        ..Default::default()
    };

    let (out, key) = create_with(&pool, "prod", options).await;

    assert!(key.starts_with("ms_live_"), "{out}");
    let prefix: String = sqlx::query_scalar("SELECT key_prefix FROM client_api_keys")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(prefix, key[..12]);
    assert!(list(&pool).await.contains(&prefix));
}

#[sqlx::test]
async fn a_new_key_never_expires_unless_asked(pool: PgPool) {
    let (out, _) = create(&pool, "laptop").await;

    let never: bool = sqlx::query_scalar("SELECT expires_at IS NULL FROM client_api_keys")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(never);
    assert!(!out.contains("expires"), "{out}");
    let listing = list(&pool).await;
    let line = listing.lines().last().unwrap();
    assert!(line.contains("never"), "{line}");
}

#[sqlx::test]
async fn expiry_is_counted_from_creation(pool: PgPool) {
    let (out, key) = create_with(&pool, "contractor", expiring_in(30)).await;

    let exact: bool = sqlx::query_scalar(
        "SELECT expires_at = created_at + interval '30 days' FROM client_api_keys",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(exact);
    assert!(out.contains("It expires"), "{out}");
    assert_eq!(out.lines().last().unwrap(), key, "the key stays last");
}

#[sqlx::test]
async fn create_rejects_invalid_expiry_before_writing(pool: PgPool) {
    for days in [0, 3651] {
        let options = expiring_in(days);

        let result = keys::create(&pool, "x", options, &mut Vec::new()).await;

        let error = result.unwrap_err().to_string();
        assert!(error.contains("between 1 and 3650"), "{error}");
    }
    assert_eq!(table_dump(&pool).await, "");
}

#[sqlx::test]
async fn list_shows_expired_keys_as_of_the_time_given(pool: PgPool) {
    create_with(&pool, "contractor", expiring_in(30)).await;
    let listing_at = async |now| {
        let mut out = Vec::new();
        keys::list(&pool, now, &mut out).await.unwrap();
        String::from_utf8(out).unwrap()
    };

    let today = listing_at(Utc::now()).await;
    let later = listing_at(Utc::now() + TimeDelta::days(31)).await;

    assert!(today.lines().last().unwrap().contains("active"), "{today}");
    assert!(later.lines().last().unwrap().contains("expired"), "{later}");
}

#[sqlx::test]
async fn an_expired_key_can_still_be_revoked(pool: PgPool) {
    create_with(&pool, "contractor", expiring_in(1)).await;
    let id = only_key_id(&pool).await;
    sqlx::query(
        "UPDATE client_api_keys SET created_at = now() - interval '2 days', \
                expires_at = now() - interval '1 day'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(list(&pool).await.contains("expired"));

    revoke(&pool, id).await.unwrap();

    assert!(
        list(&pool)
            .await
            .lines()
            .last()
            .unwrap()
            .contains("revoked")
    );
}

#[sqlx::test]
async fn the_schema_rejects_an_expiry_before_creation(pool: PgPool) {
    create(&pool, "laptop").await;

    let result =
        sqlx::query("UPDATE client_api_keys SET expires_at = created_at - interval '1 second'")
            .execute(&pool)
            .await;

    assert!(result.is_err(), "an impossible expiry was stored");
}

#[sqlx::test]
async fn migrations_can_run_again_safely(pool: PgPool) {
    storage::MIGRATOR.run(&pool).await.unwrap();
}
