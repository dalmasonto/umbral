//! End-to-end tests for authentication + permissions + opt-in views.
//!
//! Boots a real `RestPlugin` against an in-memory SQLite with:
//!
//! - `FnAuthentication` that reads an `X-User: <id>` header so tests
//!   can simulate authenticated requests with one line. Real apps use
//!   `umbral-sessions::current_user`, HTTP Basic Auth, or a JWT — the
//!   trait shape is the same.
//! - `ReadOnly` permission on the `note` resource — anyone reads,
//!   nobody writes.
//! - `views([List, Retrieve])` scope on the `archive` resource —
//!   exposes only those two actions. A scoped-out write still hits a
//!   served URI (GET works), so it returns `405 Method Not Allowed`
//!   with an `Allow` header listing the verbs that *are* served — not a
//!   404, which would imply the URI doesn't exist.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{
    Action, FnAuthentication, Identity, IsStaff, ReadOnly, ResourceConfig, RestPlugin,
};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Note {
    id: i64,
    title: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Secret {
    id: i64,
    label: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Archive {
    id: i64,
    body: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Catalog {
    id: i64,
    name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

/// Every test in this binary shares one ambient SQLite pool (created once in
/// `boot()` and published into umbral-core's process-wide `OnceLock`s by
/// `App::build()`). The default test harness runs these `#[tokio::test]`s on
/// parallel OS threads, so they hammer that single pool concurrently — which
/// is what tripped the intermittent sqlite SIGSEGV under full-workspace runs
/// (gaps2 #30). Serialising the test bodies on this lock makes the shared pool
/// single-user-at-a-time. Mirrors the `TEST_LOCK` pattern in
/// `plugins/umbral-signals/tests/*`.
fn test_lock() -> &'static tokio::sync::Mutex<()> {
    static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &TEST_LOCK
}

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth_permission.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        // Test-shaped auth backend: read `X-User: <id>` and treat
        // `id == 99` as staff. Real apps wire this to umbral-sessions
        // or umbral-auth.
        let auth = FnAuthentication::new(|headers| async move {
            let user_id: i64 = headers.get("x-user")?.to_str().ok()?.parse().ok()?;
            Some(Identity::user(user_id).with_staff(user_id == 99))
        });

        let rest = RestPlugin::default()
            .authenticate(auth)
            // `note` is public-read, no-write.
            .resource(ResourceConfig::new("note").permission(ReadOnly))
            // `secret` is staff-only across all actions.
            .resource(ResourceConfig::new("secret").permission(IsStaff))
            // `archive` exposes only List + Retrieve. A scoped-out write
            // (POST/PUT/DELETE) hits a URI that still serves GET, so the
            // gate short-circuits with 405 + an `Allow` header.
            .resource(ResourceConfig::new("archive").views([Action::List, Action::Retrieve]))
            // `catalog` exposes ONLY List. The detail URI (`/catalog/{id}`)
            // serves no verb at all, so a request there 404s — the URI
            // genuinely isn't served, unlike `archive`'s detail (GET works).
            .resource(ResourceConfig::new("catalog").views([Action::List]));

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Note>()
            .model::<Secret>()
            .model::<Archive>()
            .model::<Catalog>()
            .plugin(rest)
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        for ddl in [
            "CREATE TABLE note (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)",
            "CREATE TABLE secret (id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT NOT NULL)",
            "CREATE TABLE archive (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL)",
            "CREATE TABLE catalog (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }
        sqlx::query("INSERT INTO note (title) VALUES ('hello'), ('world')")
            .execute(&pool)
            .await
            .expect("seed notes");
        sqlx::query("INSERT INTO secret (label) VALUES ('classified')")
            .execute(&pool)
            .await
            .expect("seed secret");
        sqlx::query("INSERT INTO archive (body) VALUES ('archived row')")
            .execute(&pool)
            .await
            .expect("seed archive");

        app.into_router()
    })
    .await
}

async fn send(
    router: axum::Router,
    method: &str,
    uri: &str,
    user: Option<i64>,
    body: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        req = req.header("x-user", u.to_string());
    }
    let req = if let Some(b) = body {
        req.header("content-type", "application/json")
            .body(Body::from(b.to_string()))
    } else {
        req.body(Body::empty())
    }
    .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed)
}

/// Like [`send`] but surfaces the `Allow` response header — needed by the
/// view-scope 405 tests, which assert the header lists exactly the verbs
/// the resource still serves.
async fn send_allow(
    router: axum::Router,
    method: &str,
    uri: &str,
    user: Option<i64>,
    body: Option<&str>,
) -> (StatusCode, String) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        req = req.header("x-user", u.to_string());
    }
    let req = if let Some(b) = body {
        req.header("content-type", "application/json")
            .body(Body::from(b.to_string()))
    } else {
        req.body(Body::empty())
    }
    .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let allow = resp
        .headers()
        .get(axum::http::header::ALLOW)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    (status, allow)
}

// =====================================================================
// ReadOnly: list/retrieve allowed, write methods denied.
// =====================================================================

#[tokio::test]
async fn readonly_list_succeeds_anonymously() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    let (status, _) = send(app, "GET", "/api/note/", None, None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn readonly_retrieve_succeeds_anonymously() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    let (status, _) = send(app, "GET", "/api/note/1", None, None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn readonly_create_returns_403() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    let (status, body) = send(
        app,
        "POST",
        "/api/note/",
        Some(1),
        Some(r#"{"title":"new"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
}

#[tokio::test]
async fn readonly_delete_returns_403() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    let (status, _) = send(app, "DELETE", "/api/note/1", Some(1), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// =====================================================================
// IsStaff: 401 anonymous, 403 non-staff, 200/2xx staff.
// =====================================================================

#[tokio::test]
async fn isstaff_anonymous_returns_401() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    let (status, body) = send(app, "GET", "/api/secret/", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthenticated");
}

#[tokio::test]
async fn isstaff_non_staff_returns_403() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    let (status, body) = send(app, "GET", "/api/secret/", Some(1), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
}

#[tokio::test]
async fn isstaff_staff_user_succeeds() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    let (status, _) = send(app, "GET", "/api/secret/", Some(99), None).await;
    assert_eq!(status, StatusCode::OK);
}

// =====================================================================
// Opt-in views: archive only exposes List + Retrieve.
// =====================================================================

#[tokio::test]
async fn opt_in_views_list_exposed() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    let (status, _) = send(app, "GET", "/api/archive/", None, None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn opt_in_views_retrieve_exposed() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    let (status, _) = send(app, "GET", "/api/archive/1", None, None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn opt_in_views_create_returns_405_with_allow() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    // POST is scoped out, but the collection still serves GET (List), so
    // the URI exists → 405, and `Allow` advertises only the served verbs.
    let (status, allow) = send_allow(
        app,
        "POST",
        "/api/archive/",
        Some(99),
        Some(r#"{"body":"new"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(
        allow.contains("OPTIONS") && allow.contains("GET"),
        "Allow lists the served verbs: {allow}"
    );
    assert!(
        !allow.contains("POST"),
        "POST is scoped out — must not appear in Allow: {allow}"
    );
}

#[tokio::test]
async fn opt_in_views_delete_returns_405_with_allow() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    let (status, allow) = send_allow(app, "DELETE", "/api/archive/1", Some(99), None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(
        allow.contains("OPTIONS") && allow.contains("GET"),
        "detail Allow lists the served verbs: {allow}"
    );
    assert!(
        !allow.contains("DELETE") && !allow.contains("PUT"),
        "scoped-out verbs must not appear in Allow: {allow}"
    );
}

#[tokio::test]
async fn opt_in_views_update_returns_405_with_allow() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    let (status, allow) = send_allow(
        app,
        "PUT",
        "/api/archive/1",
        Some(99),
        Some(r#"{"body":"x"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(
        allow.contains("OPTIONS") && allow.contains("GET") && !allow.contains("PUT"),
        "Allow lists served verbs only: {allow}"
    );
}

#[tokio::test]
async fn list_only_collection_post_returns_405() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    // `catalog` exposes only List, so the collection still serves GET →
    // a POST there is 405, Allow advertises just GET.
    let (status, allow) = send_allow(
        app,
        "POST",
        "/api/catalog/",
        Some(99),
        Some(r#"{"name":"x"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(
        allow.contains("GET") && !allow.contains("POST"),
        "Allow: {allow}"
    );
}

#[tokio::test]
async fn list_only_detail_uri_serves_nothing_returns_404() {
    let _guard = test_lock().lock().await;
    let app = boot().await.clone();
    // The detail URI serves NO verb (no Retrieve/Update/Delete), so it's
    // genuinely unserved → 404, not 405. No `Allow` header to advertise.
    let (status, allow) = send_allow(app, "GET", "/api/catalog/1", Some(99), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(allow.is_empty(), "a 404 carries no Allow header: {allow:?}");
}
