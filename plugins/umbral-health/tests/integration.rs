//! Feature #47 — health + readiness probe integration tests.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbral::plugin::Plugin;
use umbral_health::{HealthCheck, HealthError, HealthPlugin};

async fn boot() {
    use tokio::sync::OnceCell;
    static BOOT: OnceCell<()> = OnceCell::const_new();
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults always load");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite always connects");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(HealthPlugin::default())
            .build()
            .expect("App::build should succeed");
    })
    .await;
}

#[tokio::test]
async fn liveness_always_returns_200() {
    boot().await;
    let router = HealthPlugin::default().routes();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn readiness_returns_200_when_db_is_up_and_no_checks_registered() {
    boot().await;
    let router = HealthPlugin::default().routes();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["checks"]["database"]["status"], "ok");
}

struct AlwaysOk;
#[async_trait::async_trait]
impl HealthCheck for AlwaysOk {
    fn name(&self) -> &'static str {
        "redis"
    }
    async fn check(&self) -> Result<(), HealthError> {
        Ok(())
    }
}

struct AlwaysFail;
#[async_trait::async_trait]
impl HealthCheck for AlwaysFail {
    fn name(&self) -> &'static str {
        "stripe"
    }
    async fn check(&self) -> Result<(), HealthError> {
        Err(HealthError::new("timeout"))
    }
}

#[tokio::test]
async fn readiness_surfaces_registered_check_results() {
    boot().await;
    let router = HealthPlugin::default()
        .check(AlwaysOk)
        .check(AlwaysFail)
        .routes();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "any failing check forces a 503",
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "fail");
    assert_eq!(json["checks"]["redis"]["status"], "ok");
    assert_eq!(json["checks"]["stripe"]["status"], "fail");
    assert_eq!(json["checks"]["stripe"]["reason"], "timeout");
    assert_eq!(json["checks"]["database"]["status"], "ok");
}

#[tokio::test]
async fn route_paths_announces_both_endpoints() {
    let paths = HealthPlugin::default().route_paths();
    assert_eq!(paths.len(), 2);
    assert!(paths.iter().any(|p| p.path == "/healthz"));
    assert!(paths.iter().any(|p| p.path == "/ready"));
}
