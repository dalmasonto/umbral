//! audit_2 H10 — a default umbral app ships minimal hardening response headers
//! from core (so it isn't clickjackable / MIME-sniffable even without
//! SecurityPlugin), set ONLY if absent so a handler / SecurityPlugin can
//! override without a duplicated header.
//!
//! Own test binary: `App::build()` sets the process-global settings `OnceLock`
//! exactly once.

use axum::body::Body;
use axum::http::{Request, header};
use axum::response::Response;
use tower::ServiceExt;
use umbral::routes::Routes;

async fn plain() -> &'static str {
    "ok"
}

/// A handler that sets its OWN X-Frame-Options — core must defer to it.
async fn custom_frame() -> Response {
    let mut r = Response::new(Body::from("ok"));
    r.headers_mut().insert(
        header::X_FRAME_OPTIONS,
        header::HeaderValue::from_static("SAMEORIGIN"),
    );
    r
}

async fn build() -> axum::Router {
    let settings = umbral::Settings::from_env().expect("settings");
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .routes(
            Routes::new()
                .get("/plain", plain)
                .get("/custom", custom_frame),
        )
        .build()
        .expect("build")
        .into_router()
}

#[tokio::test]
async fn default_headers_present_and_defer_to_handler() {
    let router = build().await;

    // Default route: core sets all three hardening headers.
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/plain")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let h = resp.headers();
    assert_eq!(h.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
    assert_eq!(h.get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
    assert_eq!(
        h.get(header::REFERRER_POLICY).unwrap(),
        "strict-origin-when-cross-origin"
    );

    // A handler that set its own X-Frame-Options wins (set-if-absent), but the
    // other defaults are still applied — and never duplicated.
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/custom")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let h = resp.headers();
    assert_eq!(h.get(header::X_FRAME_OPTIONS).unwrap(), "SAMEORIGIN");
    assert_eq!(
        h.get_all(header::X_FRAME_OPTIONS).iter().count(),
        1,
        "no duplicated header"
    );
    assert_eq!(h.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
}
