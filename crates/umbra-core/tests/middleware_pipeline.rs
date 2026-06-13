//! Feature #68 — the framework `Middleware` pipeline. One `App::build`
//! (settings init is one-shot) wires an app-level tagger, a gate that can
//! short-circuit, and a plugin-contributed tagger, then proves:
//!   * `before_request` runs in registration order (app middleware before
//!     plugin middleware),
//!   * `after_response` runs in reverse (onion unwind),
//!   * a `before_request` returning `Err(response)` short-circuits the
//!     handler and later middleware, and only the middleware that already
//!     ran get an `after_response`.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbra::async_trait;
use umbra::middleware::Middleware;
use umbra::plugin::Plugin;
use umbra::web::{Router, get};

/// Appends its `tag` to the `x-before` request header (read by the
/// handler) and the `x-after` response header. The header chain records
/// execution order.
struct Tagger {
    tag: &'static str,
}

fn append(value: Option<&str>, tag: &str) -> String {
    match value {
        Some(v) if !v.is_empty() => format!("{v},{tag}"),
        _ => tag.to_string(),
    }
}

#[async_trait]
impl Middleware for Tagger {
    async fn before_request(&self, mut req: Request) -> Result<Request, Response> {
        let prev = req.headers().get("x-before").and_then(|v| v.to_str().ok());
        let next = append(prev, self.tag);
        req.headers_mut().insert("x-before", next.parse().unwrap());
        Ok(req)
    }

    async fn after_response(&self, mut res: Response) -> Response {
        let prev = res.headers().get("x-after").and_then(|v| v.to_str().ok());
        let next = append(prev, self.tag);
        res.headers_mut().insert("x-after", next.parse().unwrap());
        res
    }
}

/// Short-circuits any request to `/blocked` with a 403; passes everything
/// else through untouched.
struct Gate;

#[async_trait]
impl Middleware for Gate {
    async fn before_request(&self, req: Request) -> Result<Request, Response> {
        if req.uri().path() == "/blocked" {
            return Err((StatusCode::FORBIDDEN, "blocked by gate").into_response());
        }
        Ok(req)
    }
}

/// Contributes the routes plus one plugin-level middleware (tagger "B").
struct AppPlugin;

impl Plugin for AppPlugin {
    fn name(&self) -> &'static str {
        "appplug"
    }
    fn routes(&self) -> Router {
        // Handler echoes the accumulated `x-before` chain in the body.
        async fn echo(req: Request) -> Response {
            let chain = req
                .headers()
                .get("x-before")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("none")
                .to_string();
            (StatusCode::OK, chain).into_response()
        }
        Router::new()
            .route("/ok", get(echo))
            .route("/blocked", get(echo))
    }
    fn middleware(&self) -> Vec<Arc<dyn Middleware>> {
        vec![Arc::new(Tagger { tag: "B" })]
    }
}

async fn build() -> axum::Router {
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("sqlite");
    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    // Stack order: app middleware first [A, Gate], then plugin's [B].
    umbra::App::builder()
        .settings(settings)
        .database("default", pool)
        .middleware(Tagger { tag: "A" })
        .middleware(Gate)
        .plugin(AppPlugin)
        .build()
        .expect("App::build")
        .into_router()
}

async fn call(router: &axum::Router, path: &str) -> Response {
    router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .expect("oneshot")
}

fn after_header(resp: &Response) -> Option<String> {
    resp.headers()
        .get("x-after")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

// One `App::build` (settings init is one-shot), both paths exercised.
#[tokio::test]
async fn pipeline_orders_hooks_and_supports_short_circuit() {
    let router = build().await;

    // --- /ok: full pass-through. ---
    let resp = call(&router, "/ok").await;
    assert_eq!(resp.status(), StatusCode::OK);
    // after_response unwinds onion-style: B then A (reverse of before).
    assert_eq!(
        after_header(&resp).as_deref(),
        Some("B,A"),
        "after hooks unwind onion-style"
    );
    // before_request order, as seen by the handler: A then B (Gate leaves
    // the header alone).
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"A,B", "before hooks ran in registration order");

    // --- /blocked: Gate (index 1) short-circuits. ---
    let resp = call(&router, "/blocked").await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "gate rejected before the handler"
    );
    // Only the middleware that already ran (A, index 0) gets an
    // after_response; B never ran its before, so it's absent from x-after.
    assert_eq!(
        after_header(&resp).as_deref(),
        Some("A"),
        "only already-run middleware unwind on short-circuit"
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"blocked by gate");
}
