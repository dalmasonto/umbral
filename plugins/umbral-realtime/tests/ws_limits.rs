//! WebSocket inbound message-size cap: a frame above the configured
//! `ws_max_message_bytes` never reaches the app's `MessageHandler` and
//! terminates the connection, while frames under the cap flow normally.
//!
//! One test per binary: the ambient `Realtime` handle is process-global, so
//! the capped plugin config is installed exactly once here.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::SinkExt;
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

const CAP: usize = 1024;

/// Build the app with a small inbound WS message cap, bind a server on a
/// random port, and return its address + the recorded-inbound channel.
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
        .plugin(
            RealtimePlugin::default()
                .message_handler(Recorder { tx })
                .ws_max_message_bytes(CAP),
        )
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
        if Realtime::registry().connection_count().await == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn oversized_inbound_frame_is_rejected_and_small_frames_pass() {
    let (addr, mut inbound) = setup().await;

    let (mut ws, _resp) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/realtime/ws?groups=public:lobby"))
            .await
            .expect("connect");
    wait_for_conns(1).await;
    assert_eq!(Realtime::registry().connection_count().await, 1);

    // Under the cap: dispatched to the handler as usual.
    ws.send(Message::Text("small-message".into()))
        .await
        .expect("client send (small)");
    let got = tokio::time::timeout(Duration::from_secs(3), inbound.recv())
        .await
        .expect("inbound timeout (small)")
        .expect("handler received the small message");
    assert_eq!(got, "small-message");

    // Over the cap: the server refuses the frame — it must never reach the
    // handler, and the connection is torn down (and deregistered).
    let oversized = "x".repeat(CAP * 4);
    ws.send(Message::Text(oversized))
        .await
        .expect("client send (oversized)");

    // The server drops the connection on the capacity violation.
    wait_for_conns(0).await;
    assert_eq!(
        Realtime::registry().connection_count().await,
        0,
        "the oversized frame must terminate (and deregister) the connection"
    );

    // And the handler never saw the oversized payload.
    match tokio::time::timeout(Duration::from_millis(300), inbound.recv()).await {
        Err(_elapsed) => {} // nothing arrived — correct
        Ok(None) => {}      // channel closed with nothing buffered — also fine
        Ok(Some(msg)) => panic!(
            "an oversized frame reached the MessageHandler ({} bytes)",
            msg.len()
        ),
    }
}
