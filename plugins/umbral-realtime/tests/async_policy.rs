//! gaps4 #36: the group policy is async, so a room can be gated on async
//! state — the canonical case being a DB membership check at the SSE/WS
//! handshake. Before this, `GroupPolicy::can_join` was sync and a
//! downstream app had to bridge with `tokio::task::block_in_place` +
//! `Handle::current().block_on(...)` (panics on a current-thread runtime,
//! parks a worker thread per join).
//!
//! This drives a REAL async policy through the REAL SSE handshake: the
//! policy awaits shared async state (an RwLock membership set standing in
//! for the membership table) on every decision. A member is admitted, a
//! non-member is refused, an anonymous caller is refused — all at the
//! handshake, with no worker-thread parking.
//!
//! Own test binary: the ambient `Realtime` handle is a one-shot `OnceLock`
//! per process, so a custom-policy boot can't share a binary with the
//! default-policy tests.

use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Body;
use http::{Request, StatusCode};
use tower::ServiceExt;
use umbral_realtime::{Realtime, RealtimePlugin};

async fn boot() -> axum::Router {
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    // The "membership table": user 7 belongs to project 1. The policy reads
    // it through an async lock, so every decision is a genuine await point.
    let members: Arc<tokio::sync::RwLock<HashSet<(String, String)>>> = Arc::new(
        tokio::sync::RwLock::new(HashSet::from([("7".to_string(), "project:1".to_string())])),
    );

    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(
            RealtimePlugin::default()
                // Identity from the `x-user` header — the async resolver
                // precedent; a real app uses with_auth_sessions().
                .identity_resolver(|headers: http::HeaderMap| async move {
                    headers
                        .get("x-user")
                        .and_then(|v| v.to_str().ok())
                        .map(String::from)
                })
                .group_policy_async_fn(move |user_id, group| {
                    let members = members.clone();
                    async move {
                        if group.starts_with("public:") {
                            return true;
                        }
                        let Some(uid) = user_id else { return false };
                        members.read().await.contains(&(uid, group))
                    }
                }),
        )
        .build()
        .expect("App::build");
    app.into_router()
}

async fn sse(router: &axum::Router, uri: &str, user: Option<&str>) -> http::Response<Body> {
    let mut req = Request::builder().uri(uri);
    if let Some(u) = user {
        req = req.header("x-user", u);
    }
    router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .expect("oneshot")
}

#[tokio::test]
async fn async_policy_gates_the_sse_handshake_on_membership() {
    let router = boot().await;

    // A member joins their project room: the policy awaited the membership
    // read and admitted the handshake.
    let resp = sse(&router, "/realtime/sse?groups=project:1", Some("7")).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a member is admitted to the private group at the handshake"
    );
    assert_eq!(Realtime::registry().connection_count().await, 1);

    // A non-member is refused the same room — the async lookup fails closed.
    let resp = sse(&router, "/realtime/sse?groups=project:1", Some("8")).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "a non-member is refused the private group"
    );

    // Anonymous is refused: no identity, no membership to match.
    let resp = sse(&router, "/realtime/sse?groups=project:1", None).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "an anonymous caller is refused the private group"
    );

    // The public prefix stays open without consulting membership.
    let resp = sse(&router, "/realtime/sse?groups=public:lobby", None).await;
    assert_eq!(resp.status(), StatusCode::OK, "public rooms stay public");

    // Nothing extra got registered by the denied handshakes.
    assert_eq!(
        Realtime::registry().connection_count().await,
        2,
        "only the two admitted handshakes registered connections"
    );
}
