//! Fakes and helpers shared by the integration tests.

// Each test file is its own crate and uses a different subset of these.
#![allow(dead_code)]

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

use modelswitch::storage::{BoxFuture, DatabaseCheck, PingError};
use modelswitch::{AppState, build_router};

/// Deliberately sensitive, so leak tests have something to catch.
pub const SENSITIVE_ERROR: &str =
    "connection refused: postgres://modelswitch:hunter2@db.internal:5432/modelswitch_dev";

pub const DB_CHECK_TIMEOUT: Duration = Duration::from_secs(1);

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

pub fn state(db: FakeDatabase) -> AppState {
    AppState {
        db: Arc::new(db),
        db_check_timeout: DB_CHECK_TIMEOUT,
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
    /// Captures logs at `level` and above on the current thread until
    /// the returned guard is dropped. `#[tokio::test]` runs the whole
    /// test on one thread, so the request's logs land here.
    pub fn start(level: tracing::Level) -> (Self, tracing::subscriber::DefaultGuard) {
        let logs = Self::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(level)
            .with_ansi(false)
            .with_writer(logs.clone())
            .finish();
        (logs.clone(), tracing::subscriber::set_default(subscriber))
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
