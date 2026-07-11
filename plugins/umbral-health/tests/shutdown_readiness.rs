//! Kikosi #5 — a draining process reports not-ready.
//!
//! `AppBuilder::shutdown_drain` marks the process draining on SIGTERM;
//! `/readyz` must then return 503 so a load balancer stops routing here before
//! the server stops accepting connections. This binary pins the health side of
//! that: with the draining flag set, readiness short-circuits to 503 — before
//! any DB probe, so no pool or `App::build()` is needed. The drain *sequencing*
//! (`drain_after`) is unit-tested in `umbral-core`'s `app.rs`.
//!
//! `begin_drain` sets a process-global flag that never resets, so this is the
//! only test in this binary — it must not leave a poisoned flag for a sibling.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbral::plugin::Plugin;
use umbral_health::HealthPlugin;

#[tokio::test]
async fn a_draining_process_is_not_ready() {
    let router = HealthPlugin::default().routes();

    // Before the signal, this would probe the DB; we only assert the drain path,
    // so go straight to draining.
    umbral::shutdown::begin_drain();
    assert!(umbral::shutdown::is_draining());

    let resp = router
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "a draining pod must fail readiness so the load balancer drains it",
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "draining");
    assert_eq!(json["checks"]["shutdown"]["status"], "draining");
    // Draining short-circuits before the DB probe: no `database` check ran.
    assert!(
        json["checks"].get("database").is_none(),
        "draining must short-circuit before the DB probe; body: {json}",
    );
}
