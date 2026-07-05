//! Bulk endpoints preserve EVERY existing security guarantee (gaps2 #82):
//!   - per-action permission classes (a ReadOnly resource → 403 on a bulk
//!     write),
//!   - the blocked-table set (a default-blocked table like `auth_user` has
//!     NO bulk surface — bulk create/update/delete all 404), and
//!   - the `password_hash` / hidden-field denylist (stripped from a bulk
//!     create response, never written from a bulk body).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;

use tokio::sync::OnceCell;
use umbral_rest::{ReadOnly, ResourceConfig, RestPlugin};

// A normally-blocked table — opting into `.expose()` lets us serve it, but
// the bulk write must still inherit the per-table permission.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "auth_user")]
struct AuthUserStub {
    id: i64,
    username: String,
    password_hash: String,
}

// A plain table we serve ReadOnly + bulk-enabled: reads work, bulk writes
// are 403 (ReadOnly denies create/update/delete).
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Article {
    id: i64,
    title: String,
}

// A bulk-enabled table where we EXPOSE the blocked `auth_user` peer and
// hide its hash — bulk create must strip password_hash from the response.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "account")]
struct Account {
    id: i64,
    email: String,
    // Nullable so the denylist can strip the client-supplied value on
    // write without tripping a NOT NULL constraint — the point is that the
    // secret never lands, not that the column is required.
    password_hash: Option<String>,
}

// One App::build per binary; share the router across tests.
static ROUTER: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> axum::Router {
    ROUTER.get_or_init(build).await.clone()
}

async fn build() -> axum::Router {
    let settings = umbral::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("bulk_security.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    let rest = RestPlugin::default()
        // `article` is ReadOnly + bulk: bulk writes must 403.
        .resource(
            ResourceConfig::for_::<Article>()
                .permission(ReadOnly)
                .bulk(),
        )
        // `account` is exposed for writes (AllowAny via default? no — give
        // it AllowAny explicitly) + bulk + hide the hash.
        .resource(
            ResourceConfig::for_::<Account>()
                .permission(umbral_rest::AllowAny)
                .hide("password_hash")
                .bulk(),
        )
        // `auth_user` is bulk-enabled in config but NOT exposed → stays
        // blocked, so it has no REST surface at all.
        .resource(ResourceConfig::for_::<AuthUserStub>().bulk());

    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Article>()
        .model::<Account>()
        .model::<AuthUserStub>()
        .plugin(rest)
        .build()
        .expect("App::build");

    let pool = umbral::db::pool();
    sqlx::query("CREATE TABLE article (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create article");
    sqlx::query(
        "CREATE TABLE account (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL, password_hash TEXT)",
    )
    .execute(&pool)
    .await
    .expect("create account");
    sqlx::query(
        "CREATE TABLE auth_user (id INTEGER PRIMARY KEY AUTOINCREMENT, username TEXT NOT NULL, password_hash TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .expect("create auth_user");

    app.into_router()
}

async fn send(
    router: &axum::Router,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn readonly_resource_rejects_bulk_writes() {
    let router = boot().await;

    let (status, _b) = send(
        &router,
        Method::POST,
        "/api/article/",
        json!([{ "title": "x" }, { "title": "y" }]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "bulk create denied by ReadOnly"
    );

    let (status, _b) = send(
        &router,
        Method::PATCH,
        "/api/article/",
        json!([{ "id": 1, "title": "z" }]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "bulk update denied by ReadOnly"
    );

    let (status, _b) = send(
        &router,
        Method::DELETE,
        "/api/article/",
        json!({ "ids": [1] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "bulk delete denied by ReadOnly"
    );

    assert_eq!(
        Article::objects().count().await.unwrap(),
        0,
        "no writes leaked through"
    );
}

#[tokio::test]
async fn blocked_table_has_no_bulk_surface() {
    let router = boot().await;

    // auth_user is default-blocked and was NOT `.expose()`d → 404 on every
    // bulk verb, same as it has no single-object surface.
    let (status, _b) = send(
        &router,
        Method::POST,
        "/api/auth_user/",
        json!([{ "username": "x", "password_hash": "h" }]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no bulk create on a blocked table"
    );

    let (status, _b) = send(
        &router,
        Method::PATCH,
        "/api/auth_user/",
        json!([{ "id": 1, "username": "y" }]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no bulk update on a blocked table"
    );

    let (status, _b) = send(
        &router,
        Method::DELETE,
        "/api/auth_user/",
        json!({ "ids": [1] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no bulk delete on a blocked table"
    );
}

#[tokio::test]
async fn password_hash_stripped_and_not_writable_in_bulk() {
    let router = boot().await;

    // Bulk create two accounts, each sending a password_hash in the body.
    // The hidden-field denylist strips it on write, and it never appears
    // in the response.
    let (status, body) = send(
        &router,
        Method::POST,
        "/api/account/",
        json!([
            { "email": "a@x.com", "password_hash": "SECRET-A" },
            { "email": "b@x.com", "password_hash": "SECRET-B" }
        ]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "bulk create on exposed account; {body}"
    );
    let arr = body.as_array().expect("array");
    for row in arr {
        assert!(
            row.get("password_hash").is_none(),
            "password_hash never appears in a bulk response"
        );
    }

    // The body's password_hash was stripped before write: the stored row
    // has the empty-default, NOT the client-supplied secret.
    let stored: Vec<Option<String>> =
        sqlx::query_scalar("SELECT password_hash FROM account ORDER BY id")
            .fetch_all(&umbral::db::pool())
            .await
            .unwrap();
    assert!(
        stored
            .iter()
            .all(|h| h.as_deref() != Some("SECRET-A") && h.as_deref() != Some("SECRET-B")),
        "the client-supplied password_hash was NOT written (denylist held); got {stored:?}"
    );
}
