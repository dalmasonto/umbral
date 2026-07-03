//! audit_2 core-web H11 — the framework-wide request hardening layers wired by
//! `App::build`: a request-body size cap (`RequestBodyLimitLayer` → 413) and a
//! per-request timeout (`TimeoutLayer` → 408).
//!
//! One `App::build` per binary (settings init is one-shot via a `OnceLock`), so
//! a single app is configured with BOTH a tiny body cap and a short timeout and
//! every assertion runs against it.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use std::time::Duration;
use tower::ServiceExt;
use umbral::routes::Routes;

async fn build() -> axum::Router {
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    // Consumes the body so the `RequestBodyLimitLayer` streaming guard fires
    // (an oversized body errors mid-read → 413) even when the synthetic test
    // request carries no `Content-Length` header for the up-front check.
    async fn echo(_body: axum::body::Bytes) -> &'static str {
        "ok"
    }
    async fn slow() -> &'static str {
        // Far longer than the configured 100ms timeout, so the TimeoutLayer
        // always wins the race — no flakiness from a tight margin.
        tokio::time::sleep(Duration::from_secs(3)).await;
        "eventually"
    }

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        // 1 KiB body ceiling, 100ms request timeout — both well inside the
        // test's control so the layers fire deterministically.
        .max_request_body(Some(1024))
        .request_timeout(Some(Duration::from_millis(100)))
        .routes(Routes::new().post("/echo", echo).get("/slow", slow))
        .build()
        .expect("App::build")
        .into_router()
}

#[tokio::test]
async fn body_limit_and_timeout_are_enforced_by_default() {
    let router = build().await;

    // --- Body under the cap → handler runs (200). ---
    let small = Request::builder()
        .method(Method::POST)
        .uri("/echo")
        .body(Body::from(vec![b'x'; 512]))
        .unwrap();
    let resp = router.clone().oneshot(small).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a body under the cap must pass through"
    );

    // --- Body over the cap → 413 before the handler buffers it. ---
    let big = Request::builder()
        .method(Method::POST)
        .uri("/echo")
        .body(Body::from(vec![b'x'; 4096]))
        .unwrap();
    let resp = router.clone().oneshot(big).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "a body over the cap must be rejected with 413"
    );

    // --- Handler slower than the timeout → 408. ---
    let slow = Request::builder()
        .method(Method::GET)
        .uri("/slow")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(slow).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::REQUEST_TIMEOUT,
        "a handler exceeding the timeout must be aborted with 408"
    );
}
