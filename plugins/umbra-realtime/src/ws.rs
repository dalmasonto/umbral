//! WebSocket transport — `GET /realtime/ws?groups=chat:123`.
//!
//! Bidirectional. Outbound shares the same per-connection sink as SSE
//! (events arrive as JSON text frames `{"event":..,"data":..}`); inbound
//! client frames dispatch to the app's [`MessageHandler`](crate::MessageHandler).
//! Identity + group policy are checked at handshake exactly like SSE; a
//! rejected group fails with `403` before the upgrade.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::Query;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use http::{HeaderMap, StatusCode};
use serde::Deserialize;

use crate::{ConnId, DEFAULT_BUFFER, MessageContext, MessageHandler, Realtime, Registry};

/// `?groups=chat:123,presence` — the rooms a client joins at handshake.
#[derive(Deserialize)]
pub(crate) struct WsQuery {
    groups: Option<String>,
}

/// The WebSocket endpoint. Validates identity + groups *before* upgrading
/// (so a denied group returns `403`, not a half-open socket), then hands
/// the live socket to [`handle_socket`].
pub(crate) async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(q): Query<WsQuery>,
) -> Response {
    let user_id = Realtime::resolver()(headers.clone()).await;

    let requested: Vec<String> = q
        .groups
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    let policy = Realtime::policy();
    for g in &requested {
        if !policy.can_join(user_id, g) {
            return (
                StatusCode::FORBIDDEN,
                format!("not allowed to join group `{g}`"),
            )
                .into_response();
        }
    }

    let groups: HashSet<String> = requested.into_iter().collect();
    let registry = Realtime::registry();
    let handler = Realtime::message_handler();
    ws.on_upgrade(move |socket| handle_socket(socket, user_id, groups, registry, handler))
}

/// Drive one live socket: register the connection, then run the outbound
/// (registry → client) and inbound (client → handler) loops until either
/// side closes. The `WsGuard` deregisters on exit (normal or panic).
async fn handle_socket(
    socket: WebSocket,
    user_id: Option<i64>,
    groups: HashSet<String>,
    registry: Arc<Registry>,
    handler: Arc<dyn MessageHandler>,
) {
    let (conn_id, mut rx) = registry.register(user_id, groups, DEFAULT_BUFFER).await;
    let _guard = WsGuard {
        registry: registry.clone(),
        conn_id,
    };
    let (mut tx, mut wrx) = socket.split();
    let ctx = MessageContext { conn_id, user_id };

    // Outbound: drain the connection's channel into JSON text frames.
    let outbound = async {
        while let Some(ev) = rx.recv().await {
            let frame = serde_json::json!({ "event": ev.event, "data": ev.data }).to_string();
            if tx.send(Message::Text(frame.into())).await.is_err() {
                break;
            }
        }
    };

    // Inbound: dispatch client text frames to the app handler. axum
    // auto-replies to pings; we ignore binary/ping/pong here.
    let inbound = async {
        while let Some(Ok(msg)) = wrx.next().await {
            match msg {
                Message::Text(t) => handler.on_message(&ctx, t.as_str().to_string()).await,
                Message::Close(_) => break,
                _ => {}
            }
        }
    };

    tokio::select! {
        _ = outbound => {},
        _ = inbound => {},
    }
    // `_guard` drops here → deregister.
}

/// Deregisters the connection when the socket task ends.
struct WsGuard {
    registry: Arc<Registry>,
    conn_id: ConnId,
}

impl Drop for WsGuard {
    fn drop(&mut self) {
        let registry = self.registry.clone();
        let id = self.conn_id;
        tokio::spawn(async move {
            registry.deregister(id).await;
        });
    }
}
