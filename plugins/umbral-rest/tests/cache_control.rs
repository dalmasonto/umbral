//! REST responses carry an explicit `Cache-Control` (gaps3 #36).
//!
//! A `200 application/json` with NO cache directive is *heuristically cacheable*
//! by browsers and shared proxies (RFC 9111 §4.2.2). On a mutable API that is a
//! data-loss bug rather than a perf nit: a refetch immediately after a write can
//! be served the pre-write snapshot out of cache and silently clobber fresh
//! state. So the framework ships `no-store` by default rather than leaving the
//! decision to a heuristic.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral_rest::{ResourceConfig, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "widget")]
pub struct Widget {
    pub id: i64,
    pub name: String,
}

/// A genuinely cacheable, slow-changing public read endpoint.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "country")]
pub struct Country {
    pub id: i64,
    pub name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .expect("pool");
        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Widget>()
            .model::<Country>()
            .plugin(
                RestPlugin::default()
                    .resource(ResourceConfig::new("country").cache_control("public, max-age=3600")),
            )
            .build()
            .expect("App::build");
        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        app.into_router()
    })
    .await
}

async fn cache_header(path: &str) -> (StatusCode, Option<String>) {
    let app = boot().await.clone();
    let res = app
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .expect("request");
    let cc = res
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (res.status(), cc)
}

/// The default: a mutable API is never left heuristically cacheable.
#[tokio::test]
async fn rest_responses_default_to_no_store() {
    let (status, cc) = cache_header("/api/widget/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        cc.as_deref(),
        Some("no-store"),
        "a JSON list with no directive is heuristically cacheable — a refetch \
         after a write could be served the stale pre-write snapshot",
    );
}

/// A genuinely cacheable read endpoint can opt back in, per resource.
#[tokio::test]
async fn a_resource_can_opt_into_caching() {
    let (_, cc) = cache_header("/api/country/").await;
    assert_eq!(cc.as_deref(), Some("public, max-age=3600"));
}
