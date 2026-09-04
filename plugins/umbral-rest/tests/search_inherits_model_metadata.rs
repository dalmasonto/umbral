//! gaps4 #95 — REST `?search=` inherits the model's own declared
//! `#[umbral(search)]` fields, so a model that declares its searchable columns
//! powers `?search=` with NO per-`ResourceConfig` setup, and the search stays
//! restricted to those columns (not every text column).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::RestPlugin;

// `name` is the ONLY searchable column (declared on the model). `sku` and
// `note` carry the search term on OTHER rows but must NOT match, proving the
// search restricts to the model's declared field with no ResourceConfig.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "sim_widget")]
struct Widget {
    id: i64,
    #[umbral(search)]
    name: String,
    sku: String,
    note: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("sim.sqlite");
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

        // Plain default plugin — NO ResourceConfig::search_fields. The search
        // surface must come entirely from the model's `#[umbral(search)]`.
        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Widget>()
            .plugin(RestPlugin::default())
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();
        // Row 1: term in `name`. Row 2: term only in `sku`. Row 3: term only in `note`.
        sqlx::query(
            "INSERT INTO sim_widget (name, sku, note) VALUES \
             ('zephyr drive', 'AAA-1', 'nothing here'),\
             ('other item', 'zephyr-777', 'nothing'),\
             ('plain', 'BBB-2', 'a zephyr appears in the note')",
        )
        .execute(&pool)
        .await
        .expect("seed widgets");

        app.into_router()
    })
    .await
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
async fn search_matches_only_the_model_declared_search_field() {
    let router = boot().await.clone();

    let (status, body) = get(router, "/api/sim_widget/?search=zephyr").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let results = body["results"].as_array().expect("results array");
    // Only row 1 (term in `name`) matches; the `sku`/`note` rows must NOT,
    // because search is restricted to the model's declared `name` field.
    assert_eq!(
        results.len(),
        1,
        "search must restrict to the declared `name` field, not every column: {body}"
    );
    assert_eq!(results[0]["name"], "zephyr drive");
}
