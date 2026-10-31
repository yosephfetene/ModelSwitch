//! An in-process mock HTTP provider on a random local port: it records
//! every request and answers with whatever the test scripts. No network
//! access or API key is involved.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Received {
    pub method: Method,
    pub uri: Uri,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Received {
    pub fn path(&self) -> &str {
        self.uri.path()
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

/// What the mock answers with.
#[derive(Clone)]
struct Reply {
    status: StatusCode,
    headers: Vec<(String, String)>,
    body: String,
}

#[derive(Clone)]
pub struct Mock {
    received: Arc<Mutex<Vec<Received>>>,
    reply: Arc<Mutex<Reply>>,
}

impl Mock {
    /// Starts a server answering every request with `status` and `body`;
    /// returns it with its root URL (`http://127.0.0.1:<port>`).
    pub async fn start(status: StatusCode, body: impl Into<String>) -> (Self, String) {
        let mock = Self {
            received: Arc::default(),
            reply: Arc::new(Mutex::new(Reply {
                status,
                headers: Vec::new(),
                body: body.into(),
            })),
        };
        let app = Router::new().fallback(handle).with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (mock, format!("http://{address}"))
    }

    pub fn with_header(&self, name: &str, value: &str) {
        self.reply
            .lock()
            .unwrap()
            .headers
            .push((name.to_owned(), value.to_owned()));
    }

    pub fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }
}

async fn handle(
    State(mock): State<Mock>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    mock.received.lock().unwrap().push(Received {
        method,
        uri,
        headers,
        body,
    });
    let reply = mock.reply.lock().unwrap().clone();
    let mut response = (
        reply.status,
        [(header::CONTENT_TYPE, "application/json")],
        reply.body,
    )
        .into_response();
    for (name, value) in reply.headers {
        response.headers_mut().insert(
            name.parse::<header::HeaderName>().unwrap(),
            value.parse().unwrap(),
        );
    }
    response
}

/// A port with (almost certainly) nothing listening: bound, then released.
pub async fn closed_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}
