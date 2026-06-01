//! Compiled admin.css — embedded at compile time, served in prod, the
//! Tailwind CDN replaces it in dev (see `wrapper.html`).
//!
//! ⚠ This handler is the simplest possible thing that works for one
//! file. A multi-asset static-handler refactor is tracked separately
//! (umbra-core gains a `static_dir` Plugin hook; this whole module
//! collapses into one builder call).

use umbra::web::{IntoResponse, StatusCode};

static ADMIN_CSS: &str = include_str!("assets/admin.css");

/// `GET /admin/static/admin.css` — return the embedded production
/// stylesheet with a one-day cache header. Build it with:
///
/// ```sh
/// cd plugins/umbra-admin/css && npm install && npm run build
/// ```
pub(crate) async fn serve_admin_css() -> impl IntoResponse {
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/css; charset=utf-8")
        .header("Cache-Control", "public, max-age=86400")
        .body(axum::body::Body::from(ADMIN_CSS))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
