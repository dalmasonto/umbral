//! audit_2 H1/P2 — object-level row scoping. `ResourceConfig::owned_by` /
//! `.scope(...)` restricts EVERY built-in CRUD action to the rows the caller
//! may access, so a caller past the model-level permission gate can't
//! read/update/delete another owner's row by id (IDOR), and list returns only
//! in-scope rows. Out-of-scope rows are 404 (never revealed).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral::auth::{FnAuthentication, Identity};
use umbral_rest::{AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Doc {
    id: i64,
    title: String,
    #[umbral(string)]
    owner_id: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("object_scope.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        // Auth: the `x-user` header IS the identity (a test stand-in for a real
        // session/bearer resolver). AllowAny isolates the test to SCOPING.
        let auth = FnAuthentication::new(|headers: umbral::web::HeaderMap| async move {
            headers
                .get("x-user")
                .and_then(|v| v.to_str().ok())
                .map(Identity::user)
        });
        let resource = ResourceConfig::new("doc")
            .permission(AllowAny)
            .owned_by("owner_id");
        let rest = RestPlugin::default().authenticate(auth).resource(resource);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Doc>()
            .plugin(rest)
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();
        sqlx::query("INSERT INTO doc (id, title, owner_id) VALUES (1, 'alice-doc', 'alice'), (2, 'bob-doc', 'bob')")
            .execute(&pool)
            .await
            .expect("seed");

        app.into_router()
    })
    .await
}

async fn req(
    method: Method,
    uri: &str,
    user: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let router = boot().await.clone();
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        b = b.header("x-user", u);
    }
    let request = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = router.oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn list_returns_only_the_callers_rows() {
    let (status, body) = req(Method::GET, "/api/doc/", Some("alice"), None).await;
    assert_eq!(status, StatusCode::OK);
    // The list envelope carries `results`; alice sees only her one row.
    let results = body
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(results.len(), 1, "alice must see only her row: {body}");
    assert_eq!(results[0]["owner_id"], "alice");
}

#[tokio::test]
async fn retrieve_own_ok_others_404() {
    let (own, _) = req(Method::GET, "/api/doc/1", Some("alice"), None).await;
    assert_eq!(own, StatusCode::OK, "alice reads her own row");
    let (other, _) = req(Method::GET, "/api/doc/2", Some("alice"), None).await;
    assert_eq!(
        other,
        StatusCode::NOT_FOUND,
        "alice must NOT read bob's row (IDOR)"
    );
}

#[tokio::test]
async fn update_others_row_is_404() {
    let (s, _) = req(
        Method::PATCH,
        "/api/doc/2",
        Some("alice"),
        Some(json!({ "title": "hijacked" })),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "alice must NOT update bob's row");
    // Confirm bob's row is untouched.
    let (s2, body) = req(Method::GET, "/api/doc/2", Some("bob"), None).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(body["title"], "bob-doc", "bob's row must be unchanged");
}

#[tokio::test]
async fn delete_others_row_is_404() {
    let (s, _) = req(Method::DELETE, "/api/doc/1", Some("bob"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "bob must NOT delete alice's row");
    // alice's row still readable by alice.
    let (s2, _) = req(Method::GET, "/api/doc/1", Some("alice"), None).await;
    assert_eq!(s2, StatusCode::OK);
}

#[tokio::test]
async fn anonymous_is_denied_everything() {
    let (list, body) = req(Method::GET, "/api/doc/", None, None).await;
    assert_eq!(list, StatusCode::OK);
    let results = body
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(results.is_empty(), "anonymous sees no rows: {body}");
    let (detail, _) = req(Method::GET, "/api/doc/1", None, None).await;
    assert_eq!(
        detail,
        StatusCode::NOT_FOUND,
        "anonymous can't read a row by id"
    );
}
