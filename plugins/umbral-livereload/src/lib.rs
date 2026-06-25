//! umbral-livereload — opt-in dev live-reload over SSE.
//!
//! Add it to your app and, in `Dev`, the browser reloads itself when you
//! save a template or asset — no manual refresh, no polling:
//!
//! ```ignore
//! App::builder()
//!     .plugin(LiveReloadPlugin::new())          // watches ./templates + ./static
//!     // .plugin(LiveReloadPlugin::new().watch("plugins"))  // add more roots
//!     .build()?;
//! ```
//!
//! ## How it works (the Vite shape, minus the bundler)
//!
//! - A **file watcher** ([`notify`]) runs in the server process and, on a
//!   save, **pushes** an event down an open **SSE** connection. The browser
//!   never polls — the server speaks first.
//! - A `.css` change pushes a `css` event → the client swaps the
//!   stylesheet `<link>` in place (no reload). Anything else pushes
//!   `reload` → `location.reload()`.
//! - A **`.rs` change** is handled by `umbral dev` rebuilding + restarting
//!   the binary: the SSE connection drops, the browser auto-reconnects to
//!   the new process, sees a new **boot id**, and reloads.
//! - The tiny client script is **auto-injected** into every `text/html`
//!   response via [`Plugin::wrap_router`], so there's zero per-app
//!   template work — any app that adds the plugin gets reload everywhere.
//!
//! Everything is gated to [`Environment::Dev`]; in any other environment
//! the plugin contributes nothing (no route, no watcher, no injection).
//!
//! Why SSE over WebSocket: a reload signal is one-way (server → client),
//! and `EventSource` reconnects on its own — which is exactly the
//! restart→reload behaviour we want, for free.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast;
use umbral::plugin::{AppContext, Plugin, PluginError};
use umbral::web::{Router, get};

/// SSE endpoint the injected client (or the shared worker) connects to.
const SSE_PATH: &str = "/__umbral/livereload";

/// URL the injected client loads as a `SharedWorker`. One worker per origin
/// holds a SINGLE `EventSource` to [`SSE_PATH`] and fans events out to every
/// open tab, so N tabs share 1 server connection instead of N — the dev page
/// never exhausts the browser's ~6-per-host HTTP/1.1 connection budget.
const WORKER_PATH: &str = "/__umbral/livereload-worker.js";

/// Capacity of the reload broadcast channel. Reload events are coalesced
/// (one per debounce window), so a handful of slots is plenty.
const BUS_CAPACITY: usize = 16;

/// Process-wide reload bus, set in `on_ready` (Dev only). The SSE handler
/// subscribes; the file watcher publishes.
static BUS: OnceLock<broadcast::Sender<String>> = OnceLock::new();

/// Per-process boot id sent to the client on connect. A new value after a
/// reconnect means the server restarted → the client reloads.
static BOOT_ID: OnceLock<String> = OnceLock::new();

/// Keeps the `notify` watcher alive for the process lifetime (dropping it
/// stops watching).
static WATCHER: OnceLock<std::sync::Mutex<notify::RecommendedWatcher>> = OnceLock::new();

/// Opt-in dev live-reload plugin. `new()` watches `./templates` and
/// `./static`; add more roots with [`watch`](LiveReloadPlugin::watch).
#[derive(Debug, Clone)]
pub struct LiveReloadPlugin {
    watch_dirs: Vec<PathBuf>,
}

impl Default for LiveReloadPlugin {
    fn default() -> Self {
        Self {
            watch_dirs: vec![PathBuf::from("templates"), PathBuf::from("static")],
        }
    }
}

impl LiveReloadPlugin {
    /// New plugin watching the default roots (`./templates`, `./static`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Add another path to watch — a **directory** (watched recursively) or
    /// a **single file**. Relative paths resolve against the process working
    /// directory. Use this for non-standard layouts:
    ///
    /// ```ignore
    /// LiveReloadPlugin::new()
    ///     .watch("plugins")          // per-plugin template dirs
    ///     .watch("content")          // a markdown/content tree
    ///     .watch("site.config.toml") // a single config file
    /// ```
    ///
    /// You do **not** need to watch `src` for Rust changes: `main.rs` and
    /// every other `.rs` edit already reloads the browser via the
    /// `umbral dev` rebuild → restart → reconnect path (the watcher ignores
    /// `.rs` so it can't fire a premature reload before the new build is up).
    pub fn watch(mut self, path: impl Into<PathBuf>) -> Self {
        self.watch_dirs.push(path.into());
        self
    }

    /// Replace the watch list entirely (directories and/or files).
    pub fn watch_only(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        self.watch_dirs = paths.into_iter().collect();
        self
    }
}

impl Plugin for LiveReloadPlugin {
    fn name(&self) -> &'static str {
        "livereload"
    }

    fn routes(&self) -> Router {
        if !is_dev() {
            return Router::new();
        }
        Router::new()
            .route(SSE_PATH, get(sse_handler))
            .route(WORKER_PATH, get(worker_handler))
    }

    fn wrap_router(&self, router: Router) -> Router {
        if !is_dev() {
            return router;
        }
        // Inject the client script into every text/html response.
        router.layer(axum::middleware::from_fn(inject_client))
    }

    fn on_ready(&self, ctx: &AppContext) -> Result<(), PluginError> {
        if !matches!(ctx.settings.environment, umbral::Environment::Dev) {
            return Ok(());
        }
        let boot = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos().to_string())
            .unwrap_or_else(|_| "dev".to_string());
        let _ = BOOT_ID.set(boot);

        let (tx, _rx) = broadcast::channel::<String>(BUS_CAPACITY);
        let _ = BUS.set(tx.clone());

        spawn_watcher(self.watch_dirs.clone(), tx);
        tracing::info!(
            "livereload: SSE at {SSE_PATH}, watching {:?}",
            self.watch_dirs
        );
        Ok(())
    }
}

/// True when the ambient settings say we're in Dev. False if settings
/// aren't initialised (defensive — `App::build` sets them before plugin
/// hooks run).
fn is_dev() -> bool {
    umbral::settings::get_opt()
        .map(|s| matches!(s.environment, umbral::Environment::Dev))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// SSE endpoint
// ---------------------------------------------------------------------------

async fn sse_handler() -> impl axum::response::IntoResponse {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures_util::StreamExt;

    let boot = BOOT_ID.get().cloned().unwrap_or_default();
    // First frame: the boot id. The client reloads if it ever sees a
    // different one after a reconnect (server restarted).
    let hello = futures_util::stream::once(async move {
        Ok::<_, std::convert::Infallible>(Event::default().event("hello").data(boot))
    });

    let updates = match BUS.get() {
        Some(tx) => tokio_stream::wrappers::BroadcastStream::new(tx.subscribe())
            .filter_map(|msg| async move {
                msg.ok().map(|data| {
                    Ok::<_, std::convert::Infallible>(Event::default().event("change").data(data))
                })
            })
            .boxed(),
        None => futures_util::stream::empty().boxed(),
    };

    let stream = hello.chain(updates).boxed();
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

// ---------------------------------------------------------------------------
// Shared worker — one SSE connection for all tabs
// ---------------------------------------------------------------------------

/// Serve the `SharedWorker` script (one per origin) that owns the single
/// `EventSource` to [`SSE_PATH`] and fans every event out to all tabs.
async fn worker_handler() -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        WORKER_JS,
    )
}

/// The `SharedWorker` body. It holds ONE `EventSource` to the SSE endpoint
/// regardless of how many tabs are open, posting each event to every
/// connected tab's port. A freshly-opened tab is immediately synced to the
/// current boot id so it still reloads on the next restart. When the last
/// tab closes, the lone `EventSource` is left to `EventSource`'s own
/// reconnect; the next opened tab reuses the already-live worker.
const WORKER_JS: &str = r#"// umbral livereload shared worker — one EventSource for every tab.
"use strict";
var ports = [];
var es = null;
var lastHello = null;
function broadcast(msg) {
  ports = ports.filter(function (p) {
    try { p.postMessage(msg); return true; } catch (_) { return false; }
  });
}
function connect() {
  es = new EventSource("/__umbral/livereload");
  es.addEventListener("hello", function (e) {
    lastHello = e.data;
    broadcast({ kind: "hello", data: e.data });
  });
  es.addEventListener("change", function (e) {
    broadcast({ kind: "change", data: e.data });
  });
  es.onerror = function () { broadcast({ kind: "error" }); };
}
self.onconnect = function (e) {
  var port = e.ports[0];
  ports.push(port);
  port.start();
  port.onmessage = function (ev) {
    if (ev.data === "bye") {
      ports = ports.filter(function (p) { return p !== port; });
    }
  };
  // Sync a newly-opened tab to the current boot id: it otherwise wouldn't see
  // a "hello" until the next reconnect and would miss the restart reload.
  if (lastHello !== null) {
    try { port.postMessage({ kind: "hello", data: lastHello }); } catch (_) {}
  }
  if (es === null) connect();
};
"#;

// ---------------------------------------------------------------------------
// HTML-injection middleware
// ---------------------------------------------------------------------------

/// The client script, injected before `</body>` on every dev HTML response.
///
/// Preferred path: a `SharedWorker` ([`WORKER_PATH`]) holds ONE `EventSource`
/// for every tab of the origin, so opening many dev tabs never exhausts the
/// browser's ~6-connections-per-host HTTP/1.1 limit (the failure mode where
/// the page hangs as if the server were down). Falls back to a per-tab
/// `EventSource` on browsers without `SharedWorker`.
const CLIENT_SNIPPET: &str = r#"<script data-umbral-livereload>
(function () {
  var booted = null, lostLogged = false;
  function bustCss() {
    document.querySelectorAll('link[rel="stylesheet"]').forEach(function (l) {
      var base = (l.href || "").split("?")[0];
      if (base) l.href = base + "?v=" + Date.now();
    });
  }
  // One handler for events from either the shared worker or a direct stream.
  function onEvent(kind, data) {
    if (kind === "hello") {
      lostLogged = false;
      // First "hello" records the boot id; a different id later means the
      // server restarted → reload.
      if (booted === null) booted = data;
      else if (data !== booted) location.reload();
    } else if (kind === "change") {
      var d = {};
      try { d = JSON.parse(data); } catch (_) {}
      if (d.type === "css") bustCss();
      else location.reload();
    } else if (kind === "error") {
      // Expected while `umbral dev` rebuilds; the stream reconnects on its own.
      if (!lostLogged) {
        lostLogged = true;
        console.debug("[umbral livereload] connection lost — server rebuilding? reconnecting…");
      }
    }
  }
  // Preferred: share ONE connection across every tab via a SharedWorker.
  if ("SharedWorker" in window) {
    try {
      var w = new SharedWorker("/__umbral/livereload-worker.js", "umbral-livereload");
      w.port.start();
      w.port.onmessage = function (ev) { onEvent(ev.data.kind, ev.data.data); };
      window.addEventListener("beforeunload", function () {
        try { w.port.postMessage("bye"); } catch (_) {}
      });
      return;
    } catch (_) { /* fall through to a per-tab EventSource */ }
  }
  // Fallback: a per-tab EventSource (browsers without SharedWorker).
  if (!("EventSource" in window)) return;
  var es = null;
  function connect() {
    es = new EventSource("/__umbral/livereload");
    es.addEventListener("hello", function (e) { onEvent("hello", e.data); });
    es.addEventListener("change", function (e) { onEvent("change", e.data); });
    es.onerror = function () { onEvent("error"); };
  }
  window.addEventListener("beforeunload", function () { if (es) es.close(); });
  connect();
})();
</script>"#;

async fn inject_client(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::HeaderValue;
    use axum::http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};

    let mut res = next.run(req).await;

    // Dev = never let the browser cache. This is why a template/asset edit
    // can look "stuck" after a save even with no cache plugin: the browser
    // is serving its own cached copy. Force revalidation on every response
    // (HTML, CSS, JS, …) so saves always show up. Dev-only — this layer is
    // mounted only in `Environment::Dev`.
    res.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, must-revalidate"),
    );

    let is_html = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|c| c.contains("text/html"))
        .unwrap_or(false);
    if !is_html {
        return res;
    }

    let (mut parts, body) = res.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        // Body already consumed and unreadable — return an empty body with
        // the original headers rather than panicking (dev-only path).
        Err(_) => return axum::response::Response::from_parts(parts, axum::body::Body::empty()),
    };

    let html = String::from_utf8_lossy(&bytes);
    let injected = inject_into_html(&html);

    // Body length changed; drop the stale Content-Length so axum recomputes.
    parts.headers.remove(CONTENT_LENGTH);
    axum::response::Response::from_parts(parts, axum::body::Body::from(injected))
}

/// Insert the client snippet just before the closing `</body>` (falling back
/// to appending it). Pure + testable.
fn inject_into_html(html: &str) -> String {
    if let Some(idx) = html.rfind("</body>") {
        let mut out = String::with_capacity(html.len() + CLIENT_SNIPPET.len());
        out.push_str(&html[..idx]);
        out.push_str(CLIENT_SNIPPET);
        out.push_str(&html[idx..]);
        out
    } else {
        let mut out = String::with_capacity(html.len() + CLIENT_SNIPPET.len());
        out.push_str(html);
        out.push_str(CLIENT_SNIPPET);
        out
    }
}

// ---------------------------------------------------------------------------
// File watcher
// ---------------------------------------------------------------------------

/// Classify a changed path for the reload bus. `Some(true)` = CSS (hot-swap
/// in place), `Some(false)` = full reload, `None` = ignore.
///
/// This is a **denylist**, not an allowlist: anything under a watched root
/// reloads the browser *unless* it's editor/VCS/build noise or a build
/// input. That means a project with a non-standard layout (templates in
/// `views/`, content in `content/`, a data file the page reads, …) works by
/// just adding that root or file with `.watch(...)` — there's no extension
/// allowlist to keep in sync.
///
/// `.rs` (and other Cargo build inputs) are deliberately **ignored here**:
/// a source edit is handled by `umbral dev` rebuilding + restarting the
/// binary, which the browser picks up via the boot-id reconnect. Reacting
/// to `.rs` in the watcher would fire a premature reload against the
/// old/dying process before the new build is up. So watching `src` is
/// harmless but unnecessary — `main.rs` and every other `.rs` change
/// already reloads the page through the restart path.
fn classify(path: &Path) -> Option<bool> {
    let s = path.to_string_lossy();
    // Editor temp / VCS / build / IDE noise.
    if s.ends_with('~')
        || s.contains(".swp")
        || s.contains(".swx")
        || s.contains(".tmp")
        || s.contains("/.git/")
        || s.contains("/.hg/")
        || s.contains("/node_modules/")
        || s.contains("/target/")
        || s.contains("/.idea/")
        || s.contains("/.vscode/")
    {
        return None;
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("css") => Some(true),
        // Cargo build inputs/outputs — handled by the rebuild→restart path,
        // not by an in-process reload. Directory events (no extension) are
        // noise too.
        Some("rs") | Some("lock") | Some("rlib") | Some("rmeta") | Some("d") | None => None,
        // Everything else under a watched root → reload.
        _ => Some(false),
    }
}

fn spawn_watcher(dirs: Vec<PathBuf>, bus: broadcast::Sender<String>) {
    use notify::{EventKind, RecursiveMode, Watcher};

    // notify's callback runs on its own thread; `UnboundedSender::send` is
    // a sync method, so we can forward from there into an async debouncer.
    let (evt_tx, mut evt_rx) = tokio::sync::mpsc::unbounded_channel::<bool>();

    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if !matches!(
                event.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            ) {
                return;
            }
            for path in event.paths {
                if let Some(is_css) = classify(&path) {
                    let _ = evt_tx.send(is_css);
                }
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("livereload: watcher init failed: {e}; live-reload disabled");
                return;
            }
        };

    let mut watched_any = false;
    for path in &dirs {
        if !path.exists() {
            tracing::warn!("livereload: watch path {path:?} does not exist; skipping");
            continue;
        }
        // A directory is watched recursively; an individual file is watched
        // on its own (so users can pin a single config/content file).
        let mode = if path.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        match watcher.watch(path, mode) {
            Ok(()) => watched_any = true,
            Err(e) => tracing::warn!("livereload: cannot watch {path:?}: {e}"),
        }
    }
    if !watched_any {
        tracing::warn!("livereload: no watch paths exist ({dirs:?}); live-reload inactive");
        return;
    }
    let _ = WATCHER.set(std::sync::Mutex::new(watcher));

    // Debounce: a single save fans out into several fs events. Collect a
    // burst over a short window and emit ONE message — `reload` if any
    // non-CSS file changed, else `css`.
    tokio::spawn(async move {
        while let Some(first) = evt_rx.recv().await {
            let mut css_only = first;
            let deadline = tokio::time::sleep(Duration::from_millis(90));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    maybe = evt_rx.recv() => match maybe {
                        Some(is_css) => { if !is_css { css_only = false; } }
                        None => break,
                    }
                }
            }
            let msg = if css_only {
                r#"{"type":"css"}"#
            } else {
                r#"{"type":"reload"}"#
            };
            // Ignore send errors (no subscribers connected yet).
            let _ = bus.send(msg.to_string());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_before_closing_body() {
        let html = "<html><body><h1>hi</h1></body></html>";
        let out = inject_into_html(html);
        assert!(out.contains("data-umbral-livereload"), "snippet injected");
        let body_close = out.find("</body>").unwrap();
        let script = out.find("data-umbral-livereload").unwrap();
        assert!(script < body_close, "snippet sits before </body>");
        assert!(
            out.starts_with("<html><body><h1>hi</h1>"),
            "original content preserved"
        );
    }

    #[test]
    fn client_prefers_shared_worker_with_eventsource_fallback() {
        // The shared worker is the primary transport (one connection for all
        // tabs); a per-tab EventSource remains as the fallback for browsers
        // without SharedWorker. Both must be present and point at the right URLs.
        assert!(
            CLIENT_SNIPPET.contains("SharedWorker"),
            "client uses a SharedWorker so many tabs share one connection"
        );
        assert!(
            CLIENT_SNIPPET.contains(WORKER_PATH),
            "client loads the worker at WORKER_PATH ({WORKER_PATH})"
        );
        // The SharedWorker branch is tried first and returns before reaching it.
        let worker_at = CLIENT_SNIPPET.find("SharedWorker").unwrap();
        let fallback_at = CLIENT_SNIPPET.find("new EventSource").unwrap();
        assert!(
            worker_at < fallback_at,
            "SharedWorker is attempted before the EventSource fallback"
        );
    }

    #[test]
    fn worker_holds_a_single_eventsource_and_fans_out() {
        // The whole point: ONE EventSource regardless of tab count, broadcast
        // to every connected port, with new tabs synced to the current boot id.
        assert_eq!(
            WORKER_JS.matches("new EventSource").count(),
            1,
            "the worker opens exactly one EventSource for all tabs"
        );
        assert!(WORKER_JS.contains("onconnect"), "worker accepts tab ports");
        assert!(
            WORKER_JS.contains("broadcast"),
            "worker fans events out to every port"
        );
        assert!(
            WORKER_JS.contains("lastHello"),
            "worker syncs a newly-opened tab to the current boot id"
        );
        assert!(
            WORKER_JS.contains(SSE_PATH),
            "worker connects to the SSE endpoint ({SSE_PATH})"
        );
    }

    #[test]
    fn appends_when_no_body_tag() {
        let out = inject_into_html("<div>fragment</div>");
        assert!(out.starts_with("<div>fragment</div>"));
        assert!(out.trim_end().ends_with("</script>"));
    }

    #[test]
    fn classify_routes_css_vs_reload_vs_ignore() {
        // CSS hot-swaps; everything else under a watched root reloads.
        assert_eq!(classify(Path::new("static/css/app.css")), Some(true));
        assert_eq!(classify(Path::new("templates/home.html")), Some(false));
        assert_eq!(classify(Path::new("static/app.js")), Some(false));
        // Denylist: arbitrary content types reload too (non-standard layouts).
        assert_eq!(classify(Path::new("content/post.md")), Some(false));
        assert_eq!(classify(Path::new("site.config.toml")), Some(false));
        assert_eq!(classify(Path::new("data/seed.csv")), Some(false));
        // Ignored: source files (handled by the rebuild→restart path),
        // build inputs, temp/VCS/IDE noise, and bare directory events.
        assert_eq!(classify(Path::new("src/main.rs")), None);
        assert_eq!(classify(Path::new("Cargo.lock")), None);
        assert_eq!(classify(Path::new("templates/.home.html.swp")), None);
        assert_eq!(classify(Path::new("templates/home.html~")), None);
        assert_eq!(classify(Path::new("target/debug/foo")), None);
        assert_eq!(classify(Path::new("templates")), None); // dir, no ext
    }
}
