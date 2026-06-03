//! Proof that per-route `.layer(...)` survives the trip through the
//! `Routes` builder.
//!
//! The classic axum gotcha is `Router::new().route(...).layer(L)` —
//! the layer applies to *every* route on that Router instance, not
//! just the most recent one. `MethodRouter::layer(L)` is the
//! route-scoped form. `Routes::layered(method, path, mr)` (and the
//! equivalent `Routes::route(&[method], path, mr)`) accept a
//! `MethodRouter<()>` so chained layers attach to that one path —
//! these tests assert the contract holds end-to-end.

use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn;
use axum::routing::get;
use tower::ServiceExt;
use umbra_core::routes::Routes;

// Per-test statics so the parallel runner doesn't race counters
// across tests. Each test owns its own pair.
mod layered_path {
    use super::AtomicUsize;
    pub static LAYER_HITS: AtomicUsize = AtomicUsize::new(0);
    pub static OPEN_HITS: AtomicUsize = AtomicUsize::new(0);
    pub static PROTECTED_HITS: AtomicUsize = AtomicUsize::new(0);
}

mod route_form {
    use super::AtomicUsize;
    pub static LAYER_HITS: AtomicUsize = AtomicUsize::new(0);
}

#[tokio::test]
async fn per_route_layer_fires_only_for_the_layered_path() {
    use layered_path::*;

    async fn layer_middleware(
        req: Request<Body>,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        LAYER_HITS.fetch_add(1, Ordering::SeqCst);
        next.run(req).await
    }
    async fn protected() -> &'static str {
        PROTECTED_HITS.fetch_add(1, Ordering::SeqCst);
        "protected"
    }
    async fn open() -> &'static str {
        OPEN_HITS.fetch_add(1, Ordering::SeqCst);
        "open"
    }

    let (router, specs) = Routes::new()
        .get("/open", open)
        .layered(
            "GET",
            "/protected",
            get(protected).layer(from_fn(layer_middleware)),
        )
        .into_parts();

    // Hit the open route — handler fires, layer must NOT.
    let res = router
        .clone()
        .oneshot(Request::builder().uri("/open").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(OPEN_HITS.load(Ordering::SeqCst), 1);
    assert_eq!(
        LAYER_HITS.load(Ordering::SeqCst),
        0,
        "layer must scope to /protected only — it leaked into /open"
    );

    // Hit the protected route — handler AND layer fire.
    let res = router
        .oneshot(
            Request::builder()
                .uri("/protected")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(PROTECTED_HITS.load(Ordering::SeqCst), 1);
    assert_eq!(
        LAYER_HITS.load(Ordering::SeqCst),
        1,
        "layer should have fired for the protected route"
    );

    // Both routes appear in the registry with their methods.
    assert_eq!(specs.len(), 2);
    let by_path: std::collections::HashMap<&str, &Vec<&str>> = specs
        .iter()
        .map(|s| (s.path.as_str(), &s.methods))
        .collect();
    assert_eq!(by_path["/open"], &vec!["GET"]);
    assert_eq!(by_path["/protected"], &vec!["GET"]);
}

#[tokio::test]
async fn route_form_also_supports_layered_method_router() {
    use route_form::*;

    async fn layer_middleware(
        req: Request<Body>,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        LAYER_HITS.fetch_add(1, Ordering::SeqCst);
        next.run(req).await
    }
    async fn h() -> &'static str {
        "ok"
    }

    let (router, _) = Routes::new()
        .route(
            &["POST"],
            "/upload",
            axum::routing::post(h).layer(from_fn(layer_middleware)),
        )
        .into_parts();

    let res = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/upload")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        LAYER_HITS.load(Ordering::SeqCst),
        1,
        "explicit .route(...) form must preserve the per-route layer",
    );
}
