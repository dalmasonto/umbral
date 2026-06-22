//! WebSocket Origin guard (CSWSH) integration test: in `Environment::Prod`,
//! a WS upgrade carrying a cross-origin `Origin` header is rejected with 403
//! *before* the handshake completes, while a same-origin one (and a request
//! with no Origin) connects. Also asserts a `.at("/rt")` remount mounts the
//! WS endpoint at the new base.

use std::net::SocketAddr;

use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::StatusCode;
use umbra_realtime::RealtimePlugin;

/// Boot a prod app with the realtime plugin mounted at `base`, bind a server,
/// return its address. Prod is required so the Origin guard actually enforces
/// (Dev passes everything through).
async fn setup(base: &'static str) -> SocketAddr {
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();
    settings.environment = umbra::Environment::Prod;
    // Prod boot fails a system check on the insecure dev secret-key default;
    // set a real one so the app builds (we're testing the Origin guard, not
    // the secret-key check).
    settings.secret_key = "test-secret-key-not-the-dev-default-0123456789".to_string();

    let app = umbra::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(RealtimePlugin::default().at(base))
        .build()
        .expect("App::build");
    let router = app.into_router();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn cross_origin_ws_upgrade_is_rejected_in_prod() {
    // Mount at /rt to also prove `.at()` remounts the WS endpoint.
    let addr = setup("/rt").await;

    // A cross-origin upgrade (Origin != Host) must be rejected with 403
    // BEFORE the WS handshake — the CSWSH guard. tungstenite surfaces a
    // non-101 handshake as an error carrying the HTTP response.
    let mut req = format!("ws://{addr}/rt/ws?groups=public:lobby")
        .into_client_request()
        .expect("request");
    req.headers_mut()
        .insert("origin", "https://evil.example.com".parse().unwrap());
    let denied = tokio_tungstenite::connect_async(req).await;
    match denied {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "cross-origin WS upgrade must be 403"
            );
        }
        Err(other) => panic!("expected an HTTP 403, got error: {other:?}"),
        Ok(_) => panic!("cross-origin WS upgrade should have been rejected, but it connected"),
    }

    // A same-origin upgrade (Origin host == Host) connects: the Host the
    // client sends is `127.0.0.1:<port>`, so make the Origin match it.
    let mut req = format!("ws://{addr}/rt/ws?groups=public:lobby")
        .into_client_request()
        .expect("request");
    req.headers_mut().insert(
        "origin",
        format!("http://{addr}").parse().unwrap(),
    );
    let (_ws, resp) = tokio_tungstenite::connect_async(req)
        .await
        .expect("same-origin WS upgrade should connect");
    assert_eq!(
        resp.status(),
        StatusCode::SWITCHING_PROTOCOLS,
        "same-origin upgrade completes the handshake"
    );

    // A request with NO Origin (non-browser client) connects too: tungstenite
    // doesn't add an Origin header by default, so the plain URL form works.
    let (_ws2, resp2) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/rt/ws?groups=public:lobby"))
            .await
            .expect("no-Origin WS upgrade should connect");
    assert_eq!(resp2.status(), StatusCode::SWITCHING_PROTOCOLS);
}
