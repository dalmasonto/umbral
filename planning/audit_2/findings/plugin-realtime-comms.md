# Audit — plugin-realtime-comms (`umbral-realtime`, `umbral-email`, `umbral-cache`)

> **Verification stamp — code re-triaged 2026-07-06.** Checked against current code. **Fixed:** #1 (`cache_page` bypasses personalised/`Authorization` responses + honors `no-store`/`private`), #3 (Redis URL redacted before logging), #4 (WS conn cap 10k + 1 MiB frame + inbound rate limit — all three sub-parts), #6 (`EmailConfig`/`ApiRequest` `Debug` masks creds), #8 (docs). **Still open →** #5 (O(N²) presence rebroadcast — wire-protocol change) and #7 (unbounded cache body buffer) tracked in `planning/gaps3.md #28`. **#2 FIXED (2026-07-07):** first-class `MessageContext::publish` / `can_send` publish-authz seam shipped (see the row below). #1's optional `PROXY_AUTHORIZATION`/`Vary` extension in `#27`. Treat the per-finding text below as historical.

Scope: security-first production audit of the three communication/caching plugins. Every finding cites `file:line` from code read in this pass. Runtime infra (TLS termination, log sinks, reverse-proxy caching, deployment env vars) was out of reach and is listed in Blind spots.

---

## A. Executive summary

Overall posture: the realtime and email plugins are **security-conscious by construction** — realtime is default-deny on group joins (`PublicGroupsOnly`), fail-closed on anonymous identity, has a real CSWSH/Origin guard, and id-only projections; email has a tested CRLF/header-injection guard and a tested fail-closed console-backend prod guard. The cache plugin is the weak link.

The single most urgent issue is in `umbral-cache`: **`cache_page` decides a 200 response is safe to store-and-share on the sole basis of whether the *request* carried an `umbral_session` cookie** (`cache_page.rs:152,245`). Any app that authenticates with a bearer token / `Authorization` header, HTTP Basic, or a differently-named cookie will have per-user responses cached and served to other users. Compounding it, the response-side bypass only honours `Cache-Control: no-store` (`cache_page.rs:261-282`) and ignores `private` / `no-cache` — so even a careful handler that marks its response `Cache-Control: private` is still cached into the shared store. The docs actively claim this keeps the cache to a "safe anonymous-only subset," which is false outside cookie-session auth.

Second: the realtime **inbound WebSocket message path has no built-in per-message authorization**, and the shipped documentation example (`transports.mdx:99-104`) broadcasts to a client-controlled `msg.room` with no membership check — a connected client can inject frames into any group (message spoofing / IDOR). Third: `RedisBroker` logs the full Redis connection URL at info level (`lib.rs:1743`), which commonly embeds a password.

What I could not assess: whether real deployments wrap authenticated routes in `cache_page`; the actual log sink/retention (secret-leak blast radius); TLS/`Secure`/`SameSite` cookie flags (owned by `umbral-sessions`, out of scope); axum's configured WS frame-size limits; and whether `EmailConfig`/`ApiRequest` are ever `{:?}`-logged downstream.

---

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix | Status |
|---|----------|------|----------------------|---------|--------|-----------------|--------|
| 1 | HIGH | Cache / authz | `umbral-cache/src/cache_page.rs:152,245-256,261-282` | `cache_page` gates cacheability only on an `umbral_session` request cookie and only bypasses on `Cache-Control: no-store`; it ignores `Authorization` headers, other cookie names, HTTP Basic, and the `private`/`no-cache` directives | Per-user authenticated 200 responses cached under a URL-only key and served to other users / anonymous clients → cross-user data disclosure | Bypass when `Authorization`/`Proxy-Authorization` present; honour `Cache-Control: private`, `no-cache`, `max-age=0`; make authenticated caching explicit opt-in; document the anonymous-only contract loudly | ✅ done |
| 2 | MEDIUM | Realtime / authz (IDOR) | `umbral-realtime/src/ws.rs:217-225`; doc `transports.mdx:99-104` | Inbound WS `MessageHandler` receives client frames with no framework-enforced per-message send authz; the documented example forwards to client-supplied `msg.room` with no membership/policy check | A client joined to `public:lobby` can `send({"room":"anything"})` and inject messages into groups it never joined (spoofing / abuse) | Provide/`document a `Realtime::policy().can_join(ctx.user_id, room)` (or registry membership) check before `to_group`; fix the doc example to check membership + add a warning | **FIXED 2026-07-07.** Shipped the first-class seam: `MessageContext::publish(group, event, data)` runs `GroupPolicy::can_send` and drops the frame (returns `false`) if the sender may not post to `group`, else broadcasts — the safe-by-default replacement for the raw `Realtime::to_group(...).send(...)`; plus `MessageContext::can_send(group)`. Handler doc + `transports.mdx` now teach `ctx.publish(...)`. Test `tests/publish_authz.rs`. |
| 3 | MEDIUM | Realtime / secrets in logs | `umbral-realtime/src/lib.rs:1743` | `tracing::info!("realtime: redis broker backplane → {url}")` logs the full Redis URL, which commonly carries `redis://:password@host` | SMTP-style credential leak to log aggregators | Log only the host (parse + redact userinfo), or log a fixed message without the URL | ✅ done |
| 4 | MEDIUM | Realtime / DoS | `umbral-realtime/src/lib.rs:1430` (`max_connections: None`), `ws.rs:217-225` | Connection cap defaults to unlimited; WS inbound loop has no per-connection message-rate cap and relies on axum's default frame-size ceiling | A few clients can exhaust FDs / memory, or flood the handler | Ship a sane default cap; add an inbound message-rate limit and an explicit max frame size | partial: 1 MiB default WS message+frame cap shipped (`ws_max_message_bytes`, tested in `tests/ws_limits.rs`) + prod `max_connections` Callout in `transports.mdx`; default connection cap deferred (flipping the unlimited default breaks existing deployments and needs an unlimited escape hatch); message-rate limit deferred (needs a per-conn limiter design) |
| 5 | LOW | Realtime / DoS amplification | `umbral-realtime/src/lib.rs:1213-1222` (`dispatch_presence` sync) | On every join, the full deduped member list is re-broadcast to the entire group, not just the joining conn | O(N²) fan-out for large presence rooms; CPU/bandwidth spike under join storms | Send the `presence:sync` snapshot only to the joining connection | deferred: changes the shipped presence wire protocol (clients may rely on the full-group re-sync); not a contained fix |
| 6 | LOW | Email / secrets in logs | `umbral-email/src/lib.rs:395-413` (`EmailConfig` derives `Debug`, holds `smtp_password`,`api_key`); `825-832` (`ApiRequest` derives `Debug`, holds `bearer`) | Plaintext credential fields under a `#[derive(Debug)]` type | Latent secret leak if any of these are ever `{:?}`-formatted into a log/error | Implement manual `Debug` that redacts secret fields, or wrap them in a redacting newtype | ✅ done |
| 7 | LOW | Cache / DoS | `umbral-cache/src/cache_page.rs:200-218` | `body.collect().await` buffers the entire upstream response in memory with no size cap before caching | A large streamed route wrapped in `cache_page` buffers unbounded memory per request | Cap the buffered size; skip caching (stream through) when the body exceeds a threshold | deferred: a real cap needs a stream-through path for oversized bodies (partial-buffer + body stitching), not a contained fix |
| 8 | LOW | Cache / docs drift | doc `plugins/cache.mdx:97,99-104` | Docs say the key is `cache:page:<METHOD>:<URI>` (no Host) and omit the session-cookie bypass | Misleads readers about isolation guarantees | Doc fixed this pass (see Docs updated) | ✅ done |

No SQL-injection, template-injection, open-relay, or header-injection issues were found in the artifacts read — those surfaces are parameterised (`sqlx::query(...).bind(...)`) or explicitly guarded (`validate_header_value`, tested in `header_injection.rs`).

---

## C. Detailed findings (HIGH / notable)

### #1 (HIGH) — `cache_page` serves authenticated responses to other users

Vulnerable code (`umbral-cache/src/cache_page.rs`):

```rust
// request side — the ONLY personalisation gate:
if request_has_session_cookie(&req) {          // line 152
    return inner.call(req).await;               // bypass
}
// ...
fn request_has_session_cookie<B>(req: &Request<B>) -> bool {   // line 245
    req.headers().get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').any(|p| p.trim().starts_with("umbral_session=")))
        .unwrap_or(false)
}

// response side — the ONLY store gate besides status==200:
fn response_bypasses_cache<B>(resp: &Response<B>) -> bool {    // line 261
    if let Some(cc) = resp.headers().get(header::CACHE_CONTROL) {
        if cc.to_str().unwrap_or("").split(',')
            .any(|d| d.trim().eq_ignore_ascii_case("no-store")) { return true; }
    }
    if resp.headers().contains_key(header::SET_COOKIE) { return true; }
    false
}
```

The cache key is `cache:page:{method}:{host}:{uri}` (`cache_page.rs:166`) — no user dimension, no `Vary` awareness.

Attack scenario: an app authenticates its API with `Authorization: Bearer <jwt>` (the framework's own docs at `gating.mdx:54-66` show exactly this token/JWT resolver pattern for realtime, so token auth is a first-class supported style). A developer wraps a route subtree — including `GET /api/me` returning the caller's profile JSON — in `cache_page(...)`. The handler returns `200`, sets `Cache-Control: private` (correct, per HTTP semantics: "do not store in a shared cache"), and sets no `Set-Cookie`. First authenticated request: no `umbral_session` cookie ⇒ not bypassed; `private` is ignored ⇒ the response is stored under `cache:page:GET:host:/api/me`. Every subsequent request to `/api/me` — including from a different user or an anonymous client — is served the first user's cached profile. This is a direct cross-user PII disclosure at 10M-user scale.

Corrected direction:

```rust
// Request side: bypass for ANY credentialed request, not just one cookie name.
fn request_is_personalised<B>(req: &Request<B>) -> bool {
    let h = req.headers();
    if h.contains_key(header::AUTHORIZATION) || h.contains_key(header::PROXY_AUTHORIZATION) {
        return true;
    }
    // Any cookie at all is a personalisation signal for a shared page cache.
    h.get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| !s.trim().is_empty())
}

// Response side: honour the directives that mean "not for a shared cache".
fn response_bypasses_cache<B>(resp: &Response<B>) -> bool {
    if let Some(cc) = resp.headers().get(header::CACHE_CONTROL).and_then(|v| v.to_str().ok()) {
        for d in cc.split(',').map(str::trim) {
            if d.eq_ignore_ascii_case("no-store")
                || d.eq_ignore_ascii_case("private")
                || d.eq_ignore_ascii_case("no-cache")
                || d.eq_ignore_ascii_case("max-age=0")
            { return true; }
        }
    }
    resp.headers().contains_key(header::SET_COOKIE)
        || resp.headers().contains_key(header::VARY)  // don't cache Vary-ing responses w/o key support
}
```

Blanket-bypassing on "any cookie" is deliberately conservative for a *shared* page cache; if that is too aggressive, at minimum the `Authorization` bypass and the `private`/`no-cache` honouring are non-negotiable, plus a prominent doc contract that `cache_page` is safe **only** for genuinely anonymous, non-`Vary`-ing content. (Assumption: `umbral_session` is the only auth signal the current code understands — confirmed by the literal-string match at `cache_page.rs:253`.)

### #2 (MEDIUM) — WS inbound messages have no built-in per-group authorization

`handle_socket` (`ws.rs:190-232`) dispatches every client text frame straight to `handler.on_message(&ctx, text)` (line 220). The handshake-time `GroupPolicy` gate (`ws.rs:151-160`) governs which groups the connection *joined*, but `to_group` dispatch (`lib.rs:430`) reaches whoever is in the target group regardless of who *sent* it — the sender need not be a member. The shipped example does no check:

```rust
// transports.mdx:99-104 (and lib.rs:801-806 doc) — INSECURE as written:
async fn on_message(&self, ctx: &MessageContext, text: String) {
    let msg: ChatMsg = serde_json::from_str(&text).unwrap();
    Realtime::to_group(&msg.room).send("message", &msg).await;  // msg.room is attacker-controlled
}
```

A client that connected with `?groups=public:lobby` can send `{"room":"chat:secret-team","body":"..."}` and inject a frame into `chat:secret-team`. Corrected pattern the docs should teach:

```rust
async fn on_message(&self, ctx: &MessageContext, text: String) {
    let msg: ChatMsg = match serde_json::from_str(&text) { Ok(m) => m, Err(_) => return };
    // The sender may only publish to a room its own identity is allowed to join.
    if !Realtime::policy().can_join(ctx.user_id.as_deref(), &msg.room) {
        return; // drop unauthorized publish
    }
    Realtime::to_group(&msg.room).send("message", &msg).await;
}
```

(Doc fixed this pass; the framework could also grow a first-class `Realtime::authorize_publish(ctx, room)` helper so this isn't left to each app.)

### #3 (MEDIUM) — Redis URL (with embedded password) logged at info level

`umbral-realtime/src/lib.rs:1743`:

```rust
tracing::info!("realtime: redis broker backplane → {url}");
```

`url` is whatever was passed to `RealtimePlugin::redis(...)`; the canonical form is `redis://[user:pass@]host:port/db` (see the cache plugin's own doc, `cache.mdx:43`). At info level this writes the password into every log sink. Fix: redact userinfo before logging, e.g. log `redacted_host(&url)` that keeps scheme+host+port only. (The `umbral-cache` Redis path does **not** log the URL — `lib.rs:426-432` — so this is realtime-specific.)

---

## D. Blind spots (could not verify from the artifacts)

- Whether any real app wraps authenticated routes in `cache_page` (finding #1 is a misuse-enabling design flaw; exploitability depends on wiring I can't see).
- Actual auth mechanism(s) apps use — if 100% of consumers use the `umbral_session` cookie, #1's blast radius shrinks; the framework docs advertising JWT/token resolvers (`gating.mdx:54`) suggest otherwise.
- Log sink, level filtering, and retention — determines the real severity of #3 and #6.
- Cookie flags (`HttpOnly`/`Secure`/`SameSite`) and CSRF for the session cookie — owned by `umbral-sessions`, out of this scope; the realtime CSWSH guard assumes the session cookie is `SameSite`-appropriate.
- axum's configured WebSocket max frame/message size (finding #4) — the plugin sets none, so it inherits the axum/tungstenite default (not visible here).
- Whether `EmailConfig`/`ApiRequest` `Debug` output ever reaches a log (finding #6) — no such call site found in the three plugins, but downstream code wasn't in scope.
- The realtime `RedisBroker` swallows per-publish errors and reconnects with a fixed 1 s backoff (`lib.rs:657-661,688-695`) — I could not assess message loss / at-least-once semantics under sustained Redis outage.
- SSE has no Origin guard by design (relies on browser CORS blocking cross-origin `EventSource` reads — `sse.rs` has no origin check). This is correct for the read side but I could not confirm no CORS layer elsewhere sets `Access-Control-Allow-Origin: *` with credentials, which would break the assumption.

---

## E. Prioritized action plan

Quick wins (< 1 day):
1. #3 — redact the Redis URL in the realtime info log.
2. #1 (partial) — add the `Authorization`/`Proxy-Authorization` bypass and honour `Cache-Control: private`/`no-cache` in `response_bypasses_cache`.
3. #2 — fix the `MessageHandler` doc example to check `can_join` before publishing (done in docs this pass) and add a warning callout.
4. #6 — manual redacting `Debug` for `EmailConfig` and `ApiRequest`.

Short term (< 2 weeks):
5. #1 (full) — document `cache_page`'s anonymous-only contract prominently; add `Vary`-response bypass; consider requiring explicit opt-in for any credentialed caching.
6. #4 — ship a default `max_connections`, an inbound WS message-rate limit, and an explicit max frame size.
7. #7 — cap buffered body size in `cache_page`.

Structural (needs design work):
8. #2 — add a first-class publish-authorization seam (`Realtime::authorize_publish`) so per-message authz isn't reinvented per app.
9. #1 — a proper `Vary`-aware cache key (key on the varying request headers) so per-encoding / per-language caching is correct instead of silently wrong.

---

## Docs updated

- **`documentation/docs/v0.0.1/plugins/cache.mdx`** — (a) corrected the stated cache key to include the Host segment (`cache:page:<METHOD>:<host>:<URI>`) to match `cache_page.rs:166`; (b) added the `umbral_session` request-cookie bypass to the "what bypasses caching" list to match `cache_page.rs:152`; (c) added a security `Callout` warning that `cache_page` only recognises the `umbral_session` cookie and only `Cache-Control: no-store` — so token/`Authorization`-header auth and `Cache-Control: private` responses are NOT protected (finding #1). Reason: the page claimed a "safe anonymous-only subset" the code does not deliver.
- **`documentation/docs/v0.0.1/realtime/transports.mdx`** — rewrote the `MessageHandler` example to check `Realtime::policy().can_join(ctx.user_id, &msg.room)` before `to_group`, and added a warning `Callout` that inbound frames carry no automatic send-authorization (finding #2). Reason: the prior example demonstrated an IDOR/message-spoofing pattern (publish to a client-controlled room with no membership check).

The realtime **`gating.mdx`** page was reviewed and is accurate for the *join* side (default-deny, anonymous-by-default footgun, CSWSH cross-reference) — left unchanged.
