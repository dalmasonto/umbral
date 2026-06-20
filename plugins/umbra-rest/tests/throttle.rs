//! Integration: a resource with a tiny `AnonRateThrottle` returns 429 +
//! `Retry-After` once an anonymous IP is over its rate, and stays 200
//! while under the limit. Mirrors the `default_safe_permission` boot
//! shape (in-memory SQLite, real router, `oneshot` requests).
//!
//! Also exercises the throttle classes' unit-level keying through the
//! live dispatch: a different IP gets its own bucket.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbra_rest::{AnonRateThrottle, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
struct Gadget {
    id: i64,
    name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

/// Boot a RestPlugin whose every resource carries a 2/min anonymous
/// throttle. `default_permission(AllowAny)` so the permission gate never
/// fires — we want to isolate the THROTTLE behaviour, not the permission.
async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("rest_throttle.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let app = umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Gadget>()
            .plugin(
                RestPlugin::default()
                    .default_permission(umbra_rest::AllowAny)
                    .default_throttle(AnonRateThrottle::new("2/min")),
            )
            .build()
            .expect("App::build with RestPlugin");

        let pool = umbra::db::pool();
        let meta = umbra::migrate::ModelMeta::for_::<Gadget>();
        let op = umbra::migrate::Operation::CreateTable {
            table: "gadget".to_string(),
            columns: meta.fields.clone(),
            unique_together: Vec::new(),
            indexes: Vec::new(),
        };
        for stmt in umbra::migrate::render_operation_for(&op, "sqlite") {
            sqlx::query(&stmt)
                .execute(&pool)
                .await
                .expect("create gadget");
        }
        sqlx::query("INSERT INTO gadget (name) VALUES ('seed')")
            .execute(&pool)
            .await
            .expect("seed gadget");

        app.into_router()
    })
    .await
}

/// Anonymous GET /api/gadget/ from the given IP (via X-Forwarded-For).
fn list_from(ip: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/api/gadget/")
        .header("x-forwarded-for", ip)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn throttle_returns_429_with_retry_after_after_limit() {
    let router = boot().await;
    let ip = "203.0.113.10";

    // First two requests from this IP are under the 2/min limit.
    for n in 1..=2 {
        let resp = router.clone().oneshot(list_from(ip)).await.expect("oneshot");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "request {n} from {ip} should be under the 2/min limit"
        );
    }

    // Third request is over the limit → 429 with a Retry-After header and
    // the DRF body shape.
    let resp = router.clone().oneshot(list_from(ip)).await.expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "3rd request from {ip} must be throttled (429)"
    );
    let retry = resp
        .headers()
        .get(header::RETRY_AFTER)
        .expect("Retry-After header present on a 429")
        .to_str()
        .expect("Retry-After is valid ASCII");
    let secs: u64 = retry.parse().expect("Retry-After is whole seconds");
    assert!(secs > 0, "Retry-After should be a positive second count");

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    assert_eq!(body["detail"], "Request was throttled.");
    assert!(
        body["retry_after"].as_u64().is_some(),
        "body carries a numeric retry_after"
    );
}

#[tokio::test]
async fn a_different_ip_has_its_own_bucket() {
    let router = boot().await;
    // Exhaust one IP entirely.
    let busy = "198.51.100.5";
    for _ in 0..3 {
        let _ = router.clone().oneshot(list_from(busy)).await.expect("oneshot");
    }
    let resp = router.clone().oneshot(list_from(busy)).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

    // A fresh IP is unaffected — its own 2/min bucket.
    let fresh = "198.51.100.99";
    let resp = router.clone().oneshot(list_from(fresh)).await.expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a different IP must not inherit the busy IP's throttle state"
    );
}
