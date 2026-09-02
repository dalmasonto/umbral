//! End-to-end coverage for `ResourceConfig::action_by(...)` (gaps4 #80):
//! a by-natural-key detail action mounted at `/api/<table>/<value>/` with
//! no `/<name>/` suffix, keyed by a `#[umbral(unique)]` column instead of
//! the primary key.
//!
//! Three things this suite proves against a real booted app + a real
//! SQLite-backed row (not asserts against a mocked router):
//!
//! 1. `GET /api/community/<slug>/` resolves the row by `slug` and hands
//!    the handler the real row (`ctx.resolved_row`) + its PK (`ctx.pk`).
//! 2. When the URL segment does NOT match any `slug`, the route falls back
//!    to the ordinary PK-based `retrieve` — so a table declaring
//!    `.action_by` doesn't lose its normal `GET /api/<table>/<id>` — and
//!    `PUT`/`PATCH`/`DELETE` at that same URL keep working unmodified.
//! 2. Permission-gated coverage (401 for an unauthorized caller) lives in
//!    `action_by_gated.rs` (own binary, same reason `actions_gated.rs` is
//!    split from `actions.rs`: the resource's permission is plugin-wide).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{ActionContext, ActionError, AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Community {
    id: i64,
    #[umbral(unique)]
    slug: String,
    name: String,
}

async fn aggregate(ctx: ActionContext) -> Result<Value, ActionError> {
    let row = ctx
        .resolved_row
        .expect("action_by always resolves a row before calling the handler");
    Ok(json!({
        "found_by": ctx.name,
        "pk": ctx.pk,
        "community": row,
    }))
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("action_by.sqlite");
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

        let resource =
            ResourceConfig::new("community").action_by("slug", Method::GET, aggregate);
        let rest = RestPlugin::default()
            .default_permission(AllowAny)
            .resource(resource);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Community>()
            .plugin(rest)
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();
        sqlx::query(
            "INSERT INTO community (id, slug, name) VALUES (1, 'acme-corp', 'Acme Corp'), (2, '42', 'The Answer')",
        )
        .execute(&pool)
        .await
        .expect("seed community");

        app.into_router()
    })
    .await
}

async fn run(
    router: axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req_builder = Request::builder().method(method).uri(uri);
    let req = match body {
        Some(b) => req_builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap(),
        None => req_builder.body(Body::empty()).unwrap(),
    };
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `GET /api/community/<slug>/` resolves the row by `slug`, not by PK, and
/// the handler sees the real resolved row plus its PK.
#[tokio::test]
async fn by_field_lookup_serves_the_resolved_row() {
    let router = boot().await.clone();

    let (status, body) = run(router, Method::GET, "/api/community/acme-corp/", None).await;
    assert_eq!(status, StatusCode::OK, "body was {body}");
    assert_eq!(body["found_by"], json!("slug"));
    assert_eq!(body["pk"], json!("1"));
    assert_eq!(body["community"]["name"], json!("Acme Corp"));
    assert_eq!(body["community"]["slug"], json!("acme-corp"));
}

/// Trailing slash is optional, same as `.action()`.
#[tokio::test]
async fn by_field_lookup_accepts_no_trailing_slash() {
    let router = boot().await.clone();

    let (status, body) = run(router, Method::GET, "/api/community/acme-corp", None).await;
    assert_eq!(status, StatusCode::OK, "body was {body}");
    assert_eq!(body["found_by"], json!("slug"));
}

/// A value that matches NO row's `slug` falls back to the ordinary
/// PK-based retrieve — proving `.action_by` doesn't break the resource's
/// normal `GET /api/<table>/<id>` at the same URL. Row 1's PK is `1`,
/// which is not equal to row 2's slug `"42"` or row 1's slug
/// `"acme-corp"`, so `GET /api/community/1/` can only succeed via the PK
/// fallback path.
#[tokio::test]
async fn value_not_matching_lookup_column_falls_back_to_pk_retrieve() {
    let router = boot().await.clone();

    let (status, body) = run(router, Method::GET, "/api/community/1/", None).await;
    assert_eq!(status, StatusCode::OK, "body was {body}");
    // Went through the PLAIN retrieve handler, not the action handler: no
    // `found_by` wrapper, just the row's own columns at the top level.
    assert!(
        body.get("found_by").is_none(),
        "expected the PK fallback (plain retrieve), got the action wrapper: {body}"
    );
    assert_eq!(body["id"], json!(1));
    assert_eq!(body["slug"], json!("acme-corp"));
}

/// A slug value that happens to look numeric still resolves via the
/// lookup column FIRST (documented trade-off: the by-field lookup always
/// wins over a coincidental PK match).
#[tokio::test]
async fn numeric_looking_slug_resolves_via_lookup_field_first() {
    let router = boot().await.clone();

    let (status, body) = run(router, Method::GET, "/api/community/42/", None).await;
    assert_eq!(status, StatusCode::OK, "body was {body}");
    assert_eq!(body["found_by"], json!("slug"));
    assert_eq!(body["community"]["name"], json!("The Answer"));
}

/// Neither the lookup column nor the PK matches: 404, same envelope shape
/// as the ordinary retrieve 404.
#[tokio::test]
async fn no_match_on_either_lookup_or_pk_is_404() {
    let router = boot().await.clone();

    let (status, _body) = run(router, Method::GET, "/api/community/nope/", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// `PUT`/`PATCH`/`DELETE` at the same literal `/api/<table>/<id>/` URL are
/// untouched by `.action_by` — they still resolve by PK through the
/// ordinary `update`/`destroy` handlers.
#[tokio::test]
async fn write_verbs_at_the_same_url_still_use_the_standard_pk_handlers() {
    let router = boot().await.clone();

    let (status, body) = run(
        router.clone(),
        Method::PATCH,
        "/api/community/1/",
        Some(json!({ "name": "Acme Corp Renamed" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body was {body}");
    assert_eq!(body["name"], json!("Acme Corp Renamed"));

    // Read it back through the lookup-field path to confirm the write landed.
    let (status, body) = run(router, Method::GET, "/api/community/acme-corp/", None).await;
    assert_eq!(status, StatusCode::OK, "body was {body}");
    assert_eq!(body["community"]["name"], json!("Acme Corp Renamed"));
}
