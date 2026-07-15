//! gaps4 #31 — a plugin's `routes_builder()` makes route metadata drift-free:
//! the mounted axum router AND the registry's declared specs come from ONE
//! source, so an audit/discovery surface can't report a path that isn't served
//! (or miss one that is).
//!
//! This drives the real thing end-to-end: build an App with such a plugin, hit
//! each declared route (proving it's actually mounted), then read the published
//! route registry and assert it lists EXACTLY those paths — no more, no fewer.
//! Own binary because `App::build` publishes the registry into a process-wide
//! `OnceLock`.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbral::App;
use umbral::plugin::Plugin;
use umbral::routes::Routes;

async fn health() -> &'static str {
    "ok"
}
async fn create_thing() -> &'static str {
    "created"
}

/// A plugin that mounts its routes through the recording builder. It does NOT
/// implement `routes()` / `route_paths()` — the whole point is that one method
/// supplies both halves.
struct BuilderPlugin;

impl Plugin for BuilderPlugin {
    fn name(&self) -> &'static str {
        "builder_demo"
    }

    fn routes_builder(&self) -> Option<Routes> {
        Some(
            Routes::new()
                .get("/demo/health", health)
                .post("/demo/thing", create_thing),
        )
    }
}

async fn boot() -> axum::Router {
    let pool = umbral_core::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let app = App::builder()
        .settings(umbral::Settings::from_env().expect("settings"))
        .database("default", pool)
        .plugin(BuilderPlugin)
        .build()
        .expect("App::build");
    app.into_router()
}

/// The two builder routes are actually served (200), AND the published registry
/// lists exactly those two paths under the plugin — one source, zero drift.
#[tokio::test]
async fn builder_routes_are_mounted_and_match_the_registry_exactly() {
    let router = boot().await;

    // 1. Both declared routes are really mounted and respond.
    let health = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/demo/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK, "GET /demo/health served");

    let thing = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/demo/thing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(thing.status(), StatusCode::OK, "POST /demo/thing served");

    // 2. The registry entry for the plugin lists EXACTLY those paths — derived
    //    from the same builder, so it cannot drift from what was mounted.
    let registry = umbral::routes::get().expect("registry published at build");
    let specs = registry
        .by_plugin
        .get("builder_demo")
        .expect("plugin's routes recorded");

    let mut paths: Vec<&str> = specs.iter().map(|s| s.path.as_str()).collect();
    paths.sort();
    assert_eq!(
        paths,
        vec!["/demo/health", "/demo/thing"],
        "registry lists exactly the mounted paths — no drift"
    );

    // The method was recorded too, not just the path.
    let thing_spec = specs
        .iter()
        .find(|s| s.path == "/demo/thing")
        .expect("thing spec");
    assert_eq!(thing_spec.methods, vec!["POST"]);
}
