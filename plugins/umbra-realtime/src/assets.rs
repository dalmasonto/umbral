//! Browser-side realtime assets: a `SharedWorker` that holds ONE
//! `EventSource` per distinct subscription and a small client helper
//! (`umbra.realtime.subscribe`) that prefers the worker and falls back to a
//! per-tab `EventSource`.
//!
//! Why: every tab opening its own `EventSource('/realtime/sse?...')` burns one
//! of the browser's ~6-per-host HTTP/1.1 connections. Open the same realtime
//! page in a handful of tabs and the budget is gone — later requests hang as
//! if the server were down. Routing every tab's subscription through a single
//! `SharedWorker` collapses N tabs of the same subscription into 1 server
//! connection. This mirrors the livereload plugin's worker exactly.

/// The `SharedWorker` body. One worker per origin keeps, **per distinct
/// normalized `groups` key**, a single `EventSource('/realtime/sse?groups=…')`
/// shared by every tab subscribed to that key. Each tab port records which
/// event-names it wants; the worker `addEventListener`s once per name and fans
/// each frame only to the ports that asked for that key+name. When a key has
/// no ports left, its `EventSource` is closed so idle streams don't leak.
pub const REALTIME_WORKER_JS: &str = r#"// umbra realtime shared worker — one EventSource per distinct subscription.
"use strict";
// key -> { es: EventSource, listening: Set<eventName>, ports: [{port, names:Set}] }
var subs = {};

function normalizeKey(groups) {
  return String(groups || "")
    .split(",")
    .map(function (s) { return s.trim(); })
    .filter(function (s) { return s.length; })
    .sort()
    .join(",");
}

function fanout(key, name, data, id) {
  var sub = subs[key];
  if (!sub) return;
  sub.ports = sub.ports.filter(function (entry) {
    if (!entry.names.has(name)) return true; // keep, just not interested
    try {
      entry.port.postMessage({ key: key, event: name, data: data, id: id });
      return true;
    } catch (_) {
      return false; // dead port — prune it
    }
  });
  cleanupIfEmpty(key);
}

function listen(key, name) {
  var sub = subs[key];
  if (!sub || sub.listening.has(name)) return;
  sub.listening.add(name);
  sub.es.addEventListener(name, function (e) {
    fanout(key, name, e.data, e.lastEventId);
  });
}

function ensureSub(key) {
  if (subs[key]) return subs[key];
  var es = new EventSource("/realtime/sse?groups=" + encodeURIComponent(key));
  subs[key] = { es: es, listening: new Set(), ports: [] };
  return subs[key];
}

function cleanupIfEmpty(key) {
  var sub = subs[key];
  if (sub && sub.ports.length === 0) {
    try { sub.es.close(); } catch (_) {}
    delete subs[key];
  }
}

function subscribePort(port, groups, events) {
  var key = normalizeKey(groups);
  if (!key) return;
  var sub = ensureSub(key);
  var entry = null;
  for (var i = 0; i < sub.ports.length; i++) {
    if (sub.ports[i].port === port) { entry = sub.ports[i]; break; }
  }
  if (!entry) { entry = { port: port, names: new Set() }; sub.ports.push(entry); }
  (events || []).forEach(function (name) {
    entry.names.add(name);
    listen(key, name);
  });
}

function unsubscribePort(port, groups) {
  var key = normalizeKey(groups);
  if (!subs[key]) return;
  subs[key].ports = subs[key].ports.filter(function (e) { return e.port !== port; });
  cleanupIfEmpty(key);
}

function dropPort(port) {
  Object.keys(subs).forEach(function (key) {
    subs[key].ports = subs[key].ports.filter(function (e) { return e.port !== port; });
    cleanupIfEmpty(key);
  });
}

self.onconnect = function (e) {
  var port = e.ports[0];
  port.start();
  port.onmessage = function (ev) {
    var msg = ev.data || {};
    if (msg === "bye") { dropPort(port); return; }
    if (msg.subscribe) {
      subscribePort(port, msg.subscribe.groups, msg.subscribe.events);
    } else if (msg.unsubscribe) {
      unsubscribePort(port, msg.unsubscribe.groups);
    } else if (msg.bye) {
      dropPort(port);
    }
  };
};
"#;

/// The client helper, served at `GET /realtime/client.js`. Exposes
/// `umbra.realtime.subscribe(groups, handlers)`:
///
/// - **Preferred**: a `SharedWorker('/realtime/worker.js', 'umbra-realtime')`
///   so every tab with the same subscription shares ONE server connection.
/// - **Fallback** (no `SharedWorker`, or its constructor throws under CSP): a
///   per-tab `EventSource('/realtime/sse?groups=…')` — same observable
///   behavior, just not shared across tabs.
/// - **Degrade**: if `EventSource` is also missing, `subscribe` is a no-op
///   returning a no-op `unsubscribe`.
pub const REALTIME_CLIENT_JS: &str = r#"// umbra realtime client — share one SSE connection across tabs.
"use strict";
(function () {
  window.umbra = window.umbra || {};
  if (window.umbra.realtime && window.umbra.realtime.subscribe) return;

  function noop() {}
  function noSub() { return { unsubscribe: noop }; }

  function call(handlers, name, data, id) {
    var fn = handlers[name];
    if (!fn) return;
    var parsed;
    try { parsed = JSON.parse(data); } catch (_) { parsed = data; }
    try { fn(parsed, { id: id }); } catch (_) {}
  }

  function viaWorker(groups, handlers, names) {
    var worker = new SharedWorker("/realtime/worker.js", "umbra-realtime");
    var port = worker.port;
    port.start();
    port.onmessage = function (ev) {
      var msg = ev.data || {};
      if (handlers[msg.event] !== undefined) {
        call(handlers, msg.event, msg.data, msg.id);
      }
    };
    port.postMessage({ subscribe: { groups: groups, events: names } });
    var done = false;
    function unsubscribe() {
      if (done) return; done = true;
      try { port.postMessage({ unsubscribe: { groups: groups } }); } catch (_) {}
      try { port.postMessage("bye"); } catch (_) {}
    }
    window.addEventListener("beforeunload", function () {
      try { port.postMessage("bye"); } catch (_) {}
    });
    return { unsubscribe: unsubscribe };
  }

  function viaEventSource(groups, handlers, names) {
    var es = new EventSource("/realtime/sse?groups=" + groups);
    names.forEach(function (name) {
      es.addEventListener(name, function (e) {
        call(handlers, name, e.data, e.lastEventId);
      });
    });
    var done = false;
    function unsubscribe() {
      if (done) return; done = true;
      try { es.close(); } catch (_) {}
    }
    window.addEventListener("beforeunload", function () {
      try { es.close(); } catch (_) {}
    });
    return { unsubscribe: unsubscribe };
  }

  window.umbra.realtime = {
    // subscribe(groups, { eventName: function (data, rawEvent) { ... }, ... })
    // → { unsubscribe: fn }
    subscribe: function (groups, handlers) {
      handlers = handlers || {};
      var names = Object.keys(handlers);
      // Preferred: one shared connection per subscription across all tabs.
      if ("SharedWorker" in window) {
        try { return viaWorker(groups, handlers, names); }
        catch (_) { /* CSP / construction failure → per-tab fallback */ }
      }
      // Fallback: a per-tab EventSource (no SharedWorker, or it threw).
      if ("EventSource" in window) {
        try { return viaEventSource(groups, handlers, names); }
        catch (_) { return noSub(); }
      }
      // Degrade silently — same as the legacy raw-EventSource consumer did.
      return noSub();
    }
  };
})();
"#;

/// `GET /realtime/worker.js` — the `SharedWorker` script.
pub async fn worker_js_handler() -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        REALTIME_WORKER_JS,
    )
}

/// `GET /realtime/client.js` — the `umbra.realtime.subscribe` client helper.
pub async fn client_js_handler() -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        REALTIME_CLIENT_JS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_opens_one_eventsource_per_key() {
        // The whole point: the EventSource is parameterized by the normalized
        // groups key (created once in ensureSub, keyed in `subs`), never
        // duplicated per tab.
        assert!(
            REALTIME_WORKER_JS.contains("new EventSource(\"/realtime/sse?groups=\" + encodeURIComponent(key))"),
            "the worker opens one EventSource per groups key"
        );
        // Exactly one `new EventSource` call site — one per key, not per port.
        assert_eq!(
            REALTIME_WORKER_JS.matches("new EventSource").count(),
            1,
            "a single EventSource creation site, keyed by the groups key"
        );
        assert!(
            REALTIME_WORKER_JS.contains("subs[key]"),
            "EventSources are held in a per-key map"
        );
    }

    #[test]
    fn worker_has_onconnect_and_normalizes_key() {
        assert!(REALTIME_WORKER_JS.contains("self.onconnect"), "SharedWorker entry point");
        assert!(
            REALTIME_WORKER_JS.contains(".sort()"),
            "normalizes the groups key by sorting so a,b == b,a"
        );
        assert!(REALTIME_WORKER_JS.contains("normalizeKey"), "has a key normalizer");
    }

    #[test]
    fn worker_listens_per_event_name() {
        assert!(
            REALTIME_WORKER_JS.contains("sub.es.addEventListener(name"),
            "adds a listener per requested event name"
        );
    }

    #[test]
    fn worker_closes_eventsource_when_no_ports_left() {
        assert!(
            REALTIME_WORKER_JS.contains("cleanupIfEmpty"),
            "has a cleanup path for keys with no ports"
        );
        assert!(
            REALTIME_WORKER_JS.contains("sub.es.close()"),
            "closes the EventSource when a key has no ports left"
        );
    }

    #[test]
    fn client_defines_subscribe_and_prefers_worker_over_fallback() {
        assert!(
            REALTIME_CLIENT_JS.contains("umbra.realtime"),
            "exposes the umbra.realtime namespace"
        );
        assert!(
            REALTIME_CLIENT_JS.contains("subscribe:"),
            "defines umbra.realtime.subscribe"
        );
        assert!(REALTIME_CLIENT_JS.contains("SharedWorker"), "prefers a SharedWorker");
        assert!(
            REALTIME_CLIENT_JS.contains("new EventSource"),
            "has a per-tab EventSource fallback"
        );
        // SharedWorker is attempted before the EventSource fallback.
        let worker_at = REALTIME_CLIENT_JS.find("SharedWorker").unwrap();
        let fallback_at = REALTIME_CLIENT_JS.find("new EventSource").unwrap();
        assert!(
            worker_at < fallback_at,
            "SharedWorker is attempted before the EventSource fallback"
        );
    }

    #[tokio::test]
    async fn handlers_serve_javascript() {
        use axum::body::to_bytes;
        use axum::response::IntoResponse;

        let res = client_js_handler().await.into_response();
        let ct = res
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(ct, "application/javascript; charset=utf-8");
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("umbra.realtime"));

        let res = worker_js_handler().await.into_response();
        let ct = res
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(ct, "application/javascript; charset=utf-8");
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("self.onconnect"));
    }
}
