//! WebSocket transport integration test: a real bound server + a
//! tungstenite client exercising both directions — server→client push
//! (`Realtime::to_group(...).send`) and client→server (`MessageHandler`),
//! plus the handshake group gate.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_tungstenite::tungstenite::Message;
use umbral_realtime::{MessageContext, MessageHandler, Realtime, RealtimePlugin};

/// Records inbound client messages so the test can assert on them.
struct Recorder {
    tx: UnboundedSender<String>,
}

#[umbral_realtime::async_trait]
impl MessageHandler for Recorder {
    async fn on_message(&self, _ctx: &MessageContext, text: String) {
        let _ = self.tx.send(text);
    }
}

/// Build the app (sets the ambient Realtime + the recording handler), bind
/// a server on a random port, and return its address + the inbound channel.
async fn setup() -> (SocketAddr, UnboundedReceiver<String>) {
    let (tx, rx) = unbounded_channel();

    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    let app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(RealtimePlugin::default().message_handler(Recorder { tx }))
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

    (addr, rx)
}

async fn wait_for_conns(n: usize) {
    for _ in 0..150 {
        if Realtime::registry().connection_count().await >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn ws_round_trip_and_group_gate() {
    let (addr, mut inbound) = setup().await;

    // A non-public group is rejected at handshake → the upgrade fails.
    let denied =
        tokio_tungstenite::connect_async(format!("ws://{addr}/realtime/ws?groups=secret:room"))
            .await;
    assert!(
        denied.is_err(),
        "non-public group must fail the WS handshake"
    );

    // A public group connects.
    let (mut ws, _resp) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/realtime/ws?groups=public:lobby"))
            .await
            .expect("connect");
    wait_for_conns(1).await;
    assert_eq!(
        Realtime::registry().connection_count().await,
        1,
        "the WS connection registered"
    );

    // Outbound: server → client.
    Realtime::to_group("public:lobby")
        .send("greeting", &serde_json::json!({ "hi": "there" }))
        .await;
    let msg = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("outbound timeout")
        .expect("stream ended")
        .expect("ws error");
    let text = match msg {
        Message::Text(t) => t.to_string(),
        other => panic!("expected a text frame, got {other:?}"),
    };
    assert!(
        text.contains("greeting") && text.contains("there"),
        "the pushed event arrives as a JSON frame; got: {text}"
    );

    // Inbound: client → MessageHandler.
    ws.send(Message::Text("ping-from-client".into()))
        .await
        .expect("client send");
    let got = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .expect("inbound timeout")
        .expect("handler received the message");
    assert_eq!(got, "ping-from-client");
}
