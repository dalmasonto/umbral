//! `AppBuilder::compression()` wraps the router in a gzip/brotli layer
//! (feature #66). One `App::build` (settings init is one-shot): assert the
//! layer compresses when the client accepts gzip and passes through when
//! it doesn't.

use axum::body::Body;
use http::Request;
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbral::plugin::{AppContext, Plugin, PluginError};
use umbral::web::{Router, get};

/// Contributes a route returning a large, highly-compressible body.
struct BigPlugin;

impl Plugin for BigPlugin {
    fn name(&self) -> &'static str {
        "big"
    }
    fn routes(&self) -> Router {
        Router::new().route("/big", get(|| async { "x".repeat(4096) }))
    }
    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

async fn build() -> axum::Router {
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(BigPlugin)
        .compression()
        .build()
        .expect("App::build")
        .into_router()
}

async fn get_big(router: &axum::Router, accept_encoding: Option<&str>) -> http::Response<Body> {
    let mut req = Request::builder().uri("/big");
    if let Some(ae) = accept_encoding {
        req = req.header("accept-encoding", ae);
    }
    router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .expect("oneshot")
}

#[tokio::test]
async fn compression_layer_respects_accept_encoding() {
    let router = build().await;

    // 1. Client accepts gzip → the body comes back gzip-encoded + smaller.
    let resp = get_big(&router, Some("gzip")).await;
    assert_eq!(
        resp.headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "Content-Encoding: gzip when the client accepts it"
    );
    let compressed = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(
        compressed.len() < 4096,
        "the 4KB body was compressed ({} bytes)",
        compressed.len()
    );

    // 2. No Accept-Encoding → the layer passes through, full plaintext body.
    let resp = get_big(&router, None).await;
    assert!(
        resp.headers().get("content-encoding").is_none(),
        "no Content-Encoding when the client doesn't accept any"
    );
    let raw = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(raw.len(), 4096, "the uncompressed body is the full 4KB");
}
