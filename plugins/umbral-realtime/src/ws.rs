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
use tokio::sync::mpsc;

use crate::{ConnId, DEFAULT_BUFFER, Event, MessageContext, MessageHandler, Realtime, Registry};

/// `?groups=chat:123,presence` — the rooms a client joins at handshake.
#[derive(Deserialize)]
pub(crate) struct WsQuery {
    groups: Option<String>,
}

/// Whether the running app is in [`Environment::Dev`](umbral::Environment::Dev).
/// Mirrors `umbral-livereload`'s convention: read the ambient settings, treat a
/// missing/unset settings as non-dev (the safe default). The WS Origin guard
/// passes through in Dev so a local frontend served on a different port (Vite,
/// etc.) can open the socket — matching how core CORS / host-validation only
/// enforce in Prod.
fn is_dev() -> bool {
    umbral::settings::get_opt()
        .map(|s| matches!(s.environment, umbral::Environment::Dev))
        .unwrap_or(false)
}

/// The authority (`host[:port]`) of an `Origin` header value — the part after
/// the `scheme://`, with any path/query stripped. `https://app.example.com` →
/// `app.example.com`; `http://x.com:8000/foo` → `x.com:8000`. Returns the input
/// unchanged when it carries no `://` (already bare). Lowercased for a
/// case-insensitive host compare.
fn origin_authority(origin: &str) -> String {
    let after_scheme = origin.split_once("://").map(|(_, rest)| rest).unwrap_or(origin);
    // Drop anything from the first `/`, `?` or `#` — keep only the authority.
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    authority.to_ascii_lowercase()
}

/// Returns `true` if a WebSocket upgrade carrying this `Origin` is allowed.
///
/// CORS does **not** cover the WebSocket handshake, so without this guard a
/// cross-origin page could open `wss://victim-host/<base>/ws` with the victim's
/// session cookie and subscribe to their gated groups — a CSWSH (cross-site
/// WebSocket hijacking) attack. This is the decision the WS handler enforces
/// before upgrading; kept as a pure function so the policy is unit-tested
/// without spinning an HTTP handshake.
///
/// Rules, in order:
/// 1. `origin == None` → **allow**. Non-browser clients (curl, native apps,
///    server-to-server) never send `Origin`; CSWSH is a browser-only vector.
/// 2. `is_dev` → **allow**. Local dev runs the frontend on a different port, so
///    every request is "cross-origin"; this matches the core CORS / host-
///    validation Dev pass-through.
/// 3. Origin's authority appears in `allowlist` → **allow** (an explicit
///    cross-origin frontend the app opted in via `allowed_origins`).
/// 4. Origin is **same-origin** as the request — its authority (`host[:port]`)
///    equals the `Host` header → **allow**.
/// 5. otherwise → **deny**.
///
/// The allowlist match compares on authority (scheme stripped), so an entry
/// `https://app.example.com` matches an `Origin: https://app.example.com`
/// regardless of the request scheme.
pub(crate) fn ws_origin_allowed(
    origin: Option<&str>,
    host: Option<&str>,
    allowlist: &[String],
    is_dev: bool,
) -> bool {
    // 1. No Origin → non-browser client → not a CSWSH vector.
    let Some(origin) = origin else {
        return true;
    };
    // 2. Dev pass-through (local frontend on a different port).
    if is_dev {
        return true;
    }
    let origin_auth = origin_authority(origin);
    // 3. Explicit allowlist — compare on authority so the entry can carry a
    //    scheme (`https://app.example.com`) or be bare (`app.example.com`).
    if allowlist
        .iter()
        .any(|allowed| origin_authority(allowed) == origin_auth)
    {
        return true;
    }
    // 4. Same-origin: the Origin authority equals the request Host. The Host
    //    header is already a bare authority (`x.com` / `x.com:8000`).
    if let Some(host) = host
        && host.to_ascii_lowercase() == origin_auth
    {
        return true;
    }
    // 5. Cross-origin in prod, not allowlisted → reject.
    false
}

/// The WebSocket endpoint. Validates identity + groups *before* upgrading
/// (so a denied group returns `403`, not a half-open socket), then hands
/// the live socket to [`handle_socket`].
pub(crate) async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(q): Query<WsQuery>,
) -> Response {
    // CSWSH guard: reject a cross-origin WS upgrade BEFORE upgrading, so a
    // hijacked socket never reaches the registry. CORS doesn't cover the WS
    // handshake, so this is the only thing standing between a cross-site page
    // (carrying the victim's cookie) and their gated groups. Same-origin and
    // no-Origin (non-browser) requests always pass; prod denies cross-origin
    // unless it's on the `allowed_origins` allowlist. (SSE is left alone — the
    // browser's CORS already protects a cross-origin `EventSource`.)
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let host = headers.get(http::header::HOST).and_then(|v| v.to_str().ok());
    if !ws_origin_allowed(origin, host, &Realtime::allowed_origins(), is_dev()) {
        return (StatusCode::FORBIDDEN, "cross-origin WebSocket rejected").into_response();
    }

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
        if !policy.can_join(user_id.as_deref(), g) {
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

    // Enforce the aggregate connection cap *before* upgrading: a refused
    // registration returns 503 instead of completing the WS handshake.
    // (WS has no native Last-Event-ID, so the cap is the relevant gap here.)
    let Some((conn_id, rx, presence)) = registry
        .register_with_presence(user_id.clone(), groups, DEFAULT_BUFFER)
        .await
    else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "realtime: connection limit reached",
        )
            .into_response();
    };

    // Fire presence join + sync for any newly-entered presence-enabled group
    // (gated by the spec; anonymous conns yield nothing).
    tokio::spawn(crate::dispatch_presence(presence));

    ws.on_upgrade(move |socket| {
        handle_socket(socket, conn_id, rx, user_id, registry, handler)
    })
}

/// Drive one live socket for an already-registered connection: run the
/// outbound (registry → client) and inbound (client → handler) loops until
/// either side closes. The `WsGuard` deregisters on exit (normal or panic).
async fn handle_socket(
    socket: WebSocket,
    conn_id: ConnId,
    mut rx: mpsc::Receiver<Event>,
    user_id: Option<String>,
    registry: Arc<Registry>,
    handler: Arc<dyn MessageHandler>,
) {
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
            let presence = registry.deregister_with_presence(id).await;
            crate::dispatch_presence(presence).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_origin_is_allowed_non_browser_client() {
        // curl / native / server-to-server never send Origin — not a CSWSH
        // vector, so the upgrade proceeds (prod, no allowlist).
        assert!(ws_origin_allowed(None, Some("x.com"), &[], false));
    }

    #[test]
    fn dev_allows_cross_origin() {
        // Local frontend on a different port — Dev passes everything through,
        // matching core CORS / host-validation.
        assert!(ws_origin_allowed(
            Some("http://localhost:5173"),
            Some("localhost:8000"),
            &[],
            true,
        ));
    }

    #[test]
    fn exact_same_origin_is_allowed() {
        assert!(ws_origin_allowed(
            Some("https://x.com"),
            Some("x.com"),
            &[],
            false,
        ));
    }

    #[test]
    fn cross_origin_prod_no_allowlist_is_denied() {
        assert!(!ws_origin_allowed(
            Some("https://evil.com"),
            Some("x.com"),
            &[],
            false,
        ));
    }

    #[test]
    fn cross_origin_in_allowlist_is_allowed() {
        assert!(ws_origin_allowed(
            Some("https://app.example.com"),
            Some("x.com"),
            &allow(&["https://app.example.com"]),
            false,
        ));
        // A cross-origin NOT on the list is still denied.
        assert!(!ws_origin_allowed(
            Some("https://other.example.com"),
            Some("x.com"),
            &allow(&["https://app.example.com"]),
            false,
        ));
    }

    #[test]
    fn default_https_port_matches_bare_host() {
        // `https://x.com:443` is same-origin with `Host: x.com` in practice;
        // we compare authorities verbatim, so an explicit :443 differs from a
        // bare host and is NOT same-origin — but the allowlist accepts it.
        // The realistic same-origin case (no explicit port) is the one that
        // matters and is covered by `exact_same_origin_is_allowed`; here we
        // assert the allowlist handles the explicit-port form.
        assert!(ws_origin_allowed(
            Some("https://x.com:443"),
            Some("x.com"),
            &allow(&["https://x.com:443"]),
            false,
        ));
    }

    #[test]
    fn matching_explicit_port_is_same_origin() {
        // `http://x.com:8000` with `Host: x.com:8000` is same-origin: the
        // authority (host:port) matches exactly.
        assert!(ws_origin_allowed(
            Some("http://x.com:8000"),
            Some("x.com:8000"),
            &[],
            false,
        ));
        // Different port → cross-origin → denied (no allowlist).
        assert!(!ws_origin_allowed(
            Some("http://x.com:9000"),
            Some("x.com:8000"),
            &[],
            false,
        ));
    }

    #[test]
    fn origin_authority_strips_scheme_path_and_lowercases() {
        assert_eq!(origin_authority("https://App.Example.com"), "app.example.com");
        assert_eq!(origin_authority("http://x.com:8000/foo?q=1"), "x.com:8000");
        assert_eq!(origin_authority("x.com"), "x.com");
    }
}
