//! SSE transport — `GET /realtime/sse?groups=chat:123,presence`.
//!
//! Resolves the connection's identity from the session, validates each
//! requested group against the [`GroupPolicy`](crate::GroupPolicy),
//! registers the connection, and streams its events as Server-Sent
//! Events. A `ConnGuard` deregisters on disconnect so no index leaks.

use std::collections::HashSet;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::Query;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use http::{HeaderMap, StatusCode};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::{ConnId, DEFAULT_BUFFER, Event, Realtime, Registry};

/// `?groups=chat:123,presence` — the rooms a client joins at handshake.
#[derive(Deserialize)]
pub(crate) struct SseQuery {
    groups: Option<String>,
}

/// The SSE endpoint handler. Identity → group policy → register → stream.
pub(crate) async fn sse_handler(headers: HeaderMap, Query(q): Query<SseQuery>) -> Response {
    let user_id = umbra_auth::current_session_user_id(&headers).await;

    let requested: Vec<String> = q
        .groups
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    // Default-deny: a group the policy rejects fails the whole handshake,
    // so a client can't subscribe to a room it has no claim to.
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
    let (conn_id, rx) = registry.register(user_id, groups, DEFAULT_BUFFER).await;

    let stream = SseConn {
        rx,
        _guard: ConnGuard { registry, conn_id },
    };
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}

/// Deregisters the connection when the stream is dropped (client
/// disconnect / server shutdown), even on an abruptly-closed socket.
/// `deregister` is async, so it spawns — the server runtime is always
/// present when a response body drops.
struct ConnGuard {
    registry: Arc<Registry>,
    conn_id: ConnId,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let registry = self.registry.clone();
        let id = self.conn_id;
        tokio::spawn(async move {
            registry.deregister(id).await;
        });
    }
}

/// The SSE body stream: drains the connection's channel into SSE events.
struct SseConn {
    rx: mpsc::Receiver<Event>,
    _guard: ConnGuard,
}

impl Stream for SseConn {
    type Item = Result<SseEvent, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // SseConn is Unpin (Receiver + ConnGuard are), so get_mut is sound.
        let this = self.get_mut();
        match this.rx.poll_recv(cx) {
            Poll::Ready(Some(ev)) => Poll::Ready(Some(Ok(SseEvent::default()
                .event(ev.event)
                .data(ev.data.to_string())))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
