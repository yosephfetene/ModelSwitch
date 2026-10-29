//! `modelswitch keys` against a real PostgreSQL. `#[sqlx::test]` gives
//! each test a fresh database with `migrations/` already applied.

mod common;

use sqlx::PgPool;
use uuid::Uuid;

use common::CapturedLogs;
use modelswitch::auth::api_key::{KeyHash, KeyName, NewApiKey};
use modelswitch::cli::keys;
use modelswitch::storage::{self, api_keys};

/// Runs `keys create` and returns (output, plaintext key).
async fn create(pool: &PgPool, name: &str) -> (String, String) {
    let mut out = Vec::new();
    keys::create(pool, name, &mut out).await.unwrap();
    let out = String::from_utf8(out).unwrap();
    let key = out.lines().last().unwrap().to_owned();
    (out, key)
}

async fn list(pool: &PgPool) -> String {
    let mut out = Vec::new();
    keys::list(pool, &mut out).await.unwrap();
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
        let result = keys::create(&pool, name, &mut Vec::new()).await;

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
    let key = NewApiKey::from_random_bytes([9; 32]);
    let name = KeyName::parse("dup").unwrap();

    api_keys::insert(&pool, &name, &key).await.unwrap();
    let second = api_keys::insert(&pool, &name, &key).await;

    assert!(second.is_err(), "duplicate hash accepted");
}

#[sqlx::test]
async fn migrations_can_run_again_safely(pool: PgPool) {
    storage::MIGRATOR.run(&pool).await.unwrap();
}
