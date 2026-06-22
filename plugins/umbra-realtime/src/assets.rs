//! Browser-side realtime assets: a `SharedWorker` that holds exactly ONE
//! `EventSource` per browser — over the set-union of every connected tab's
//! groups — and a small client helper (`umbra.realtime.subscribe`) that
//! prefers the worker and falls back to a per-tab `EventSource`.
//!
//! Why: every tab opening its own `EventSource('/realtime/sse?...')` burns one
//! of the browser's ~6-per-host HTTP/1.1 connections. Open the same realtime
//! page in a handful of tabs and the budget is gone — later requests hang as
//! if the server were down. Routing every tab through a single `SharedWorker`
//! that holds ONE connection over the union of all subscriptions collapses N
//! tabs of ANY mix of subscriptions into 1 server connection — "open unlimited
//! tabs, it just works." This is possible because the server now ships every
//! event under a single `u` type with a channel-tagged envelope `{c,e,d}`, so
//! the one union connection can route each event to the right tabs by channel.

/// The `SharedWorker` body. One worker per origin holds ONE
/// `EventSource('/realtime/sse?groups=<sorted union>')` over the set-union of
/// every connected port's groups. Each port records its own groups + wanted
/// event names; the worker listens once for the single `"u"` event, parses the
/// `{c,e,d}` envelope, and routes each event to a port iff that port is
/// interested in channel `c` (it's one of the port's groups, or `c` is a
/// `@broadcast` / `@user:` same-session channel). When the union changes (a
/// port subscribes/unsubscribes a new/last group) the single `EventSource`
/// reconnects with the new union; `Last-Event-ID` fills the brief gap. When no
/// ports remain the connection is closed so idle streams don't leak.
pub const REALTIME_WORKER_JS: &str = r#"// umbra realtime shared worker — ONE EventSource per browser over the union of all groups.
"use strict";
// ports: [{ port, groups: Set<string>, names: Set<string> }]
var ports = [];
var es = null;        // the single shared EventSource
var currentUnion = ""; // the sorted-comma key the current es is connected with

function splitGroups(groups) {
  return String(groups || "")
    .split(",")
    .map(function (s) { return s.trim(); })
    .filter(function (s) { return s.length; });
}

// The sorted-comma set-union of every connected port's groups.
function unionKey() {
  var set = new Set();
  ports.forEach(function (p) {
    p.groups.forEach(function (g) { set.add(g); });
  });
  return Array.from(set).sort().join(",");
}

// A port is interested in channel `c` if it subscribed to that group, or if
// `c` is a same-session @broadcast / @user: channel (those reach every tab).
function portInterested(p, c) {
  if (c === "@broadcast") return true;
  if (c.indexOf("@user:") === 0) return true;
  return p.groups.has(c);
}

function route(c, e, d, id) {
  ports = ports.filter(function (p) {
    if (!portInterested(p, c)) return true; // keep, just not interested
    try {
      p.port.postMessage({ channel: c, event: e, data: d, id: id });
      return true;
    } catch (_) {
      return false; // dead port — prune it
    }
  });
  reconcile();
}

// Hold exactly ONE EventSource over the current union. Reconnect only when the
// union actually changes (EventSource resends Last-Event-ID, so the replay
// buffer fills the brief gap); do nothing when it's unchanged.
function reconcile() {
  var union = unionKey();
  if (ports.length === 0) {
    if (es) { try { es.close(); } catch (_) {} es = null; currentUnion = ""; }
    return;
  }
  if (es && union === currentUnion) return; // no change — no needless reconnect
  if (es) { try { es.close(); } catch (_) {} es = null; }
  currentUnion = union;
  es = new EventSource("/realtime/sse?groups=" + encodeURIComponent(union));
  // Single enveloped event type: one listener catches everything.
  es.addEventListener("u", function (ev) {
    var env;
    try { env = JSON.parse(ev.data); } catch (_) { return; }
    if (!env) return;
    route(env.c, env.e, env.d, ev.lastEventId);
  });
}

function findPort(port) {
  for (var i = 0; i < ports.length; i++) {
    if (ports[i].port === port) return ports[i];
  }
  return null;
}

function subscribePort(port, groups, events) {
  var entry = findPort(port);
  if (!entry) { entry = { port: port, groups: new Set(), names: new Set() }; ports.push(entry); }
  splitGroups(groups).forEach(function (g) { entry.groups.add(g); });
  (events || []).forEach(function (name) { entry.names.add(name); });
  reconcile();
}

function unsubscribePort(port, groups) {
  var entry = findPort(port);
  if (!entry) return;
  splitGroups(groups).forEach(function (g) { entry.groups.delete(g); });
  reconcile();
}

function dropPort(port) {
  ports = ports.filter(function (e) { return e.port !== port; });
  reconcile();
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
/// `umbra.realtime.subscribe(groups, handlers)` (signature unchanged):
///
/// - **Preferred**: a `SharedWorker('/realtime/worker.js', 'umbra-realtime')`
///   so every tab — regardless of which mix of subscriptions it holds —
///   shares ONE server connection over the union of all subscriptions.
/// - **Fallback** (no `SharedWorker`, or its constructor throws under CSP): a
///   per-tab `EventSource('/realtime/sse?groups=…')` that listens for the
///   single `"u"` event and unwraps the `{c,e,d}` envelope — same observable
///   behavior, just not shared across tabs.
/// - **Degrade**: if `EventSource` is also missing, `subscribe` is a no-op
///   returning a no-op `unsubscribe`.
///
/// Routing is by channel: a routed event reaches a handler iff its channel `c`
/// is one of THIS subscription's groups, or `c` is a `@broadcast` / `@user:`
/// same-session channel.
pub const REALTIME_CLIENT_JS: &str = r#"// umbra realtime client — share ONE SSE connection across tabs (union routing).
"use strict";
(function () {
  window.umbra = window.umbra || {};
  if (window.umbra.realtime && window.umbra.realtime.subscribe) return;

  function noop() {}
  function noSub() { return { unsubscribe: noop }; }

  function splitGroups(groups) {
    return String(groups || "")
      .split(",")
      .map(function (s) { return s.trim(); })
      .filter(function (s) { return s.length; });
  }

  // A subscription is interested in channel `c` if it's one of its groups, or
  // `c` is a same-session @broadcast / @user: channel (those reach every tab).
  function interested(mine, c) {
    if (c === "@broadcast") return true;
    if (c.indexOf("@user:") === 0) return true;
    return mine.indexOf(c) !== -1;
  }

  // Dispatch a routed event to its handler. `data` may arrive as a JSON string
  // (raw SSE) or an already-parsed value (worker post); handle both.
  function call(handlers, name, data, channel, id) {
    var fn = handlers[name];
    if (!fn) return;
    var parsed = data;
    if (typeof data === "string") {
      try { parsed = JSON.parse(data); } catch (_) { parsed = data; }
    }
    try { fn(parsed, { channel: channel, id: id }); } catch (_) {}
  }

  function viaWorker(groups, handlers, names) {
    var mine = splitGroups(groups);
    var worker = new SharedWorker("/realtime/worker.js", "umbra-realtime");
    var port = worker.port;
    port.start();
    port.onmessage = function (ev) {
      var msg = ev.data || {};
      // The worker routes by union; re-check the channel matches THIS
      // subscription (a different tab's group may share the union connection).
      if (!interested(mine, msg.channel)) return;
      if (handlers[msg.event] !== undefined) {
        call(handlers, msg.event, msg.data, msg.channel, msg.id);
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

  function viaEventSource(groups, handlers) {
    var mine = splitGroups(groups);
    var es = new EventSource("/realtime/sse?groups=" + groups);
    // Single enveloped event type: listen once, unwrap {c,e,d}, route by `c`.
    es.addEventListener("u", function (e) {
      var env;
      try { env = JSON.parse(e.data); } catch (_) { return; }
      if (!env || !interested(mine, env.c)) return;
      if (handlers[env.e] !== undefined) {
        call(handlers, env.e, env.d, env.c, e.lastEventId);
      }
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
      // Preferred: ONE shared connection over the union of all tabs' groups.
      if ("SharedWorker" in window) {
        try { return viaWorker(groups, handlers, names); }
        catch (_) { /* CSP / construction failure → per-tab fallback */ }
      }
      // Fallback: a per-tab EventSource (no SharedWorker, or it threw).
      if ("EventSource" in window) {
        try { return viaEventSource(groups, handlers); }
        catch (_) { return noSub(); }
      }
      // Degrade silently — same as the legacy raw-EventSource consumer did.
      return noSub();
    },

    // model(name, { created: fn(row), updated: fn(row), deleted: fn(row) }, { group })
    // → { unsubscribe: fn }
    //
    // Sugar over subscribe(): the server (RealtimePlugin::expose) sends the
    // action as the event name ("created"/"updated"/"deleted") to the group
    // you exposed to. `group` is REQUIRED — name the group you exposed; there
    // is no magic default. `name` is the model label, for readability only.
    model: function (name, handlers, opts) {
      opts = opts || {};
      handlers = handlers || {};
      var group = opts.group;
      if (!group) {
        if (window.console && console.warn) {
          console.warn("umbra.realtime.model('" + name + "', …): opts.group is required");
        }
        return noSub();
      }
      var routes = {};
      if (handlers.created) routes.created = handlers.created;
      if (handlers.updated) routes.updated = handlers.updated;
      if (handlers.deleted) routes.deleted = handlers.deleted;
      return window.umbra.realtime.subscribe(group, routes);
    },

    // presence(group, { sync: fn(members), join: fn(member), leave: fn(member) })
    // → { unsubscribe: fn }
    //
    // Sugar over subscribe(): the server (RealtimePlugin::with_presence) sends
    // "presence:sync" (the full [{id,...}] member list), "presence:join" (one
    // member entered) and "presence:leave" (one left) to a presence-enabled
    // group. Presence is OFF unless the server opts the group in, and a member
    // is only its id (or whatever the server's resolver returns).
    presence: function (group, handlers) {
      handlers = handlers || {};
      var routes = {};
      if (handlers.sync) routes["presence:sync"] = handlers.sync;
      if (handlers.join) routes["presence:join"] = handlers.join;
      if (handlers.leave) routes["presence:leave"] = handlers.leave;
      return window.umbra.realtime.subscribe(group, routes);
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
    fn worker_opens_one_eventsource_over_the_union() {
        // The whole point: exactly ONE EventSource per browser, built from the
        // sorted set-union of every port's groups — never one-per-key/per-tab.
        assert_eq!(
            REALTIME_WORKER_JS.matches("new EventSource").count(),
            1,
            "a single EventSource creation site for the whole browser"
        );
        assert!(
            REALTIME_WORKER_JS.contains("new EventSource(\"/realtime/sse?groups=\" + encodeURIComponent(union))"),
            "the one EventSource is parameterized by the union, not a per-tab key"
        );
        assert!(
            REALTIME_WORKER_JS.contains("function unionKey"),
            "computes the set-union of every port's groups"
        );
    }

    #[test]
    fn worker_reconnects_when_the_union_changes() {
        assert!(
            REALTIME_WORKER_JS.contains("union === currentUnion"),
            "no needless reconnect when the union is unchanged"
        );
        assert!(
            REALTIME_WORKER_JS.contains("function reconcile"),
            "reconnects the single EventSource when the union changes"
        );
    }

    #[test]
    fn worker_has_onconnect_and_sorts_the_union() {
        assert!(REALTIME_WORKER_JS.contains("self.onconnect"), "SharedWorker entry point");
        assert!(
            REALTIME_WORKER_JS.contains(".sort()"),
            "normalizes the union key by sorting so a,b == b,a"
        );
    }

    #[test]
    fn worker_listens_for_single_u_event_and_routes_by_channel() {
        assert!(
            REALTIME_WORKER_JS.contains("addEventListener(\"u\""),
            "listens once for the single enveloped `u` event"
        );
        assert!(
            REALTIME_WORKER_JS.contains("JSON.parse(ev.data)"),
            "parses the {{c,e,d}} envelope"
        );
        // Routing handles @broadcast and @user: same-session channels.
        assert!(REALTIME_WORKER_JS.contains("\"@broadcast\""), "routes @broadcast to every tab");
        assert!(REALTIME_WORKER_JS.contains("\"@user:\""), "routes @user: to every same-session tab");
        assert!(
            REALTIME_WORKER_JS.contains("p.groups.has(c)"),
            "routes a group channel only to ports subscribed to it"
        );
    }

    #[test]
    fn worker_closes_eventsource_when_no_ports_left() {
        assert!(
            REALTIME_WORKER_JS.contains("ports.length === 0"),
            "detects when no ports remain"
        );
        assert!(
            REALTIME_WORKER_JS.contains("es.close()"),
            "closes the single EventSource when no ports remain"
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

    #[test]
    fn client_defines_model_sugar_delegating_to_subscribe() {
        assert!(
            REALTIME_CLIENT_JS.contains("model:"),
            "defines umbra.realtime.model"
        );
        // It routes the action event-names a server `expose` sends.
        for action in ["created", "updated", "deleted"] {
            assert!(
                REALTIME_CLIENT_JS.contains(&format!("handlers.{action}")),
                "model() wires the {action} handler"
            );
        }
        // The sugar is defined AFTER subscribe and delegates to it (no second
        // transport path of its own).
        let subscribe_at = REALTIME_CLIENT_JS.find("subscribe: function").unwrap();
        let model_at = REALTIME_CLIENT_JS.find("model: function").unwrap();
        assert!(
            subscribe_at < model_at,
            "model() is defined after subscribe()"
        );
        assert!(
            REALTIME_CLIENT_JS.contains("window.umbra.realtime.subscribe(group, routes)"),
            "model() delegates to subscribe() rather than opening its own connection"
        );
        // `group` is required — no magic default.
        assert!(
            REALTIME_CLIENT_JS.contains("opts.group"),
            "model() reads the required group from opts"
        );
    }

    #[test]
    fn client_routes_by_channel_and_unwraps_envelope() {
        // Matches routed events by channel against this subscription's groups
        // (plus @broadcast / @user:).
        assert!(
            REALTIME_CLIENT_JS.contains("function interested"),
            "routes by channel match"
        );
        assert!(REALTIME_CLIENT_JS.contains("\"@broadcast\""), "delivers @broadcast");
        assert!(REALTIME_CLIENT_JS.contains("@user:"), "delivers @user: same-session events");
        // Fallback listens for the single `u` event and unwraps {c,e,d}.
        assert!(
            REALTIME_CLIENT_JS.contains("addEventListener(\"u\""),
            "fallback listens for the single `u` event"
        );
        assert!(REALTIME_CLIENT_JS.contains("env.c"), "fallback unwraps the channel `c`");
        assert!(REALTIME_CLIENT_JS.contains("env.e"), "fallback unwraps the event name `e`");
        assert!(REALTIME_CLIENT_JS.contains("env.d"), "fallback unwraps the data `d`");
    }

    #[test]
    fn client_defines_presence_sugar_delegating_to_subscribe() {
        assert!(
            REALTIME_CLIENT_JS.contains("presence:"),
            "defines umbra.realtime.presence"
        );
        // It wires the three presence event-names the server `with_presence` sends.
        for ev in ["presence:sync", "presence:join", "presence:leave"] {
            assert!(
                REALTIME_CLIENT_JS.contains(&format!("\"{ev}\"")),
                "presence() wires the {ev} route"
            );
        }
        // The sugar reads sync/join/leave handlers and delegates to subscribe()
        // (no transport of its own).
        assert!(REALTIME_CLIENT_JS.contains("handlers.sync"), "reads the sync handler");
        assert!(REALTIME_CLIENT_JS.contains("handlers.join"), "reads the join handler");
        assert!(REALTIME_CLIENT_JS.contains("handlers.leave"), "reads the leave handler");
        let subscribe_at = REALTIME_CLIENT_JS.find("subscribe: function").unwrap();
        let presence_at = REALTIME_CLIENT_JS.find("presence: function").unwrap();
        assert!(
            subscribe_at < presence_at,
            "presence() is defined after subscribe()"
        );
        // It delegates to subscribe() rather than opening its own connection.
        let delegations = REALTIME_CLIENT_JS
            .matches("window.umbra.realtime.subscribe(group, routes)")
            .count();
        assert!(
            delegations >= 2,
            "both model() and presence() delegate to subscribe(group, routes)"
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
