//! The REST `?search=` endpoint runs full-text search on a `tsvector`
//! column (the feature wired in filtering.rs). Restricting `search_fields`
//! to the tsvector and using a STEMMED query (`products` -> `product`)
//! proves the match comes from FTS, not the substring `LIKE` path — a
//! substring `%products%` would not match "The best product ever".
//!
//! Self-skips unless UMBRAL_TEST_POSTGRES_URL points at a Postgres server.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use umbral_rest::{ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rfts_product")]
struct RftsProduct {
    id: i64,
    name: String,
    #[serde(skip)]
    search: umbral::orm::TsVector,
}

async fn get(router: axum::Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed)
}

#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL"]
async fn rest_search_uses_full_text_on_tsvector() {
    let Ok(url) = std::env::var("UMBRAL_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRAL_TEST_POSTGRES_URL not set");
        return;
    };
    let pool = PgPool::connect(&url).await.expect("connect");

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    // `?search=` restricted to the tsvector column → only the FTS arm runs.
    let rest = RestPlugin::default()
        .resource(ResourceConfig::new("rfts_product").search_fields(["search"]));
    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<RftsProduct>()
        .plugin(rest)
        .build()
        .expect("App::build");
    let router = app.into_router();

    for ddl in [
        "DROP TABLE IF EXISTS rfts_product",
        "CREATE TABLE rfts_product (
            id BIGSERIAL PRIMARY KEY,
            name TEXT NOT NULL,
            search TSVECTOR GENERATED ALWAYS AS (to_tsvector('english', name)) STORED
        )",
        "INSERT INTO rfts_product (name) VALUES ('The best product ever'), ('A great widget')",
    ] {
        sqlx::query(ddl).execute(&pool).await.expect("setup");
    }

    let count = |body: &Value| body["results"].as_array().map(|a| a.len()).unwrap_or(0);

    // STEMMED match: "products" -> lexeme "product". FTS hits the
    // best-product row; a substring `%products%` would NOT (the name has
    // "product", not "products"), so this can only come from FTS.
    let (s, body) = get(router.clone(), "/api/rfts_product/?search=products").await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(count(&body), 1, "FTS stemmed match: {body}");

    // Multi-word: spaces mean AND → both lexemes present.
    let (s, body) = get(router.clone(), "/api/rfts_product/?search=best%20product").await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(count(&body), 1, "FTS AND match: {body}");

    // "tseb" (reverse) is no lexeme → no match (confirms it isn't substring).
    let (s, body) = get(router.clone(), "/api/rfts_product/?search=tseb").await;
    assert_eq!(s, StatusCode::OK, "body: {body}");
    assert_eq!(count(&body), 0, "reverse string must not match: {body}");

    sqlx::query("DROP TABLE rfts_product")
        .execute(&pool)
        .await
        .expect("cleanup");
}
