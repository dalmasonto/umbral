//! Tests for gap 26 — `login_required` guard.
//!
//! Covers:
//!
//! 1. `LoggedIn<U>` extractor returns the user on a valid session.
//! 2. `LoggedIn<U>` returns 401 JSON when there is no session
//!    (`LoginRequired::API` default).
//! 3. `LoggedIn<U>` returns 302 to `/login?next=/protected` on no
//!    session when the layer has been configured with
//!    `LoginRequired::html("/login")`.
//! 4. `LoginRequiredLayer` gates a whole router subtree: unauthenticated
//!    → 401 / 302 before the handler runs; authenticated → handler runs.
//! 5. `LoginRequiredLayer` + `LoggedIn<U>` compose — the layer inserts
//!    `LoginRequired` into extensions and the extractor picks it up so
//!    there is no double-check and the config flows down correctly.
//!
//! Test structure mirrors `tests/integration.rs`: one shared DB boot via
//! `tokio::sync::OnceCell`, raw SQL DDL to create the required tables
//! (`session`, `auth_user`), and `tower::ServiceExt::oneshot` to drive
//! the routers.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use http_body_util::BodyExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral_auth::{
    AuthPlugin, AuthUser, hash_password,
    login_required::{
        LoggedIn, LoginRequired, LoginRequiredLayer, login_required, login_required_html,
    },
};

// =========================================================================
// One-time DB boot
// =========================================================================

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("umbral_login_required.sqlite");
        std::mem::forget(tmp);

        let opts = SqliteConnectOptions::new()
            .busy_timeout(std::time::Duration::from_secs(5))
            .filename(&db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await
            .expect("sqlite connect");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(umbral_sessions::SessionsPlugin::default().without_auto_layer())
            .plugin(AuthPlugin::<AuthUser>::default())
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

// =========================================================================
// Helpers — insert a user and create a session
// =========================================================================

/// Insert an `auth_user` row and return its id.
async fn insert_user(username: &str) -> i64 {
    let pool = umbral::db::pool();
    let hash = hash_password("testpass").expect("hash");
    let now = chrono::Utc::now().to_rfc3339();
    let row: (i64,) = sqlx::query_as(
        "INSERT INTO auth_user
           (username, email, password_hash, is_active, is_staff, is_superuser, date_joined)
         VALUES (?, ?, ?, 1, 0, 0, ?)
         RETURNING id",
    )
    .bind(username)
    .bind(format!("{username}@example.com"))
    .bind(&hash)
    .bind(&now)
    .fetch_one(&pool)
    .await
    .expect("insert user");
    row.0
}

/// Create a session for `user_id` and return the raw token (what goes in
/// the cookie). Stores `sha256(token)` in the DB, same as umbral-sessions.
fn hash_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    format!("{:x}", h.finalize())
}

async fn create_session_for(user_id: i64) -> String {
    use uuid::Uuid;
    let pool = umbral::db::pool();
    let raw = Uuid::new_v4().to_string();
    let stored = hash_token(&raw);
    let now = chrono::Utc::now();
    let expires = now + chrono::Duration::days(14);
    sqlx::query(
        "INSERT INTO session (id, user_id, data, created_at, expires_at)
         VALUES (?, ?, '{}', ?, ?)",
    )
    .bind(&stored)
    // Session.user_id is text post-gap-#59. Bind the i64 as its
    // Display form to match the framework's storage convention.
    .bind(user_id.to_string())
    .bind(now.to_rfc3339())
    .bind(expires.to_rfc3339())
    .execute(&pool)
    .await
    .expect("insert session");
    raw
}

/// Create an **anonymous** session (user_id = NULL).
async fn create_anonymous_session() -> String {
    use uuid::Uuid;
    let pool = umbral::db::pool();
    let raw = Uuid::new_v4().to_string();
    let stored = hash_token(&raw);
    let now = chrono::Utc::now();
    let expires = now + chrono::Duration::days(14);
    sqlx::query(
        "INSERT INTO session (id, user_id, data, created_at, expires_at)
         VALUES (?, NULL, '{}', ?, ?)",
    )
    .bind(&stored)
    .bind(now.to_rfc3339())
    .bind(expires.to_rfc3339())
    .execute(&pool)
    .await
    .expect("insert anonymous session");
    raw
}

/// Build a GET request with an optional session cookie.
fn req_with_cookie(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(c) = cookie {
        builder = builder.header("cookie", format!("umbral_session={c}"));
    }
    builder.body(Body::empty()).unwrap()
}

// =========================================================================
// Test 1 — extractor returns user on valid session
// =========================================================================

async fn handler_loggedin(LoggedIn(user): LoggedIn<AuthUser>) -> String {
    format!("hello:{}", user.username)
}

#[tokio::test]
async fn extractor_returns_user_on_valid_session() {
    boot().await;

    let user_id = insert_user("lr_user1").await;
    let token = create_session_for(user_id).await;

    let router = Router::new().route("/protected", get(handler_loggedin));
    let req = req_with_cookie("/protected", Some(&token));
    let resp = router.oneshot(req).await.expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("hello:lr_user1"),
        "body should contain username; got: {text}",
    );
}

// =========================================================================
// Test 2 — extractor returns 401 JSON on no session (API default)
// =========================================================================

#[tokio::test]
async fn extractor_returns_401_on_no_session_api_default() {
    boot().await;

    let router = Router::new().route("/protected", get(handler_loggedin));
    let req = req_with_cookie("/protected", None);
    let resp = router.oneshot(req).await.expect("oneshot");

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("application/json"),
        "content-type should be application/json; got: {ct}",
    );
}

// =========================================================================
// Test 3 — extractor returns 302 to /login?next=/protected when layer
//          has injected LoginRequired::html
// =========================================================================

async fn handler_loggedin_html(LoggedIn(user): LoggedIn<AuthUser>) -> String {
    format!("html:{}", user.username)
}

#[tokio::test]
async fn extractor_returns_302_when_html_config_injected() {
    boot().await;

    // Build a router where the layer has set the HTML config.
    let inner = Router::new().route("/protected", get(handler_loggedin_html));
    let router = LoginRequiredLayer::new(LoginRequired::html("/login")).apply(inner);

    let req = req_with_cookie("/protected", None);
    let resp = router.oneshot(req).await.expect("oneshot");

    assert_eq!(resp.status(), StatusCode::FOUND);
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        location.starts_with("/login"),
        "location should start with /login; got: {location}",
    );
    assert!(
        location.contains("next="),
        "location should contain next= param; got: {location}",
    );
    assert!(
        location.contains("%2Fprotected") || location.contains("/protected"),
        "location should encode the original path; got: {location}",
    );
}

// =========================================================================
// Test 4 — layer gates a whole router subtree
// =========================================================================

async fn plain_handler() -> &'static str {
    "open"
}

async fn gated_handler() -> &'static str {
    "secret"
}

#[tokio::test]
async fn layer_gates_router_subtree_api() {
    boot().await;

    let gated = Router::new()
        .route("/secret", get(gated_handler))
        .layer(login_required());

    let open = Router::new().route("/open", get(plain_handler));
    let router = open.merge(gated);

    // Unauthenticated request to gated route → 401.
    let resp = router
        .clone()
        .oneshot(req_with_cookie("/secret", None))
        .await
        .expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "unauthenticated request to gated route should get 401",
    );

    // Unauthenticated request to open route → 200.
    let resp = router
        .clone()
        .oneshot(req_with_cookie("/open", None))
        .await
        .expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "open route should be accessible without a session",
    );

    // Authenticated request to gated route → 200.
    let user_id = insert_user("lr_layer1").await;
    let token = create_session_for(user_id).await;
    let resp = router
        .oneshot(req_with_cookie("/secret", Some(&token)))
        .await
        .expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "authenticated request to gated route should get 200",
    );
}

#[tokio::test]
async fn layer_gates_router_subtree_html_redirect() {
    boot().await;

    let inner = Router::new().route("/dashboard", get(gated_handler));
    let router = login_required_html("/login").apply(inner);

    let resp = router
        .oneshot(req_with_cookie("/dashboard", None))
        .await
        .expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::FOUND,
        "unauthenticated request should redirect",
    );
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        location.starts_with("/login"),
        "redirect should go to /login; got: {location}",
    );
}

// =========================================================================
// Test 5 — layer + extractor compose: config flows into extractor
// =========================================================================

async fn handler_with_extractor_shows_config(
    LoggedIn(user): LoggedIn<AuthUser>,
) -> impl IntoResponse {
    format!("config-flows:{}", user.username)
}

#[tokio::test]
async fn layer_config_flows_into_extractor() {
    boot().await;

    // Layer with HTML config wraps a handler that uses the extractor.
    // If the authenticated request passes the layer gate, the extractor
    // should find the LoginRequired::html config in extensions and not
    // double-reject. We verify the handler runs successfully.
    let inner = Router::new().route("/guarded", get(handler_with_extractor_shows_config));
    let router = LoginRequiredLayer::new(LoginRequired::html("/login")).apply(inner);

    let user_id = insert_user("lr_compose1").await;
    let token = create_session_for(user_id).await;

    let resp = router
        .oneshot(req_with_cookie("/guarded", Some(&token)))
        .await
        .expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "authenticated request through layer+extractor should succeed",
    );
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("lr_compose1"),
        "handler should run and return username; got: {text}",
    );
}

// =========================================================================
// Test — anonymous session (user_id IS NULL) is rejected the same as no
// session, because the account is not authenticated.
// =========================================================================

#[tokio::test]
async fn anonymous_session_is_rejected() {
    boot().await;

    let anon_token = create_anonymous_session().await;
    let router = Router::new().route("/protected", get(handler_loggedin));
    let req = req_with_cookie("/protected", Some(&anon_token));
    let resp = router.oneshot(req).await.expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "anonymous session must not authenticate",
    );
}

// =========================================================================
// Test — BUG-18 from bugs/tests/testBugs.md: LoggedIn<U> now Derefs to U
// and serialises transparently. Templates / Json responses can hand back
// the extracted value without writing `user.0.username()` every time.
// =========================================================================

#[tokio::test]
async fn loggedin_derefs_to_inner_user() {
    boot().await;
    let user_id = insert_user("deref_alice").await;
    let row: AuthUser = umbral::orm::Manager::<AuthUser>::default()
        .filter(umbral::orm::Predicate::<AuthUser>::col_eq("id", user_id))
        .first()
        .await
        .expect("query ok")
        .expect("user present");
    let wrapped = LoggedIn(row);
    // No `.0` — Deref does the projection.
    assert_eq!(wrapped.username, "deref_alice");
    assert!(wrapped.is_active);
}

#[tokio::test]
async fn loggedin_serialises_as_inner_user() {
    boot().await;
    let user_id = insert_user("ser_alice").await;
    let row: AuthUser = umbral::orm::Manager::<AuthUser>::default()
        .filter(umbral::orm::Predicate::<AuthUser>::col_eq("id", user_id))
        .first()
        .await
        .expect("query ok")
        .expect("user present");
    let wrapped = LoggedIn(row.clone());
    let direct = serde_json::to_value(&row).expect("direct serialise");
    let wrapped_json = serde_json::to_value(&wrapped).expect("wrapped serialise");
    assert_eq!(
        wrapped_json, direct,
        "LoggedIn(user) must serialise as the raw user, no wrapping object",
    );
}
