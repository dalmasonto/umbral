//! umbra-testing — test helpers for umbra apps.
//!
//! Django's `TestCase` + `Client` ergonomics, in the Rust shape. The
//! repeated work in every plugin's `tests/integration.rs` was four
//! things: spin up a fresh sqlite pool, build the router, send
//! requests, read the response. This crate collapses those into:
//!
//! - [`TempPool`] — a tempfile-backed SQLite pool that's dropped
//!   when the guard goes out of scope.
//! - [`TestClient`] — wraps an [`axum::Router`] with HTTP-verb-
//!   shaped methods, a per-client cookie jar (so a session set on
//!   one request rides on the next), and JSON helpers.
//! - [`TestResponse`] — owns the response bytes and headers and
//!   exposes assertion helpers (`assert_status`, `body_json`,
//!   `assert_body_contains`).
//!
//! This crate is **NOT** a plugin. It's a sibling utility library
//! consumed by test code — drop `umbra-testing` into a crate's
//! `[dev-dependencies]` and you don't carry it into release builds.
//!
//! ```ignore
//! use umbra_testing::{TempPool, TestClient};
//!
//! #[tokio::test]
//! async fn list_endpoint_returns_seeded_rows() {
//!     let pool = TempPool::new().await;
//!     // ... build router using pool.handle() ...
//!     let client = TestClient::new(router);
//!     let resp = client.get("/api/notes").await;
//!     resp.assert_status_ok();
//!     let notes: Vec<Note> = resp.body_json();
//!     assert_eq!(notes.len(), 2);
//! }
//! ```

use std::sync::Mutex;

use axum::Router;
use axum::body::Body;
use http::header::{COOKIE, HeaderName, HeaderValue, SET_COOKIE};
use http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;
use tower::ServiceExt;

/// A tempfile-backed SQLite pool. Holding the [`TempPool`] keeps the
/// underlying directory alive; dropping it deletes the database file
/// and every WAL artefact alongside.
///
/// In-memory SQLite (`sqlite::memory:`) would be the obvious choice
/// but it isolates per-connection: pool size > 1 means different
/// connections see different databases. The tempfile path
/// sidesteps that completely.
pub struct TempPool {
    pool: SqlitePool,
    _dir: TempDir,
}

impl TempPool {
    /// Build a fresh pool with `max_connections = 5`.
    pub async fn new() -> Self {
        Self::with_max_connections(5).await
    }

    pub async fn with_max_connections(n: u32) -> Self {
        let dir = tempfile::tempdir().expect("tempdir for TempPool");
        let path = dir.path().join("umbra_test.sqlite");
        let pool = SqlitePoolOptions::new()
            .max_connections(n)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("connect to tempfile sqlite");
        Self { pool, _dir: dir }
    }

    /// Borrow the underlying pool. Clone for ownership.
    pub fn handle(&self) -> &SqlitePool {
        &self.pool
    }

    /// Clone the pool out. Each clone shares the same backing
    /// connection pool.
    pub fn clone_handle(&self) -> SqlitePool {
        self.pool.clone()
    }
}

/// A simple cookie jar: a flat list of `name=value` pairs. Good
/// enough for end-to-end test flows that exchange session and CSRF
/// cookies; not RFC 6265 compliant (no domain, path, or expiry
/// tracking).
#[derive(Default)]
struct CookieJar {
    cookies: Vec<(String, String)>,
}

impl CookieJar {
    fn set_from_header(&mut self, header: &str) {
        // Server `Set-Cookie` shape: `name=value; Path=/; ...`. Take
        // the bit before the first `;` as the name=value pair.
        let pair = header.split(';').next().unwrap_or("").trim();
        if let Some((name, value)) = pair.split_once('=') {
            self.cookies.retain(|(n, _)| n != name);
            self.cookies.push((name.to_string(), value.to_string()));
        }
    }

    fn cookie_header(&self) -> Option<String> {
        if self.cookies.is_empty() {
            return None;
        }
        Some(
            self.cookies
                .iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.cookies
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A test client over an axum [`Router`]. Stateful: cookies set on
/// one response automatically ride on the next request.
pub struct TestClient {
    router: Router,
    jar: Mutex<CookieJar>,
    default_headers: Mutex<HeaderMap>,
}

impl TestClient {
    pub fn new(router: Router) -> Self {
        Self {
            router,
            jar: Mutex::new(CookieJar::default()),
            default_headers: Mutex::new(HeaderMap::new()),
        }
    }

    /// Add a header that rides on every subsequent request. Useful
    /// for setting an `Authorization` once per test.
    pub fn set_default_header(&self, name: HeaderName, value: HeaderValue) {
        self.default_headers
            .lock()
            .expect("default headers poisoned")
            .insert(name, value);
    }

    /// Read a cookie the server has set on the jar.
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.jar
            .lock()
            .expect("cookie jar poisoned")
            .get(name)
            .map(str::to_string)
    }

    pub async fn get(&self, uri: &str) -> TestResponse {
        self.request(Method::GET, uri, Body::empty(), None).await
    }

    pub async fn post(&self, uri: &str, body: Body) -> TestResponse {
        self.request(Method::POST, uri, body, None).await
    }

    /// POST a value serialized to JSON with `Content-Type:
    /// application/json`.
    pub async fn post_json<T: Serialize + ?Sized>(&self, uri: &str, body: &T) -> TestResponse {
        let bytes = serde_json::to_vec(body).expect("serialize body");
        self.request(
            Method::POST,
            uri,
            Body::from(bytes),
            Some(("content-type", "application/json")),
        )
        .await
    }

    pub async fn put_json<T: Serialize + ?Sized>(&self, uri: &str, body: &T) -> TestResponse {
        let bytes = serde_json::to_vec(body).expect("serialize body");
        self.request(
            Method::PUT,
            uri,
            Body::from(bytes),
            Some(("content-type", "application/json")),
        )
        .await
    }

    pub async fn delete(&self, uri: &str) -> TestResponse {
        self.request(Method::DELETE, uri, Body::empty(), None).await
    }

    /// Send a fully-formed request. Use for verbs without a typed
    /// helper or for unusual headers.
    pub async fn send(&self, method: Method, uri: &str, body: Body) -> TestResponse {
        self.request(method, uri, body, None).await
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Body,
        content_type: Option<(&str, &str)>,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(uri);

        // Replay default headers.
        for (k, v) in self.default_headers.lock().expect("dh").iter() {
            builder = builder.header(k, v);
        }
        if let Some((k, v)) = content_type {
            builder = builder.header(k, v);
        }
        if let Some(c) = self.jar.lock().expect("jar").cookie_header() {
            builder = builder.header(COOKIE, c);
        }

        let req = builder.body(body).expect("build request");
        let resp = self
            .router
            .clone()
            .oneshot(req)
            .await
            .expect("router oneshot");

        // Harvest set-cookies into the jar before stripping the body.
        let status = resp.status();
        let headers = resp.headers().clone();
        for v in headers.get_all(SET_COOKIE) {
            if let Ok(s) = v.to_str() {
                self.jar.lock().expect("jar set").set_from_header(s);
            }
        }
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes();

        TestResponse {
            status,
            headers,
            body: bytes.to_vec(),
        }
    }
}

/// The result of one round trip. Owns the response bytes so the
/// caller can read them more than once (e.g. snapshot the raw body
/// before parsing JSON, then assert).
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl TestResponse {
    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub fn body_bytes(&self) -> &[u8] {
        &self.body
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Parse the body as JSON. Panics with the raw body in the
    /// message on a parse error — much friendlier in a failing test
    /// than a bare serde error.
    pub fn body_json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "body_json: failed to parse response as JSON ({e}). raw body:\n{}",
                self.body_text()
            )
        })
    }

    /// Read the value of a single response header. None if missing
    /// or non-UTF-8.
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }

    pub fn assert_status(&self, expected: StatusCode) -> &Self {
        assert_eq!(
            self.status,
            expected,
            "expected status {expected}, got {} with body:\n{}",
            self.status,
            self.body_text()
        );
        self
    }

    pub fn assert_status_ok(&self) -> &Self {
        self.assert_status(StatusCode::OK)
    }

    pub fn assert_body_contains(&self, needle: &str) -> &Self {
        let body = self.body_text();
        assert!(
            body.contains(needle),
            "expected body to contain {needle:?}\n--- got ---\n{body}\n-----------"
        );
        self
    }

    pub fn assert_header(&self, name: &str, expected: &str) -> &Self {
        let actual = self.header(name);
        assert_eq!(
            actual.as_deref(),
            Some(expected),
            "expected header {name} to be {expected:?}, got {actual:?}"
        );
        self
    }
}
