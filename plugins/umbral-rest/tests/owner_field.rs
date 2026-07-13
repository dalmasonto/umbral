//! gaps3 #16 — `ResourceConfig::owner_field` injects the owner FK from the
//! authenticated identity on create, and REJECTS a body-supplied value, so a
//! client can't create a row owned by someone else.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
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
        let path = tmp.path().join("owner_field.sqlite");
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

        // The `x-user` header IS the identity (test stand-in for a real resolver).
        let auth = FnAuthentication::new(|headers: umbral::web::HeaderMap| async move {
            headers
                .get("x-user")
                .and_then(|v| v.to_str().ok())
                .map(Identity::user)
        });
        let resource = ResourceConfig::new("doc")
            .permission(AllowAny)
            .owner_field("owner_id");
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

        app.into_router()
    })
    .await
}

async fn post(user: Option<&str>, body: Value) -> (StatusCode, Value) {
    let router = boot().await.clone();
    let mut b = Request::builder().method(Method::POST).uri("/api/doc/");
    if let Some(u) = user {
        b = b.header("x-user", u);
    }
    let request = b
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn create_fills_owner_from_identity() {
    // alice creates a doc WITHOUT owner_id — it's filled from her identity.
    let (status, body) = post(Some("alice"), json!({ "title": "alice-doc" })).await;
    assert_eq!(status, StatusCode::CREATED, "got: {body}");
    assert_eq!(
        body["owner_id"], "alice",
        "owner injected from the token: {body}"
    );
}

#[tokio::test]
async fn create_rejects_a_body_supplied_owner() {
    // alice tries to create a doc owned by bob — rejected (can't forge ownership).
    let (status, _) = post(Some("alice"), json!({ "title": "x", "owner_id": "bob" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_requires_authentication() {
    // Anonymous create on an owner-field resource has no identity to inject.
    let (status, _) = post(None, json!({ "title": "x" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
