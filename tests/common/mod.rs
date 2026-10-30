//! Fakes and helpers shared by the integration tests.

// Each test file is its own crate and uses a different subset of these.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

use secrecy::ExposeSecret;
use uuid::Uuid;

use modelswitch::auth::api_key::{KeyHash, NewApiKey};
use modelswitch::domain::{GenerateRequest, Generation, Provider, ProviderError, Usage};
use modelswitch::providers::Registry;
use modelswitch::routing::Catalog;
use modelswitch::storage::api_keys::{KeyLookup, KeyStore, KeyStoreError};
use modelswitch::storage::{BoxFuture, DatabaseCheck, PingError};
use modelswitch::{AppState, build_router};

/// Deliberately sensitive, so leak tests have something to catch.
pub const SENSITIVE_ERROR: &str =
    "connection refused: postgres://modelswitch:hunter2@db.internal:5432/modelswitch_dev";

pub const DB_CHECK_TIMEOUT: Duration = Duration::from_secs(1);
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
pub enum DbMode {
    Up,
    Down,
    Hang,
}

/// A database whose behavior the test controls, including switching it
/// between requests.
#[derive(Clone)]
pub struct FakeDatabase {
    mode: Arc<Mutex<DbMode>>,
}

impl FakeDatabase {
    pub fn new(mode: DbMode) -> Self {
        Self {
            mode: Arc::new(Mutex::new(mode)),
        }
    }

    pub fn set(&self, mode: DbMode) {
        *self.mode.lock().unwrap() = mode;
    }
}

impl DatabaseCheck for FakeDatabase {
    fn ping(&self) -> BoxFuture<'_, Result<(), PingError>> {
        let mode = *self.mode.lock().unwrap();
        Box::pin(async move {
            match mode {
                DbMode::Up => Ok(()),
                DbMode::Down => Err(PingError(SENSITIVE_ERROR.to_owned())),
                DbMode::Hang => std::future::pending().await,
            }
        })
    }
}

/// A key store whose contents and availability the test controls. Counts
/// lookups, so tests can assert the database was never asked.
#[derive(Clone)]
pub struct FakeKeyStore {
    mode: Arc<Mutex<DbMode>>,
    keys: Arc<Mutex<HashMap<KeyHash, KeyLookup>>>,
    lookups: Arc<AtomicUsize>,
}

impl FakeKeyStore {
    pub fn new(mode: DbMode) -> Self {
        Self {
            mode: Arc::new(Mutex::new(mode)),
            keys: Arc::default(),
            lookups: Arc::default(),
        }
    }

    pub fn set(&self, mode: DbMode) {
        *self.mode.lock().unwrap() = mode;
    }

    /// Stores a new active key and returns its plaintext and ID.
    pub fn add_key(&self) -> (String, Uuid) {
        let key = NewApiKey::generate().unwrap();
        let id = Uuid::now_v7();
        self.keys
            .lock()
            .unwrap()
            .insert(key.hash(), KeyLookup { id, revoked: false });
        (key.plaintext().expose_secret().to_owned(), id)
    }

    pub fn revoke(&self, key: &str) {
        if let Some(entry) = self.keys.lock().unwrap().get_mut(&KeyHash::of(key)) {
            entry.revoked = true;
        }
    }

    pub fn lookups(&self) -> usize {
        self.lookups.load(Ordering::SeqCst)
    }
}

impl KeyStore for FakeKeyStore {
    fn find_by_hash(
        &self,
        hash: KeyHash,
    ) -> BoxFuture<'_, Result<Option<KeyLookup>, KeyStoreError>> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        let mode = *self.mode.lock().unwrap();
        let found = self.keys.lock().unwrap().get(&hash).copied();
        Box::pin(async move {
            match mode {
                DbMode::Up => Ok(found),
                DbMode::Down => Err(KeyStoreError(SENSITIVE_ERROR.to_owned())),
                DbMode::Hang => std::future::pending().await,
            }
        })
    }
}

/// What a `FakeProvider` does when called.
#[derive(Clone)]
pub enum ProviderMode {
    Reply(Generation),
    Fail(String),
    Hang,
}

/// A provider whose behavior the test controls. Records every call as
/// (model, request).
#[derive(Clone)]
pub struct FakeProvider {
    mode: Arc<Mutex<ProviderMode>>,
    calls: Arc<Mutex<Vec<(String, GenerateRequest)>>>,
}

impl FakeProvider {
    pub fn new(mode: ProviderMode) -> Self {
        Self {
            mode: Arc::new(Mutex::new(mode)),
            calls: Arc::default(),
        }
    }

    pub fn replying(output: &str, usage: Usage) -> Self {
        Self::new(ProviderMode::Reply(Generation {
            output: output.to_owned(),
            usage,
        }))
    }

    pub fn calls(&self) -> Vec<(String, GenerateRequest)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Provider for FakeProvider {
    fn generate<'a>(
        &'a self,
        model: &'a str,
        request: &'a GenerateRequest,
    ) -> BoxFuture<'a, Result<Generation, ProviderError>> {
        self.calls
            .lock()
            .unwrap()
            .push((model.to_owned(), request.clone()));
        let mode = self.mode.lock().unwrap().clone();
        Box::pin(async move {
            match mode {
                ProviderMode::Reply(generation) => Ok(generation),
                ProviderMode::Fail(detail) => Err(ProviderError::Request(detail)),
                ProviderMode::Hang => std::future::pending().await,
            }
        })
    }
}

pub fn state(db: FakeDatabase) -> AppState {
    state_with_keys(db, FakeKeyStore::new(DbMode::Up))
}

pub fn state_with_keys(db: FakeDatabase, keys: FakeKeyStore) -> AppState {
    AppState {
        db: Arc::new(db),
        db_check_timeout: DB_CHECK_TIMEOUT,
        keys: Arc::new(keys),
        auth_timeout: AUTH_TIMEOUT,
        catalog: Arc::new(Catalog::default()),
        providers: Arc::new(Registry::default()),
    }
}

pub fn app(db: FakeDatabase) -> Router {
    build_router(state(db))
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl TestResponse {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|value| value.to_str().unwrap())
    }
}

pub async fn send(app: &Router, request: Request<Body>) -> TestResponse {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    TestResponse {
        status,
        headers,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

pub async fn get(app: &Router, path: &str) -> TestResponse {
    send(app, Request::get(path).body(Body::empty()).unwrap()).await
}

/// Collects formatted log output in memory so tests can search it.
#[derive(Clone, Default)]
pub struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    /// Installs one process-wide subscriber that captures every log line
    /// at all levels, and returns its buffer. Call it first in every test
    /// of a log-asserting test binary.
    ///
    /// Global, not per-thread (`set_default`), on purpose: `tracing`
    /// caches per log statement whether anyone listens, process-wide. A
    /// parallel test hitting the same statement on a thread without a
    /// subscriber could cache "nobody listens" and silently empty the
    /// capture. Tests share the buffer, so each one should only inspect
    /// lines that carry its own request ID.
    pub fn global() -> &'static Self {
        static LOGS: OnceLock<CapturedLogs> = OnceLock::new();
        LOGS.get_or_init(|| {
            let logs = Self::default();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .with_writer(logs.clone())
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("no other global subscriber in this test binary");
            logs
        })
    }

    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedLogs {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
