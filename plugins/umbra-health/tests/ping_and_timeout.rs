//! Tests for umbra::db::ping() and the per-check timeout in the readiness runner.

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbra::plugin::Plugin;
use umbra_health::{HealthCheck, HealthError, HealthPlugin};

/// Boot a test app with an in-memory SQLite pool. A `OnceCell`
/// guards against re-initialising the ambient pool (which panics
/// when called a second time in the same process).
async fn boot() {
    use tokio::sync::OnceCell;
    static BOOT: OnceCell<()> = OnceCell::const_new();
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults always load");
        let pool = umbra::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite always connects");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(HealthPlugin::default())
            .build()
            .expect("App::build should succeed");
    })
    .await;
}

// ---------------------------------------------------------------------------
// Issue A: umbra::db::ping() via the ORM surface
// ---------------------------------------------------------------------------

/// `umbra::db::ping()` returns `Ok(())` against the live SQLite pool
/// that `boot()` installed via `App::build()`.
#[tokio::test]
async fn ping_returns_ok_against_live_sqlite_pool() {
    boot().await;
    umbra::db::ping()
        .await
        .expect("ping should succeed against the in-memory sqlite pool");
}

/// The DB check in the readiness body reports `ok` when the pool is healthy.
#[tokio::test]
async fn readiness_database_check_reports_ok_when_pool_is_healthy() {
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
    assert_eq!(json["checks"]["database"]["status"], "ok");
}

// ---------------------------------------------------------------------------
// Issue B: per-check timeout — a slow check must NOT hang the runner
// ---------------------------------------------------------------------------

/// A check that sleeps longer than the configured timeout must be recorded
/// as `fail` / `"timed out"` and the readiness runner must return promptly
/// (well within a 10 s wall-clock budget despite the check sleeping 30 s).
struct SlowCheck {
    sleep: Duration,
}

#[async_trait::async_trait]
impl HealthCheck for SlowCheck {
    fn name(&self) -> &'static str {
        "slow"
    }
    async fn check(&self) -> Result<(), HealthError> {
        tokio::time::sleep(self.sleep).await;
        Ok(())
    }
}

#[tokio::test]
async fn slow_check_is_recorded_as_timed_out_and_runner_returns_promptly() {
    boot().await;

    // The check will sleep for 30 s; we give the runner a 200 ms timeout so
    // the test completes quickly.
    let router = HealthPlugin::default()
        .check(SlowCheck {
            sleep: Duration::from_secs(30),
        })
        .check_timeout(Duration::from_millis(200))
        .routes();

    let start = Instant::now();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let elapsed = start.elapsed();

    // The runner must complete well before the 30-s sleep would have finished.
    // We allow 5 s of headroom for slow CI boxes.
    assert!(
        elapsed < Duration::from_secs(5),
        "readiness runner took {elapsed:?}; should have returned promptly after timeout",
    );

    // The overall readiness status must be `fail`.
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "fail");
    assert_eq!(json["checks"]["slow"]["status"], "fail");
    assert_eq!(json["checks"]["slow"]["reason"], "timed out");
}

/// A check that completes within the timeout must still be recorded as `ok`.
struct FastCheck;

#[async_trait::async_trait]
impl HealthCheck for FastCheck {
    fn name(&self) -> &'static str {
        "fast"
    }
    async fn check(&self) -> Result<(), HealthError> {
        Ok(())
    }
}

#[tokio::test]
async fn fast_check_is_recorded_as_ok_within_timeout() {
    boot().await;

    let router = HealthPlugin::default()
        .check(FastCheck)
        .check_timeout(Duration::from_secs(5))
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

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["checks"]["fast"]["status"], "ok");
}
