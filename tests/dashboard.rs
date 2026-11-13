//! Serving the built dashboard from `admin.dashboard_dir` (ADR-036): only
//! `index.html` and `assets/`, with page security headers, never another
//! file in the folder, and never in place of an API answer.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use secrecy::SecretString;
use uuid::Uuid;

use common::{DbMode, FakeHistoryStore, TestResponse, send};
use modelswitch::api::admin::AdminState;
use modelswitch::build_admin_router;
use modelswitch::config::AdminConfig;
use modelswitch::metrics::Metrics;

const INDEX: &str = "<!doctype html><title>ModelSwitch</title>";
const SCRIPT: &str = "console.log('dashboard');";
const SECRET: &str = "not for the browser";

/// A built dashboard in a fresh folder, removed when dropped.
struct Build(PathBuf);

impl Build {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("modelswitch-dashboard-{}", Uuid::now_v7()));
        fs::create_dir_all(dir.join("assets")).unwrap();
        fs::write(dir.join("index.html"), INDEX).unwrap();
        fs::write(dir.join("assets").join("index-abc123.js"), SCRIPT).unwrap();
        // Beside the build, but not part of what the page may load.
        fs::write(dir.join("secret.txt"), SECRET).unwrap();
        fs::write(dir.join("assets").join("..secret"), SECRET).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Build {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn app(dashboard_dir: Option<&Path>) -> Router {
    let config = AdminConfig {
        bind: "127.0.0.1:9091".parse().unwrap(),
        token: SecretString::from("admin-token-for-tests-0123456789abcdef"),
        session_ttl: Duration::from_secs(3600),
        database_timeout: Duration::from_secs(1),
        dashboard_dir: dashboard_dir.map(Path::to_path_buf),
    };
    build_admin_router(AdminState::new(
        &config,
        Arc::new(Metrics::default()),
        Arc::new(FakeHistoryStore::new(DbMode::Up)),
    ))
}

async fn get(app: &Router, path: &str) -> TestResponse {
    send(app, Request::get(path).body(Body::empty()).unwrap()).await
}

fn assert_not_found_json(response: &TestResponse, path: &str) {
    assert_eq!(response.status, StatusCode::NOT_FOUND, "{path}");
    assert_eq!(response.json()["error"]["code"], "not_found", "{path}");
    assert!(!response.body.contains(SECRET), "{path}");
}

#[tokio::test]
async fn the_page_is_served_at_the_root_without_a_session() {
    let build = Build::new();
    let response = get(&app(Some(build.path())), "/").await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, INDEX);
    assert!(
        response
            .header("content-type")
            .unwrap()
            .starts_with("text/html")
    );
}

#[tokio::test]
async fn page_responses_carry_security_headers() {
    let build = Build::new();
    let app = app(Some(build.path()));

    for path in ["/", "/assets/index-abc123.js", "/assets/missing.js"] {
        let response = get(&app, path).await;
        let policy = response.header("content-security-policy").unwrap();
        for directive in [
            "default-src 'self'",
            "frame-ancestors 'none'",
            "object-src 'none'",
            "base-uri 'none'",
        ] {
            assert!(policy.contains(directive), "{path}: {policy}");
        }
        assert!(!policy.contains("unsafe"), "{path}: {policy}");
        assert_eq!(response.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(response.header("x-frame-options"), Some("DENY"));
        assert_eq!(response.header("referrer-policy"), Some("no-referrer"));
        assert_eq!(response.header("cache-control"), Some("no-cache"));
    }
}

#[tokio::test]
async fn assets_are_served_with_their_type() {
    let build = Build::new();
    let response = get(&app(Some(build.path())), "/assets/index-abc123.js").await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, SCRIPT);
    assert!(
        response
            .header("content-type")
            .unwrap()
            .contains("javascript")
    );
}

#[tokio::test]
async fn nothing_outside_index_and_assets_is_reachable() {
    let build = Build::new();
    let app = app(Some(build.path()));

    for path in [
        "/secret.txt",
        "/index.html",
        "/assets",
        "/assets/",
        "/assets/missing.js",
        "/assets/secret.txt",
        "/assets/index.html",
        "/assets/../secret.txt",
        "/assets/%2e%2e/secret.txt",
        "/assets/..%2fsecret.txt",
        "/assets/..%5csecret.txt",
        "/assets/%2e%2e%2fsecret.txt",
    ] {
        assert_not_found_json(&get(&app, path).await, path);
    }
}

#[tokio::test]
async fn the_api_still_answers_as_json() {
    let build = Build::new();
    let app = app(Some(build.path()));

    let response = get(&app, "/admin/api/session").await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert_eq!(response.json()["error"]["code"], "login_required");
    assert_eq!(response.header("cache-control"), Some("no-store"));

    assert_not_found_json(&get(&app, "/admin/api/unknown").await, "/admin/api/unknown");
}

#[tokio::test]
async fn without_a_dashboard_dir_only_the_api_is_served() {
    let app = app(None);

    assert_not_found_json(&get(&app, "/").await, "/");
    assert_not_found_json(&get(&app, "/assets/index-abc123.js").await, "/assets");
    let response = get(&app, "/admin/api/session").await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
}
