//! Real failures carry no secrets (Stage 3).
//!
//! Stores log the database's error text as it comes, so that text must
//! never hold the connection string. Fakes cannot show this: their error
//! is invented. These tests make the real drivers fail, with canaries in
//! the connection details and the provider key.

use secrecy::SecretString;
use sqlx::PgPool;
use uuid::Uuid;

use modelswitch::accounting::{Failure, Outcome, RecordStore, RequestRecord};
use modelswitch::auth::api_key::KeyHash;
use modelswitch::budget::BudgetStore;
use modelswitch::config::DatabaseConfig;
use modelswitch::domain::{GenerateRequest, Provider, Target, Usage};
use modelswitch::providers::openai_compat::OpenAiCompatible;
use modelswitch::storage::api_keys::{KeyStore, PgKeyStore};
use modelswitch::storage::budgets::PgBudgetStore;
use modelswitch::storage::request_records::PgRecordStore;
use modelswitch::storage::{self, DatabaseCheck, PgDatabaseCheck};

const PASSWORD: &str = "CANARYpassword7";
const USER: &str = "canaryuser";
const DATABASE: &str = "canarydb";
const PROVIDER_KEY: &str = "CANARYproviderkey7";

/// The error text from all four stores whose errors are logged.
async fn store_errors(pool: PgPool) -> Vec<String> {
    let record = RequestRecord {
        request_id: format!("req_{}", Uuid::now_v7()),
        client_request_id: None,
        client_api_key_id: Uuid::now_v7(),
        requested_model: "m".to_owned(),
        provider: "p".to_owned(),
        model: "m".to_owned(),
        outcome: Outcome::Succeeded,
        failure: Failure::default(),
        latency_ms: 0,
        usage: Usage::unavailable(),
        attempts: Vec::new(),
    };
    let targets = [Target::parse("p/m").unwrap()];
    let check = PgDatabaseCheck::new(pool.clone());
    let keys = PgKeyStore::new(pool.clone());
    let budgets = PgBudgetStore::new(pool.clone());
    let records = PgRecordStore::new(pool);

    let (ping, key, budget, write) = tokio::join!(
        check.ping(),
        keys.find_by_hash(KeyHash::of("x")),
        budgets.check(Uuid::now_v7(), &targets),
        records.insert(&record),
    );
    vec![
        ping.unwrap_err().to_string(),
        key.unwrap_err().to_string(),
        budget.unwrap_err().to_string(),
        write.unwrap_err().to_string(),
    ]
}

fn assert_clean(errors: &[String]) {
    assert!(!errors.is_empty());
    for error in errors {
        assert!(!error.is_empty());
        for secret in [PASSWORD, USER, DATABASE, "postgres://"] {
            assert!(!error.contains(secret), "{secret} in: {error}");
        }
    }
}

fn lazy_pool(host_and_port: &str) -> PgPool {
    let url = format!("postgres://{USER}:{PASSWORD}@{host_and_port}/{DATABASE}");
    storage::connect_lazy(&DatabaseConfig {
        url: SecretString::from(url),
        max_connections: 2,
    })
    .unwrap()
}

#[tokio::test]
async fn a_refused_connection_names_no_connection_details() {
    assert_clean(&store_errors(lazy_pool("127.0.0.1:1")).await);
}

#[tokio::test]
async fn an_unknown_host_names_no_connection_details() {
    assert_clean(&store_errors(lazy_pool("db.modelswitch.invalid:5432")).await);
}

#[sqlx::test]
async fn a_rejected_password_is_not_repeated(pool: PgPool) {
    let options = (*pool.connect_options()).clone().password(PASSWORD);

    let result = PgDatabaseCheck::new(PgPool::connect_lazy_with(options))
        .ping()
        .await;

    // A server that trusts local connections accepts any password.
    if let Err(error) = result {
        let error = error.to_string();
        assert!(!error.contains(PASSWORD), "{error}");
        assert!(!error.contains("postgres://"), "{error}");
    }
}

#[tokio::test]
async fn a_failed_provider_call_names_neither_its_key_nor_its_url() {
    let provider = OpenAiCompatible::new(
        "http://127.0.0.1:1/canarypath",
        Some(SecretString::from(PROVIDER_KEY)),
    )
    .unwrap();
    let request = GenerateRequest {
        input: "PROMPT-CANARY".to_owned(),
        max_output_tokens: None,
    };

    let error = provider
        .generate("m", &request)
        .await
        .unwrap_err()
        .to_string();

    assert!(!error.is_empty());
    for secret in [PROVIDER_KEY, "canarypath", "PROMPT-CANARY"] {
        assert!(!error.contains(secret), "{secret} in: {error}");
    }
}
