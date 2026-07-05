//! umbral-testing — test helpers for umbral apps.
//!
//! Test-case + client ergonomics, in the Rust shape. The
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
//! consumed by test code — drop `umbral-testing` into a crate's
//! `[dev-dependencies]` and you don't carry it into release builds.
//!
//! ```ignore
//! use umbral_testing::{TempPool, TestClient};
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
        let path = dir.path().join("umbral_test.sqlite");
        let pool = SqlitePoolOptions::new()
            .max_connections(n)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true)
                    // A file-backed pool with >1 connection contends under the
                    // load of a full `cargo test --workspace` run; without a
                    // busy-timeout SQLite returns SQLITE_BUSY instantly instead
                    // of waiting, which surfaces as flaky "empty body" failures.
                    // Mirrors the 5s busy-timeout the framework's real
                    // `connect_sqlite` applies to production pools.
                    .busy_timeout(std::time::Duration::from_secs(5)),
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

// =========================================================================
// Factory — realistic test data (feature #79).
// =========================================================================

/// Re-export of the [`fake`] crate so factories can reach its generators
/// (`umbral_testing::fake::faker::...`, the `Fake` trait) without adding a
/// direct dependency of their own.
pub use fake;

use std::sync::atomic::{AtomicU64, Ordering};

/// A process-wide monotonic counter for unique values within a test run.
/// Use it to keep `unique` columns (slugs, emails, crate names) from
/// colliding across a `create_batch`:
///
/// ```ignore
/// slug: format!("plugin-{}", umbral_testing::seq()),
/// ```
pub fn seq() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed) + 1
}

/// Error from a [`Factory`] persistence call.
#[derive(Debug)]
pub enum FactoryError {
    /// The ORM write failed (constraint violation, missing table, an FK
    /// that doesn't exist yet, …).
    Write(umbral::orm::write::WriteError),
}

impl std::fmt::Display for FactoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FactoryError::Write(e) => write!(f, "factory write failed: {e}"),
        }
    }
}

impl std::error::Error for FactoryError {}

impl From<umbral::orm::write::WriteError> for FactoryError {
    fn from(e: umbral::orm::write::WriteError) -> Self {
        FactoryError::Write(e)
    }
}

/// A factory for producing realistic instances of a model — the
/// factory_boy / FactoryBot shape, in Rust.
///
/// You define a zero-sized marker type and point it at a [`Model`] through
/// the associated type. The orphan rule is why the impl lives on a marker
/// rather than on the model: in a downstream test crate both the model and
/// this trait are foreign, so `impl Factory for Plugin` wouldn't compile —
/// but `impl Factory for PluginFactory` (a local marker) does.
///
/// ```ignore
/// use umbral_testing::{Factory, fake::{Fake, faker::{lorem::en::*, company::en::*}}, seq};
///
/// struct PluginFactory;
/// impl Factory for PluginFactory {
///     type Model = Plugin;
///     fn build() -> Plugin {
///         let mut p = Plugin::default();
///         p.name = CompanyName().fake();
///         p.slug = format!("plugin-{}", seq());          // unique per call
///         p.short_description = Sentence(4..8).fake();
///         p
///     }
/// }
///
/// // In a test, after `App::builder()...build()` has set the ambient pool
/// // and the tables exist:
/// let one      = PluginFactory::create().await?;                    // one row
/// let many     = PluginFactory::create_batch(5).await?;             // five rows
/// let featured = PluginFactory::create_with(|p| p.featured = true).await?;
/// ```
///
/// [`build`](Factory::build) is pure (no I/O); the `create*` methods
/// persist through the ORM against the ambient pool, so a built app must
/// be in scope. Combine with [`TestClient`] to then exercise a handler
/// against the rows the factory produced.
///
/// [`Model`]: umbral::orm::Model
#[async_trait::async_trait]
pub trait Factory {
    /// The model this factory produces. The bound set is exactly what
    /// `#[derive(Model)]` already provides on every model (the ORM's
    /// `create` path needs `Serialize` + `FromRow` + `HydrateRelated`), so
    /// in practice you only ever write `type Model = YourModel;`.
    type Model: umbral::orm::Model
        + serde::Serialize
        + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
        + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
        + umbral::orm::HydrateRelated;

    /// A fresh, unsaved instance with realistic fake values. Pure — no
    /// database I/O. Override `unique` fields with [`seq`] so a batch
    /// doesn't collide.
    fn build() -> Self::Model;

    /// Build and persist one row through the ORM.
    async fn create() -> Result<Self::Model, FactoryError> {
        Self::create_with(|_| {}).await
    }

    /// Build one row, apply `tweak` to override specific fields, then
    /// persist. This is the `create(featured = true)` override hook.
    async fn create_with<F>(tweak: F) -> Result<Self::Model, FactoryError>
    where
        F: FnOnce(&mut Self::Model) + Send,
    {
        let mut instance = Self::build();
        tweak(&mut instance);
        umbral::orm::Manager::<Self::Model>::default()
            .create(instance)
            .await
            .map_err(FactoryError::Write)
    }

    /// Build and persist `n` rows.
    async fn create_batch(n: usize) -> Result<Vec<Self::Model>, FactoryError> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(Self::create().await?);
        }
        Ok(out)
    }
}
