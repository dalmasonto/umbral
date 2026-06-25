//! SSE transport integration test: the `GET /realtime/sse` route gates
//! groups via the policy, registers a connection, and streams events sent
//! through the ambient `Realtime` handle.
//!
//! One test function (one `App::build` per process — settings init is
//! one-shot) covering the deny path, the register path, and delivery.

use std::time::Duration;

use axum::body::Body;
use http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use umbral_realtime::{Realtime, RealtimePlugin};

async fn boot() -> axum::Router {
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(RealtimePlugin::default())
        .build()
        .expect("App::build");
    app.into_router()
}

async fn get(router: &axum::Router, uri: &str) -> http::Response<Body> {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    router.clone().oneshot(req).await.expect("oneshot")
}

/// Read frames from an SSE body until a non-empty data frame arrives (the
/// event), or time out. The 15s keep-alive won't fire inside this window,
/// so the first non-empty frame is our event.
async fn read_event(body: Body) -> String {
    let mut body = body;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        let s = String::from_utf8_lossy(&data).to_string();
                        if !s.trim().is_empty() {
                            return s;
                        }
                    }
                }
                _ => return String::new(),
            }
        }
    })
    .await
    .unwrap_or_default()
}

#[tokio::test]
async fn sse_route_gates_groups_registers_and_streams() {
    let router = boot().await;

    // 1. A non-public group is denied by the default policy → 403, and no
    //    connection is registered.
    let resp = get(&router, "/realtime/sse?groups=secret:room").await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "non-public group denied"
    );
    assert_eq!(
        Realtime::registry().connection_count().await,
        0,
        "a denied handshake registers nothing"
    );

    // 2. A public group is allowed → 200 + text/event-stream, and the
    //    connection is registered (the response is held, not dropped yet).
    let resp = get(&router, "/realtime/sse?groups=public:lobby").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ctype.contains("text/event-stream"),
        "SSE content-type, got {ctype:?}"
    );
    assert_eq!(
        Realtime::registry().connection_count().await,
        1,
        "the public handshake registered one connection"
    );

    // 3. Send an event to the group; it arrives on the open stream.
    Realtime::to_group("public:lobby")
        .send("greeting", &serde_json::json!({ "hi": "there" }))
        .await;

    let frame = read_event(resp.into_body()).await;
    assert!(
        frame.contains("greeting"),
        "the SSE frame names the event; got: {frame:?}"
    );
    assert!(
        frame.contains("there"),
        "the SSE frame carries the JSON payload; got: {frame:?}"
    );
}
