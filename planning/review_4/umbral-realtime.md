# review_4 · umbral-realtime

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 6,104 LOC (`plugins/umbral-realtime`) |
| **Reviewer** | Claude (rev4 wave) |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 1 high · 1 medium · 3 low · 1 info — 6 new, 2 known |

## Summary

- `plugins/umbral-realtime` is real-time push for umbral: SSE (`GET {base}/sse`) plus WebSocket (`GET {base}/ws`) over one shared `Registry` (per-user / per-group / per-connection indexes), a single `authorize_handshake` covering identity → group policy → aggregate connection cap → presence, a pluggable `Broker` (in-process default, Redis pub/sub backplane behind the `redis` feature), replay with `Last-Event-ID` resume (bounded, default 1024), ephemeral presence (off by default), `expose::<T>` model-change broadcasts, and served SharedWorker/client JS. 6,104 LOC including 21 test files (~2.0k LOC) — reconnect/replay, 10k-connection scale, WS origin, WS limits, publish authz, presence scope, async policy, expose suites.
- Overall state is strong, and the prior sweeps' fixes verified held in code: both transports call the same `authorize_handshake` (review_3 dedup), the Redis handoff is bounded (`QUEUE_CAP 4096` + `try_send` drop-newest + warn — review_3 codex #12), the Redis URL is redacted at the boot log, and the safe caps ship as defaults (`DEFAULT_MAX_CONNECTIONS = 10_000`, 1 MiB frames, 100 text msgs/s — audit_2 #4, whose attached stamp still says "partial" but all three sub-parts are now present).
- Biggest risk (RT-1, HIGH): a WebSocket upgrade that fails *after* the handshake registered the connection leaks the registration — and the presence JOIN — permanently. Registration happens before the upgrade by design; `WsGuard`, the only deregistration path, is created only inside the post-upgrade callback; and neither transport installs axum's `on_failed_upgrade` hook (the default is a no-op). The leak is unauthenticated (default `PublicGroupsOnly` admits any `public:*` name), accumulates against the deliberately finite 10k default cap, and has no reaper: roughly 10k aborted handshakes turn every subsequent connection — legitimate or not — into `503 realtime: connection limit reached` until restart.
- RT-2 (MEDIUM): the Redis backplane publishes on a fixed global channel (`"umbral:realtime:events"`) with no per-app namespace, and inbound envelopes from that channel are dispatched locally with no origin check. Two umbral apps sharing one Redis DB — a deployment `umbral-cache` explicitly supports and namespaces its keys for — cross-deliver and cross-inject each other's events, including `to_user` traffic.
- Fix order: RT-1 (attach failed-upgrade cleanup, or register post-upgrade), then RT-2 (channel prefix + a "dedicated DB or unique prefix" doc line), then the three small ones — two are one-line fixes in the served client JS, one is a swallowed serialization error.

## Findings

### RT-1 · HIGH · correctness/security · self-verified
**A WebSocket handshake that registers a connection (consuming a slot from the aggregate cap, and broadcasting presence JOIN) but whose upgrade then fails leaves a zombie registration — and presence ghost — that nothing ever cleans up, because `WsGuard` is constructed only inside the post-upgrade callback and `on_failed_upgrade` is left at its no-op default.**

- **Where**: `plugins/umbral-realtime/src/ws.rs:143-163` (register, then `on_upgrade`), `:176-179` (guard created only inside `handle_socket`), `:246-255` (guard drop = the only deregistration); `plugins/umbral-realtime/src/lib.rs:1528-1542` (`authorize_handshake` registers + spawns presence); axum 0.8.9 `src/extract/ws.rs:357-366` + `:438-441`; hyper 1.10 `src/upgrade.rs` (`impl Future for OnUpgrade`).
- **Evidence**:
  ```rust
  // ws.rs:156-163 — the registration happens in the handshake, BEFORE the upgrade:
      } = match crate::authorize_handshake(&headers, q.groups.as_deref()).await {
          Ok(h) => h,
          Err(resp) => return resp,
      };
      let handler = Realtime::message_handler();

      ws.on_upgrade(move |socket| handle_socket(socket, conn_id, rx, user_id, registry, handler))
  ```
  ```rust
  // lib.rs:1528-1542 — authorize_handshake: register into the cap + spawn the presence JOIN
      let Some((conn_id, rx, presence)) = registry
          .register_with_presence(user_id.clone(), groups.clone(), DEFAULT_BUFFER)
          .await
      else {
          return Err((
              http::StatusCode::SERVICE_UNAVAILABLE,
              "realtime: connection limit reached",
          )
              .into_response());
      };

      // Fire presence join + sync for any newly-entered presence-enabled group;
      // spawned so the handshake response isn't blocked on the broadcast.
      tokio::spawn(dispatch_presence(presence));
  ```
  ```rust
  // ws.rs:176-179 — handle_socket is the ONLY place the guard is built:
      let _guard = WsGuard {
          registry: registry.clone(),
          conn_id,
      };
  ```
  ```rust
  // axum 0.8.9 extract/ws.rs — the spawned upgrade task on failure calls the hook, then stops:
              Err(err) => {
                  on_failed_upgrade.call(Error::new(err));
                  return;
              }
  // and the default hook does nothing:
  impl OnFailedUpgrade for DefaultOnFailedUpgrade {
      #[inline]
      fn call(self, _error: Error) {}
  }
  ```
  ```rust
  // hyper 1.10 upgrade.rs — OnUpgrade resolves Err when the pending upgrade's
  // connection is gone before the upgrade completes:
              Err(_oneshot_canceled) => {
                  Err(crate::Error::new_canceled().with(UpgradeExpected))
              }
  ```
  No `on_failed_upgrade` appears anywhere in the crate (`grep -rn on_failed_upgrade plugins/umbral-realtime/` → no hits), and no periodic task ever sweeps the registry (the only deregistration paths are `WsGuard::drop`, `ConnGuard`'s drop in `sse.rs:88-95`, and explicit `deregister*` calls).
- **Impact**: the window is the handshake round trip, and no identity is needed to get into it — the default resolver is anonymous (`anonymous_resolver()`, installed by `Default for RealtimePlugin` at `lib.rs:1808-1812`) and the default policy permits every `public:*` group (`GroupPolicy::can_join` default at `lib.rs:862-865`, `PublicGroupsOnly` at `:881-883`). Concrete scenario: an attacker sends `GET {base}/ws?groups=public:z` with a valid WebSocket handshake and closes the TCP connection before reading the 101 (send-then-RST). The handler has already run: `authorize_handshake` registered the connection — consuming one of the `DEFAULT_MAX_CONNECTIONS = 10_000` slots (`lib.rs:100`, enforced at `lib.rs:400-404`) — and, for a presence-enabled group, spawned the JOIN broadcast that other members already observed. The write of the 101 then fails, hyper drops the pending upgrade, axum's spawned task takes the `Err` arm and calls the no-op default hook — `handle_socket` never runs, so `WsGuard` is never constructed and nothing deregisters. Repeat 10,000 times (a trivial scripted loop; wild equivalents are LB/proxy handshake timeouts mid-deploy) and `register_with_presence` returns `None` for everyone: every new connection, including real users', gets `503 realtime: connection limit reached` until the process restarts. Below the cap the leak is still live state: zombies stay in `conns`/`by_user`/`by_group`, presence rosters synced from the registry list users who are gone, and `user_in_group` suppression (`register_with_presence`, `lib.rs:411-418`) means a zombie's next *real* join produces no fresh JOIN for peers — they believe the user is still present. Each zombie also pins its per-connection buffer (≤ `DEFAULT_BUFFER` 64 deliveries). This is exactly the fail-closed-cap-gone-wrong shape: the fix for review_1 RT-3 (a finite cap) made registration a resource, and this race makes the resource non-recoverable. Not self-healing: only a successful upgrade ever creates the guard. (SSE is structurally immune — its `ConnGuard` lives *inside* the response body, `sse.rs:91-95`, so even a dropped response runs the drop.)
- **Fix**: install the failed-upgrade hook at the handshake and reuse the same teardown — `ws.on_failed_upgrade(move |_err| { let registry = registry.clone(); tokio::spawn(async move { let p = registry.deregister_with_presence(conn_id).await; crate::dispatch_presence(p).await; }); }).on_upgrade(...)` (set before `on_upgrade`; axum's builder type enforces the order). The alternative — registering only after the upgrade succeeds — is cleaner but needs the cap/policy refusal to become a post-upgrade close rather than a 503. Either way, add a regression test next to `tests/ws_origin.rs` that aborts an upgrade and asserts `registry` has no live conns.

### RT-2 · MEDIUM · drift (security)
**The Redis backplane publishes on a fixed, unprefixed channel and dispatches every inbound envelope it sees into the local registry, so two umbral apps sharing one Redis DB cross-deliver and cross-inject each other's realtime events — while the sibling `umbral-cache` plugin explicitly namespaces its keys for exactly that shared-DB deployment.**

- **Where**: `plugins/umbral-realtime/src/lib.rs:723-724` (channel), `:794-810` (inbound dispatch), `:2075-2083` (`redis()` doc); contrast `plugins/umbral-cache/src/lib.rs:725-730`.
- **Evidence**:
  ```rust
  impl RedisBroker {
      /// The pub/sub channel every umbral instance shares.
      const CHANNEL: &'static str = "umbral:realtime:events";
  ```
  ```rust
  // run_once — any inbound payload is deserialized and dispatched locally:
                      let payload: String = msg.get_payload().unwrap_or_default();
                      if let Ok(env) = serde_json::from_str::<Envelope>(&payload) {
                          registry
                              .dispatch(
                                  &env.target,
                                  Event { event: env.event, data: env.data, channel: String::new(), seq: 0 },
                              )
                              .await;
                      }
  ```
  ```rust
  /// Requires the `redis` cargo feature (`--features redis`). The same
  /// `url` must point every instance at one Redis. The [`Realtime`] API is
  /// unchanged; only the broker swaps.
  ```
  ```rust
  // umbral-cache, the sibling plugin with the same Redis dependency:
  pub const DEFAULT_PREFIX: &'static str = "umbral:cache:";
  // …doc: "set one per app when several umbral apps share one Redis DB so their caches don't collide"
  ```
- **Impact**: the shared-DB deployment is not hypothetical — `umbral-cache`'s own API documents it (a per-app prefix parameter whose reason is verbatim "several umbral apps share one Redis DB"). Realtime is the one Redis consumer with no prefix seam, and `redis()`'s doc never warns that the channel (and the Redis DB itself) must be dedicated. Two umbral apps on one Redis: every envelope published by app A is fanned out by app B's broker — `Target::Broadcast` reaches B's clients; `Target::User("42")` reaches **B's** user 42 (PK strings collide across apps); group names like `public:chat` collide so A's group traffic lands in B's subscribers; `presence` join/leave events (carried through the same dispatch path) leak user ids across apps; and anything that can `PUBLISH` on that Redis channel (any other tenant of the DB, or anything that can write to Redis at all) injects events straight into both apps' registries with the event name and data of the attacker's choosing — the per-transport join authorization never runs on this path because it's a server-to-server relay by design. Within a single app this is correct; the defect is that nothing makes the share explicit or disambiguates it. Bounded (needs a second tenant on the same Redis DB) and one config change away — MEDIUM, not HIGH — but the drift against cache's documented convention is exactly this tree's signature failure mode.
- **Fix**: derive the channel from a configurable prefix — mirror cache's seam, e.g. `redis(url).channel_prefix("myapp:")` with `DEFAULT_CHANNEL_PREFIX = "umbral:realtime:"` — or at minimum have `redis()`'s doc state that the Redis DB (or the channel) must be app-private. Also treat inbound payloads as untrusted: validate `env.target` against the same policy before `dispatch` (a cheap defense even in the dedicated-DB case).

### RT-3 · LOW · docs/drift (served client JS)
**The served SharedWorker comment claims `Last-Event-ID` closes the gap across a union-change reconnect, but `reconcile()` closes and constructs a brand-new `EventSource`, which sends no `Last-Event-ID` on its first request — and the server replays only when that header is present.**

- **Where**: `plugins/umbral-realtime/src/assets.rs:23-25` (claim), `:64-83` (`reconcile`), `:215-217` (fallback path); `plugins/umbral-realtime/src/sse.rs:68-76` (header-only replay trigger).
- **Evidence**:
  ```text
  // assets.rs:23-25 — the doc comment on the served worker:
  /// `@broadcast` / `@user:` same-session channel). When the union changes (a
  /// port subscribes/unsubscribes a new/last group) the single `EventSource`
  /// reconnects with the new union; `Last-Event-ID` fills the brief gap.
  ```
  ```js
  // assets.rs:70-82 — the reconnect is a close + NEW object:
  // union actually changes (EventSource resends Last-Event-ID, so the replay
  // buffer fills the brief gap); do nothing when it's unchanged.
  function reconcile() {
    var union = unionKey();
    ...
    if (es) { try { es.close(); } catch (_) {} es = null; }
    currentUnion = union;
    es = new EventSource("__UMBRAL_RT_BASE__/sse?groups=" + encodeURIComponent(union));
  ```
  ```rust
  // sse.rs:72-76 — replay happens only when the header is present:
      let last_event_id = headers
          .get("last-event-id")
          .and_then(|v| v.to_str().ok())
          .and_then(|s| s.trim().parse::<u64>().ok());
  ```
- **Impact**: `Last-Event-ID` is per-connection state: an automatic reconnect of the *same* `EventSource` resends it, but a freshly constructed one starts with no last id (the spec's "last event ID string" is empty for a new object), and the server has no query-param fallback. So on the path that matters most — the worker's *planned* reconnect whenever any tab subscribes or unsubscribes a group — everything published between the old stream closing and the new subscription is silently lost, while the comment tells the next maintainer no gap can exist. (The same false comfort covers the doc line: the replay buffer only ever helps network-blip reconnects.) The worker already receives `ev.lastEventId` per event in `route(...)`, so the id to send is in hand — only the plumbing is missing. Missed live updates, no data loss beyond that; LOW.
- **Fix**: remember the last seen id in the worker and append it on the new URL (`?groups=…&last_event_id=`), and accept `last_event_id` in `SseQuery` as a fallback for the header (the WS-side comment at `ws.rs:146-148` already notes WS has no native `Last-Event-ID`); or, if the replay gap is accepted, reword the comment to say the gap is possible and why.

### RT-4 · LOW · drift (served client JS)
**The `EventSource` fallback path builds its SSE URL with raw unencoded groups while the worker path percent-encodes them, so group names containing URI delimiters subscribe differently (or not at all) on exactly the browsers that need the fallback.**

- **Where**: `plugins/umbral-realtime/src/assets.rs:82` (worker) vs `:217` (`viaEventSource` fallback).
- **Evidence**:
  ```js
  // assets.rs:82 — worker path:
    es = new EventSource("__UMBRAL_RT_BASE__/sse?groups=" + encodeURIComponent(union));
  // assets.rs:217 — fallback path (no `SharedWorker` / construction threw):
      var es = new EventSource("__UMBRAL_RT_BASE__/sse?groups=" + groups);
  ```
- **Impact**: `splitGroups`/`unionKey` operate on raw names, so an app using a group like `room:hello world`, `org:a&b`, or anything composed from user input produces a query string that the fallback parses as *more* groups (`&`) or truncates at (`#`, `,` semantics aside) — the client then subscribes to the wrong set: on iOS Safari (no `SharedWorker`) or under a CSP that breaks worker construction, a user either misses their room's events or, worse, is denied (`403` from `authorize_handshake` on the phantom group name) and loses realtime entirely, while the same code works everywhere the worker path runs. Two implementations of one rule, fixed in one — the crate's signature drift, one line wide.
- **Fix**: `encodeURIComponent(groups)` in `viaEventSource`, matching the worker; the assets tests already string-match the worker's encoded form (`assets.rs:350-373`) — add the same assertion for the fallback.

### RT-5 · LOW · correctness (swallowed error)
**`Target::send` degrades a serialization failure to a silent `null` payload — every recipient gets a syntactically valid event with meaningless data and no log line anywhere.**

- **Where**: `plugins/umbral-realtime/src/lib.rs:1706-1718`.
- **Evidence**:
  ```rust
    pub async fn send<T: Serialize>(self, event: &str, data: &T) {
        let Some(rt) = REALTIME.get() else {
            return;
        };
        let data = serde_json::to_value(data).unwrap_or(serde_json::Value::Null);
  ```
- **Impact**: `serde_json::to_value` fails for entirely ordinary payloads — a `HashMap<u64, T>` map key (non-string keys are rejected by the JSON data model), any custom `Serialize` returning an error, a `serialize_with` that bails. The send then proceeds with `data = null`: clients receive `{"c":…,"e":"your-event","d":null}` and their handlers run with null data, while the server logs nothing — the "silently discards an error" pattern CLAUDE.md's fix-don't-patch rule names explicitly, and the most expensive kind to debug (a wrong payload, not a missing one; no failing assertion anywhere). Compare `pk_str`'s deliberate `unwrap_or` (a missing-field default at `lib.rs:1099-1104`) — this one is not that: serialization is expected to succeed, so failure means the caller's type is unsendable and must be surfaced. Cold path; LOW.
- **Fix**: log and skip (or send an error event) on the `Err` arm — `tracing::warn!(event, error = %e, "realtime: payload failed to serialize; skipping send")` — or return a `bool`/`Result` so callers can react. Nothing in the crate should ever transmit a `null` the app didn't ask for.

### RT-6 · INFO · limits
**The per-connection inbound rate cap counts only `Message::Text` frames; binary traffic is parsed per frame and dropped without ever touching the counter.**
- **Where**: `plugins/umbral-realtime/src/ws.rs:202-228`.
- **Observation**: `max_per_sec` is enforced inside the `Message::Text` arm (`:208-224`); `Message::Binary`, `Ping`, and `Pong` fall through to `_ => {}` (`:228`) and never increment `count_in_window`. A client can therefore sustain an unbounded binary-frame rate for as long as the OS socket buffers absorb it — each frame is still bounded by `max_message_size`/`max_frame_size` (`:127-128`) and is parsed then discarded, so the cost is per-connection CPU/parse work only (no handler dispatch is reachable through that arm, and the cap's stated purpose — protecting the app handler — is met). If the intent is to bound socket work too, counting non-`Close` frames in the same window is a one-line change; if not, a comment saying binary is intentionally unmetered would close the question. Observations only — no action required.

## Strengths

- **One handshake, structurally.** `authorize_handshake` (`lib.rs:1494-1551`) is the single join-authorization path — resolver, default-deny policy loop (`:1512-1521`), cap-aware registration, presence spawn — called verbatim by both `sse.rs:62` and `ws.rs:156`, with the doc comments on both handlers naming the shared contract. The review_3 finding (two copies that could drift) is genuinely dead, and the WS-side comment even documents that the WS gap here is the connection cap, not policy.
- **Safe defaults are the defaults.** All three audit_2 #4 limits ship enabled — `DEFAULT_MAX_CONNECTIONS = 10_000`, `DEFAULT_WS_MAX_MESSAGE_BYTES = 1 MiB`, `DEFAULT_WS_MAX_MESSAGES_PER_SEC = 100` (plus `DEFAULT_REPLAY_BUFFER = 1024` for the resume buffer), all wired in `Default for RealtimePlugin` at `lib.rs:1808-1826` — with an explicit `unlimited_connections()` opt-out; the eager-path-is-the-safe-path posture, and `tests/reconnect.rs::connection_cap_refuses_then_readmits` pins the refusal *and* the re-admission.
- **Fan-out economics are done deliberately.** One `Arc<Delivery>` rendered per dispatch and shared by every recipient (WS recipients `to_owned()` one finished string — gaps4 #38); the registry is cloned out and locked only to snapshot `(tx, user_id)` pairs, never held across send (`Registry::dispatch`, `lib.rs:541+`); `try_send` + drop on a full per-connection buffer means one slow client cannot stall a broadcast or the publisher; `tests/scale.rs::registry_stays_fast_and_non_starving_at_10k` holds 10k connections against a producer and asserts latency.
- **The broker seam is honest about loss.** `QUEUE_CAP = 4096` with drop-newest + rate-limited warning and a comment explaining why (events are ephemeral; never block a request handler on Redis — `lib.rs:726-742`, `:820-839`), plus a reconnect pump with 1s backoff. Exactly the shape review_3 codex #12 asked for, now with the reasoning in-tree.
- **Presence is a privacy-conscious feature, not a leak.** Off by default (`PresenceSpec::default()`), authenticated-only (id derived from the resolver, no anonymous presence), first-join transitions computed *before* indexing so dedup is correct (`lib.rs:409-418`), sync sent only to the joiner (O(N) not broadcast-per-member), projection defaults to id-only with an explicit custom-projection escape hatch, and policy-gated visibility — with dedicated tests (`presence.rs`, `presence_sync_scope.rs`, `presence_resolver.rs`).
- **Replay correctness details are right.** Monotonic per-connection seq stamped at dispatch; replay is target-filtered (`replay_since(id, user, groups)` — a reconnecting client can't read groups it isn't in); bounded buffer with an explicit "evicted is unrecoverable" doc; tests for ordering, resume, filtering, and cap (`tests/reconnect.rs`).
- **The CSWSH guard is a pure function with teeth.** `ws_origin_allowed` (`ws.rs:81-113`) is unit-tested across dev/prod/allowlist/same-origin/`Origin: null`, and `tests/ws_origin.rs` exercises the live router. Reject-before-register ordering is correct for policy *and* origin (475/403 happen before any registry mutation or presence).
- **The plugin stays a plugin.** Optional `auth` and `redis` deps — a REST-free, auth-free app compiles the crate with zero serializer/redis code; identity arrives via an injected closure inheriting `umbral::auth::default_authentication()` (gaps4 #42); `redact_url` sanitizes the boot log with tests; no `sqlx` anywhere in `src/` (the dev-dep is test-only, for a `#[derive(Model)]` expose test).

## Clean areas checked

- **Join authorization on every transport and API**: both transports (`sse.rs:62`, `ws.rs:156`) funnel through `authorize_handshake`; the only client-reachable endpoints are those two handshakes plus the two static assets (`routes()` enumerates exactly four) — there is no HTTP publish/subscribe API to bypass; `Registry::join` (trusted server-side membership change, `lib.rs:486-497`) is not reachable from a request; the client-to-server path (`MessageHandler` → `MessageContext::publish` → policy `can_send`) checks the send side with a default that mirrors `can_join` and tests (`publish_authz.rs`). Review_3 codex #19's "raw group publish" is by design at the `Realtime::to_group` level — the authorized path is `MessageContext`.
- **Cap enforcement points**: one shared counter (`Registry::max_connections` checked inside `register_with_presence`, `lib.rs:400-404`) so SSE and WS cannot drift; the refusal is a 503 *before* the upgrade; the rate limiter and frame-size caps are applied in the handler before upgrading (`ws.rs:127-128`).
- **Disconnect cleanup**: SSE's `ConnGuard` sits inside the stream object (drop = deregister, both on normal end and response-drop); WS's `WsGuard` covers normal exit, `break`s, and panics in the socket task; deregistration prunes all three indexes and emits presence leave/sync transitions (`deregister_with_presence`, `lib.rs:450+`). The only uncovered exit is the failed-upgrade path — RT-1.
- **Secrets/leakage**: `redact_url` strips `user:pass@` from the only URL this crate logs (boot line, `lib.rs:2194`; tests at `:2760-2800`); denied-group responses name only the group (`not allowed to join group …`) — acceptable, it echoes the client's own input; no tokens/cookies are logged or echoed.
- **SQL injection**: not applicable — no database access in `src/` (no sqlx dep; the ORM facade is not imported for queries at all).
- **Injection through the served assets**: the JS templates interpolate exactly one value (`__UMBRAL_RT_BASE__`, a plugin config string) and tests assert the rendered paths and the absence of leftover placeholders; event data travels as JSON payloads, and the client routes by name lookup rather than eval — no `innerHTML`, no `eval`, no `document.write` in the served JS.
- **Message-path panics**: no `unwrap`/`expect` on the request path outside `#[cfg(test)]`; the `expect`s I found are `Mutex::lock().expect("poisoned")` on the replay buffer, whose critical sections (push/iterate/trim) cannot panic, and the deliberate `unwrap_or` decodes noted under RT-5/`pk_str` (missing-field defaults, not error swallowing).
- **Frame handling**: outbound text only; inbound oversized frames error the read (closing the conn) rather than buffering; WS `select!` tears both halves down together; SSE keep-alive every 15s.
- **Feature-flag hygiene**: default build pulls neither `umbral-auth` nor `redis` (Cargo.toml comments state the contract; `redact_url` carries a `cfg_attr(no-redis)` allow for dead-code); the `redis` dep mirrors cache's features (`tokio-comp` + `connection-manager`).
- **Doc-claim spot checks**: the served worker/client docs vs the code (two mismatches → RT-3/RT-4; the rest verified); the "one EventSource per browser" union design verified in `reconcile`; `DEFAULT_REPLAY_BUFFER` doc matches the constant.

## Known issues already tracked elsewhere (not counted in the totals)

- `hardening/plugins-review umbra-realtime RT-1` — umbral-auth hard dependency `[fixed/verified held]` — optional behind `auth`, resolver injected, anonymous default.
- `hardening/plugins-review umbra-realtime RT-2` — no `Last-Event-ID` replay `[fixed/verified held]` — replay buffer + header resume + seq ids + `tests/reconnect.rs`.
- `hardening/plugins-review umbra-realtime RT-3` — no aggregate connection limit `[fixed/verified held]` — `DEFAULT_MAX_CONNECTIONS = 10_000`, 503 refusal, re-admission test.
- `hardening/plugins-review umbra-realtime RT-4` — sync-only `can_join` `[fixed/verified held]` — async `GroupPolicy` (gaps4 #36).
- `hardening/plugins-review umbra-realtime RT-5` — unbounded group namespace per connection `[open]` — no `max_groups`/`MAX_GROUPS` anywhere in `src/`; a handshake can still present an unbounded group list, each entry costing index + presence work. (RT-1's leak makes this one worse, but it stands on its own.)
- `hardening/plugins-review umbra-realtime RT-6` — stray MDX tool-gen artifacts `[fixed/verified held]` — not present in `src/`.
- `review_3/codex 12` — Redis broker unbounded queue `[fixed/verified held]` — `QUEUE_CAP 4096` + `try_send` drop-newest + warn (`lib.rs:726-742`, `:820-839`); verified in code, not re-reported.
- `review_3/codex 19` — raw group publish bypasses policy `[fixed/by-design]` — `Realtime::to_group` is the trusted server primitive; the client-facing seam is `MessageContext::publish`/`can_send`, documented as the authorization point (`lib.rs:867-878`).
- `review_3/claude 02_triage` — SSE/WS handshake authorization duplication `[fixed/verified held]` — `authorize_handshake` (`lib.rs:1494-1551`) used by both transports (note: `planning/gaps5.md:410` still lists this as DEFERRED — that tracker line is stale).
- `review_3/claude 02_triage` — stale `max_connections` *field* doc `[fixed/verified held]` — field doc corrected (`lib.rs:1748-1751`).
- `gaps6 #21(b)` — stale `max_connections` *builder-method* doc `[open]` — `max_connections()`'s doc still says "`None` (the default) is unlimited" while `Default` ships `Some(DEFAULT_MAX_CONNECTIONS)`; the tracker entry cites `lib.rs:1957`.
- `audit_2 plugin-realtime-comms #2-#5` `[fixed/verified held]` — publish authorization seam (#2), Redis URL redaction (#3, boot log + tests), all three #4 sub-parts now shipped (10k cap, 1 MiB frame cap, 100 msg/s limiter — the finding's attached stamp says "partial" but the code is complete at this commit), presence O(N²) → joiner-only sync (#5).
- `hardening/docs-audit misc.md` — channel/constant claims `[fixed/verified held]` — checked the documented claims against the constants; consistent (the *absence* of a prefix is RT-2, a different observation).

## Out-of-scope notes

- `umbral-cache` audit_2 #1/#7 items are the cache reviewer's file; RT-2's fix should mirror cache's `DEFAULT_PREFIX`/`connect_with_prefix` seam (`plugins/umbral-cache/src/lib.rs:725-730`) — cross-check so the two Redis consumers converge on one prefix convention.
- Dev-mode origin semantics and `umbral::settings` reads (`ws.rs:33-37` uses `umbral::settings::get_opt()`/`is_dev`) belong to the core/settings surface; if core's CORS policy ever permits credentialed cross-origin, revisit the SSE side of the CSWSH story (the WS-only guard is documented at `ws.rs:134-135` and matches audit_2's "browser CORS protects EventSource" note).
- `umbral::signals::subscribe_async` ordering/delivery semantics consumed by `expose`/`on_table` are the signals/core contract — tasks+signals reviewer.
- The axum/hyper failed-upgrade behavior cited in RT-1 is upstream-correct as designed (a hook exists for exactly this); no finding against axum — the finding is that umbral never uses the hook.
