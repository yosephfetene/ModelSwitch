//! The dashboard's API, on the internal admin listener (ADR-036).
//!
//! - `POST /admin/api/login` checks the admin token once and sets a
//!   session cookie: `HttpOnly`, so page scripts cannot read it, and
//!   `SameSite=Strict`, so other sites cannot make the browser send it.
//! - `POST /admin/api/logout` ends the session.
//! - Every other API route needs a live session.
//! - With `admin.dashboard_dir` set, the page itself is served at `/`
//!   (see `api::dashboard`). It holds no data; it calls this API.
//!
//! Login and logout also need an `Origin` matching the page's own
//! address, so a page on another site cannot drive them. Neither the
//! token nor a session ID is ever logged.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, HOST, ORIGIN, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::json;
use tokio::time::Instant;

use crate::api::error::ApiError;
use crate::auth::admin::{AdminToken, LoginThrottle, Sessions};
use crate::config::AdminConfig;
use crate::history::HistoryStore;
use crate::metrics::Metrics;

/// The session cookie's name.
pub const SESSION_COOKIE: &str = "ms_admin_session";
/// The cookie is sent only to the admin API, never to the page's files.
const COOKIE_PATH: &str = "/admin/api";
/// A login body holds one token; nothing bigger is read.
const MAX_LOGIN_BODY_BYTES: usize = 4 * 1024;

/// Shared state of the admin listener.
#[derive(Clone)]
pub struct AdminState {
    pub token: AdminToken,
    pub sessions: Arc<Sessions>,
    pub throttle: Arc<LoginThrottle>,
    pub metrics: Arc<Metrics>,
    /// Request history, a trait object so tests can substitute a fake.
    pub history: Arc<dyn HistoryStore>,
    /// Upper bound on each history read.
    pub database_timeout: Duration,
    /// The built dashboard to serve, if any.
    pub dashboard_dir: Option<PathBuf>,
}

impl AdminState {
    pub fn new(
        config: &AdminConfig,
        metrics: Arc<Metrics>,
        history: Arc<dyn HistoryStore>,
    ) -> Self {
        Self {
            token: AdminToken::new(&config.token),
            sessions: Arc::new(Sessions::new(config.session_ttl)),
            throttle: Arc::default(),
            metrics,
            history,
            database_timeout: config.database_timeout,
            dashboard_dir: config.dashboard_dir.clone(),
        }
    }
}

/// The live session behind a request, for handlers.
#[derive(Debug, Clone, Copy)]
pub struct AdminSession {
    pub expires_in_secs: u64,
}

pub fn router(state: &AdminState) -> Router<AdminState> {
    let protected = Router::new()
        .route("/admin/api/session", get(session))
        .merge(crate::api::history::router())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));
    let api = Router::new()
        .route(
            "/admin/api/login",
            post(login).layer(DefaultBodyLimit::max(MAX_LOGIN_BODY_BYTES)),
        )
        .route("/admin/api/logout", post(logout))
        .merge(protected)
        // Admin answers describe private state: never keep a copy.
        .layer(middleware::map_response(no_store));
    match &state.dashboard_dir {
        Some(dir) => api.merge(crate::api::dashboard::router(dir)),
        None => api,
    }
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginBody {
    token: SecretString,
}

async fn login(
    State(state): State<AdminState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    require_same_origin(&headers)?;
    let now = Instant::now();
    if let Err(wait) = state.throttle.check(now) {
        tracing::warn!("admin: login refused, too many failed logins");
        return Err(ApiError::LoginThrottled {
            retry_after_secs: wait.as_secs_f64().ceil() as u64,
        });
    }

    let body = parse_login(&headers, body)?;
    if !state.token.matches(body.token.expose_secret()) {
        state.throttle.record_failure(now);
        state.metrics.record_admin_login_failure();
        tracing::warn!("admin: login failed, wrong admin token");
        return Err(ApiError::LoginFailed);
    }

    let id = state.sessions.start(now).map_err(|error| {
        tracing::error!(%error, "admin: could not generate a session ID");
        ApiError::Internal
    })?;
    tracing::info!("admin: logged in");
    let max_age = state.sessions.ttl().as_secs();
    Ok((
        StatusCode::NO_CONTENT,
        [(SET_COOKIE, cookie(id.expose_secret(), max_age))],
    )
        .into_response())
}

async fn logout(State(state): State<AdminState>, headers: HeaderMap) -> Result<Response, ApiError> {
    require_same_origin(&headers)?;
    if let Some(id) = session_id(&headers) {
        state.sessions.end(id);
        tracing::info!("admin: logged out");
    }
    // Clear the cookie even without a session: logging out always works.
    Ok((StatusCode::NO_CONTENT, [(SET_COOKIE, cookie("", 0))]).into_response())
}

async fn session(Extension(session): Extension<AdminSession>) -> Json<serde_json::Value> {
    Json(json!({ "expires_in_secs": session.expires_in_secs }))
}

/// Lets a request through only with a live session.
pub async fn require_session(
    State(state): State<AdminState>,
    mut request: Request,
    next: Next,
) -> Response {
    let remaining =
        session_id(request.headers()).and_then(|id| state.sessions.remaining(id, Instant::now()));
    match remaining {
        Some(remaining) => {
            request.extensions_mut().insert(AdminSession {
                expires_in_secs: remaining.as_secs(),
            });
            next.run(request).await
        }
        None => ApiError::LoginRequired.into_response(),
    }
}

fn cookie(value: &str, max_age_secs: u64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={value}; Path={COOKIE_PATH}; HttpOnly; SameSite=Strict; \
         Max-Age={max_age_secs}"
    ))
    .expect("session IDs are hex, so the cookie is a valid header")
}

/// The session ID from the `Cookie` header, if any.
fn session_id(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
}

/// Refuses a request whose `Origin` is missing or is not this listener's
/// own address, as given by `Host`. Browsers always send `Origin` on a
/// `POST`, and a page cannot forge it.
fn require_same_origin(headers: &HeaderMap) -> Result<(), ApiError> {
    let header = |name| headers.get(name).and_then(|value| value.to_str().ok());
    let same = match (header(ORIGIN), header(HOST)) {
        (Some(origin), Some(host)) => url::Url::parse(origin).is_ok_and(|origin| {
            matches!(origin.scheme(), "http" | "https")
                && origin.path() == "/"
                && origin.host_str().is_some_and(|name| {
                    let authority = match origin.port() {
                        Some(port) => format!("{name}:{port}"),
                        None => name.to_owned(),
                    };
                    authority.eq_ignore_ascii_case(host)
                })
        }),
        _ => false,
    };
    if same {
        Ok(())
    } else {
        tracing::warn!("admin: refused a request from another origin");
        Err(ApiError::Forbidden)
    }
}

fn parse_login(
    headers: &HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<LoginBody, ApiError> {
    let body = body.map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::PayloadTooLarge
        } else {
            ApiError::InvalidRequest("The request body could not be read.")
        }
    })?;
    let is_json = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return Err(ApiError::InvalidRequest(
            "Content-Type must be application/json.",
        ));
    }
    serde_json::from_slice(&body).map_err(|_| {
        ApiError::InvalidRequest("The body must be a JSON object with a string field \"token\".")
    })
}
