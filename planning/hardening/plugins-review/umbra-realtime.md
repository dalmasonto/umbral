# Review: umbra-realtime

Read-only audit, 2026-06-16. Scope: `plugins/umbra-realtime/src/` (lib.rs, sse.rs, ws.rs) and `tests/` (sse.rs, ws.rs, broker.rs, signals.rs). Cross-referenced against `planning/hardening/backlog.md`, `reviews/security.md`, and `reviews/performance-scalability.md`.

NET-NEW items only (not already filed). Cross-refs to existing numbered entries marked "already #".

---

## Verdict

**Substantially complete, shippable for single-instance.** SSE and WS transports are both real and production-shaped: group policy is enforced at handshake, identity is resolved from the session cookie, connections deregister on drop via `ConnGuard`/`WsGuard`, the `InProcessBroker` dispatches synchronously to the registry, and the `RedisBroker` multi-instance backplane is present but feature-gated. The signals bridge is wired and tested. No `todo!()`s or stub paths.

**Worst finding:** The `umbra-auth` hard dependency breaks the "REST-free app" contract — any app that installs `Realttime` must also pull in auth, even if it has no users. (See Finding RT-1.)

---

## Completeness

| Area | Status |
|---|---|
| SSE transport (push-only) | Complete. `GET /realtime/sse?groups=…` gate, register, stream, deregister on drop. |
| WS transport (bidirectional) | Complete. `GET /realtime/ws?groups=…` same gate, `MessageHandler` for inbound frames. |
| Channel/group model | Complete. `groups=public:plugin-{id}` form works; `GroupPolicy` is the auth seam. |
| Broadcast API | Complete. `Realtime::to_user`, `to_group`, `broadcast` all implemented. |
| Auth on subscriptions | Partial — see RT-1. Policy fires, but authn is hardwired to umbra-auth session cookie with no fallback for auth-free apps. |
| Reconnection / `Last-Event-ID` | Missing — see RT-2. No event IDs emitted; `Last-Event-ID` on reconnect is silently ignored. |
| Backpressure / connection limits | Partial — see RT-3. Per-connection buffer drop is fine; no aggregate connection cap. |
| Redis multi-instance backplane | Complete (feature-gated). `RedisBroker` with reconnect pump and cross-instance relay tested. |
| Signals bridge (`on_model` / `on_table`) | Complete. Wired and tested. |

---

## Findings

### RT-1 — `umbra-auth` is a **hard** dependency (NEW)

**Severity: Important**

`Cargo.toml:16` lists `umbra-auth = { path = "../umbra-auth", version = "0.0.1" }` as a non-optional dependency. Both SSE (`sse.rs:33`) and WS (`ws.rs:35`) call `umbra_auth::current_session_user_id(&headers).await` directly.

This means:
- Any app that installs `umbra-realtime` must compile `umbra-auth` even if the app has no authentication.
- An anonymous-only push use-case (a public event-feed, a status page) is architecturally impossible without dragging in the full auth stack.
- It violates the same "plugin→plugin hard dep" concern filed as already #76 (auth→rest boundary), now mirrored here in the realtime direction.

The `user_id` result is `Option<i64>` — the call returns `None` when auth is absent. The dependency could be flipped to a trait or made feature-optional.

**Fix:** Make `umbra-auth` an optional dependency (`optional = true`; feature `auth`). Define a trait seam — `trait IdentityResolver: Send + Sync { async fn user_id(&self, headers: &HeaderMap) -> Option<i64>; }` — in `umbra-core` or the facade. The default resolver calls `umbra_auth::current_session_user_id` (feature-gated); the no-auth resolver returns `None`. `RealtimePlugin` stores `Box<dyn IdentityResolver>`.

**Gap:** NEW — fold into the same spec as already #76 (lifting auth identity traits out of plugin-specific crates).

---

### RT-2 — No `Last-Event-ID` support; reconnects lose events (NEW)

**Severity: Important**

The SSE stream (`sse.rs:107`) emits `SseEvent::default().event(...).data(...)` with no `.id(...)` field. The browser's `EventSource` sends a `Last-Event-ID` header on reconnect, but `sse_handler` ignores it (`sse.rs:32` — the function signature is `headers: HeaderMap, Query(q): Query<SseQuery>` and does not inspect `Last-Event-ID`).

The `scaling.mdx` doc is honest about this ("a message published while an instance is briefly disconnected is not replayed"), but the SSE spec says emitting IDs lets the browser ask the server for missed events, and most production realtime systems use this to catch up after a momentary disconnect. The current behaviour silently drops every event published during a reconnect window.

**Fix:**
1. Emit a monotonic `id` on every SSE frame (`SseEvent::default().id(seq.to_string()).event(…).data(…)`). The `ConnEntry` carries the current sequence; `dispatch` increments and embeds it.
2. On reconnect, inspect `Last-Event-ID` in `sse_handler`; if present, replay buffered events from that sequence (a per-connection ring buffer or a shared rolling log keyed by sequence).

A minimal first step is step 1 only (emit IDs) so the browser always knows the last received sequence and the infrastructure for catch-up is in place.

**Gap:** NEW.

---

### RT-3 — No aggregate connection limit (NEW)

**Severity: Important**

`Registry::register` (`lib.rs:122-146`) accepts every connection unconditionally. A browser that opens dozens of tabs, a DoS actor, or a client that reconnects in a tight loop on auth failure will grow `RegistryInner.conns` without bound. Each entry holds an `mpsc::Sender` (and participates in every `Broadcast` dispatch), so the registry is both a memory leak vector and a broadcast amplifier.

The per-connection `DEFAULT_BUFFER = 64` correctly prevents back-pressure onto the sender, but a full buffer just drops events — it does not close the connection.

**Fix:** Add an optional `max_connections: Option<usize>` to `RealtimePlugin` (defaulting to `None` = unlimited, to preserve zero-config behaviour). When set, `Registry::register` returns `Err` when the cap is hit; the transports convert this to a `503 Service Unavailable` or `1008 Policy Violation` WS close. Log at WARN with the current count.

**Gap:** NEW.

---

### RT-4 — `GroupPolicy::can_join` is sync but may need async DB lookups (NEW)

**Severity: Optional**

`GroupPolicy` is a `fn can_join(&self, user_id: Option<i64>, group: &str) -> bool` (synchronous). Any real app that gates room access on a membership table (`SELECT ... WHERE user_id = ? AND room_id = ?`) cannot implement this without either blocking the async runtime or caching the entire membership table in memory.

The `sse.rs` and `ws.rs` callers already `await` the identity resolution; the policy call at `sse.rs:49` / `ws.rs:48` is in an async context, so upgrading the trait to `async fn can_join(...)` would be a one-line change at the trait site and a small but backward-incompatible API change (all existing `impl GroupPolicy` need `async fn`).

**Fix:** Add `async fn can_join_async` with a blanket default calling `can_join` synchronously, or convert the trait to fully async. Ship the async variant now before the API surface stabilizes.

**Gap:** NEW.

---

### RT-5 — `groups=public:plugin-{id}` — unbounded group namespace (NEW)

**Severity: Optional**

A client that connects with `?groups=public:plugin-1,public:plugin-2,...,public:plugin-10000` will:
1. Have each group validated by the `PublicGroupsOnly` policy (which passes — all start with `public:`).
2. Register all 10,000 group memberships in `RegistryInner.by_group`.

The default `GroupPolicy::can_join` has no cardinality check. This is a modest DoS amplifier because the dispatch path iterates `by_group[group]` per event, and an authenticated user with a crafted group list could create O(n) registry entries per connection.

**Fix:** Add a `max_groups_per_connection: usize` guard in the transport (before `register`) with a hardcoded conservative default (e.g. 32) and a builder setter on `RealtimePlugin`.

**Gap:** NEW.

---

### RT-6 — Stray MDX tool-gen artifacts in realtime docs (already in backlog.md P0 docs)

**Severity: Important** (already noted in backlog.md, not a new finding)

`realtime/sse.mdx:139-141` and `realtime/scaling.mdx:72-73` both end with `</content>\n</invoke>` tool-generated artifacts that break MDX parsing. **Already flagged in backlog.md as a P0 doc fix.** Not a new finding, cited here for completeness.

---

### RT-7 — `dispatch` collects `ConnId` vec before dispatching (correctness FYI)

**Severity: Nit**

`Registry::dispatch` (`lib.rs:203-224`) acquires a read lock, collects all matching `ConnId`s into a `Vec`, drops the lock, then iterates the vec and re-acquires the read lock per iteration (`inner.conns.get(&id)`). This is correct (avoids holding the write lock during channel sends) but the re-acquisition means the lock is taken O(n) times per dispatch. On a Broadcast to many connections this is O(connections × lock). Not a correctness bug, just a style note; `DashMap` or a different dispatch pattern would be cleaner at scale.

**Gap:** None required — FYI only.

---

## Plugin-contract

- **Facade-only imports:** PARTIAL. `plugins/umbra-realtime/src/lib.rs:37` imports `umbra::plugin::{AppContext, Plugin, PluginError}` and `umbra::signals::subscribe_async` — all through the facade. The `umbra::orm::Model` bound on `on_model` uses `umbra::orm`, also through the facade. **However** `sse.rs:33` and `ws.rs:35` call `umbra_auth::current_session_user_id` directly — the hard plugin-to-plugin dep noted in RT-1.
- **Migrations:** None registered. Realtime has no persisted schema — correct.
- **`Plugin` impl:** Clean. `name()`, `routes()`, and `on_ready()` are all present and correct. No `migrations()` or `commands()` needed.

---

## Tests

| Test | File | Covers |
|---|---|---|
| Registry unit tests | `lib.rs:770-927` | to_user, to_group, broadcast, deregister, join/leave, InProcessBroker |
| SSE integration | `tests/sse.rs` | Group gate (403 vs 200), registration count, event delivery round-trip |
| WS integration | `tests/ws.rs` | Group gate, push (server→client), inbound (client→MessageHandler), real TCP bind |
| Broker / envelope | `tests/broker.rs` | JSON round-trip for all TargetKind variants; Redis relay (REDIS_URL gated) |
| Signals bridge | `tests/signals.rs` | `on_table` fans out `post_save`/`post_delete`, ignores other tables |

**Gaps:**
- No test for `Last-Event-ID` / reconnection (RT-2 is untested by design).
- No test for group cardinality DoS (RT-5).
- No test for the `redis_url` set + feature-off warning path.
- No test for `ConnGuard` / `WsGuard` actually cleaning up the registry on disconnect (the SSE test drops the response but does not poll until connection_count drops to 0 — relies on tokio task scheduling).
