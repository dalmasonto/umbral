# Session/device management and client integrity/attestation

Status: DRAFT for gaps5 #13 (tf#226) and gaps5 #12 (tf#225). Not ratified. Proposes the design; the final call and phasing are the maintainer's.
Date: 2026-08-08
Scope: two related Identity/Auth/Security backlog items, designed together because they share the same request-validation seams in `umbral-auth` and `umbral-sessions`.

## Why one doc

Both items harden the boundary between a user's credential and the client presenting it, and both hook into the exact same two validation functions: the bearer path `BearerAuthentication::authenticate` (`plugins/umbral-auth/src/bearer_auth.rs:85`) and the cookie path `read_session` (`plugins/umbral-sessions/src/lib.rs:596`). Session/device management (#13) answers "which of MY credentials is this, and can the user or an admin kill it". Client integrity (#12) answers "is the software presenting the credential a legitimate app at all". They are independent features, so they ship as separate plugins and separate phases, but designing them against one shared picture of the request lifecycle keeps the two from growing incompatible hook points.

Item #13 is the primary, most-implementable half and leads this doc. Much of its groundwork already exists in the codebase (see the honesty section below), so it is mostly additive. Item #12 is a larger, provider-heavy surface and is deliberately phased behind #13.

Both features MUST be plugins with no privileged core, consistent with the north-star (`docs/decisions/2026-08-08-product-north-star.md`): every capability is a plugin, including auth. #13 extends the existing `umbral-auth` and `umbral-sessions` plugins plus an optional admin surface. #12 is a new optional `umbral-attest` plugin that depends only on the `umbral` facade. Neither adds anything to `umbral-core`.

## Part 1: Session/device management (gaps5 #13)

### What exists today (be honest about the starting line)

The claim in gaps5 #13 that "logout revokes only the current bearer token" is precise. The single reusable logout is `umbral_auth::logout` at `plugins/umbral-auth/src/lib.rs:1194`. Its body:

1. If the request carries `Authorization: Bearer <key>`, it deletes exactly one `AuthToken` row, the one whose `key_hash` matches `digest_token(plaintext)` (`plugins/umbral-auth/src/lib.rs:1198-1205`).
2. It calls `umbral_sessions::logout` (`plugins/umbral-sessions/src/lib.rs:1097`), which destroys the one session row the request's cookie addresses and emits a clearing `Set-Cookie`.

The doc-comment on `logout` states the intent directly (`plugins/umbral-auth/src/lib.rs:1179-1183`): "logout means end THIS credential, so the user's other devices/tokens stay signed in." That is a correct primitive. What is missing is everything around it.

Three pieces of the requested feature are ALREADY built, and the design must not reinvent them:

- **Revoke-all for cookie sessions** exists: `umbral_sessions::revoke_user_sessions` (`plugins/umbral-sessions/src/lib.rs:643`) deletes every session row for a user PK by routing through `SessionStore::destroy_user`. It is the "log out everywhere" primitive already used by the password-reset sweep. The `Session.user_id` column is indexed for exactly this (`plugins/umbral-sessions/src/lib.rs:176`).
- **Absolute max session age** exists: `SessionsPlugin::max_session_age(secs)` (`plugins/umbral-sessions/src/lib.rs:311`) seals a cap that `read_session` enforces on every read (`plugins/umbral-sessions/src/lib.rs:610-621`), destroying a session older than the cap from its `created_at` no matter how far sliding expiry pushed `expires_at`.
- **Token hashing at rest and per-token labels** exist: `AuthToken` stores `base64(sha256(plaintext))` under a UNIQUE index, carries a human `name` label, and tracks `last_used_at` (coalesced to one write per minute) for staleness pruning (`plugins/umbral-auth/src/token.rs:67-92`, `196-209`).

So #13 is NOT "build session revocation from scratch". It is: (a) give bearer tokens the expiry and revoke-all that cookie sessions already have; (b) record device metadata (ip, user-agent, label, last-seen) for BOTH credential kinds, which neither model stores today; (c) present a single unified inventory across both kinds; (d) add self-service and admin revocation UI; (e) emit security-event notifications.

### The gaps, precisely

- `AuthToken` has NO `expires_at`, NO `revoked` flag, and NO device metadata (ip, user-agent, device label). A minted token lives until explicitly deleted. `SessionsPlugin::max_session_age` has no bearer-token equivalent.
- `Session` records `created_at`/`expires_at` but no ip, user-agent, or device label and no `last_seen_at` distinct from `expires_at`, so a "your devices" list cannot be rendered from it.
- There is no endpoint a logged-in user can call to list their active credentials or revoke one that is not the current request's credential.
- There is no admin surface for an operator to view and revoke a compromised user's sessions.
- There is no security-event notification (new-device sign-in, all-sessions-revoked, password-changed-elsewhere).

### The DeviceSession model (the inventory)

Add one inventory model, owned by `umbral-auth` (it already owns tokens and the login flows; it can optionally depend on `umbral-sessions` for the cookie side). One row per active credential, whether that credential is a cookie session or a bearer token. This unifies the list surface without forcing a UNION over two dissimilar tables at query time.

```rust
// plugins/umbral-auth/src/device.rs (new)
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct DeviceSession {
    pub id: i64,

    /// Owning user PK, stringified via Display (gap #59 convention: the
    /// same text form Session.user_id and Identity use, so a custom
    /// UserModel with a Uuid/String PK works unchanged). Indexed: the
    /// inventory list and revoke-all both filter on it.
    #[umbral(index, max_length = 64)]
    pub user_id: String,

    /// Which credential kind this row shadows.
    pub kind: DeviceCredentialKind, // Cookie | Bearer (a small enum -> CHECK)

    /// Opaque back-reference to the underlying credential so a revoke can
    /// destroy the real thing. For Cookie: the hashed session id
    /// (Session.id, which is already sha256(token)). For Bearer:
    /// AuthToken.key_hash. Indexed + UNIQUE so a validation-path lookup
    /// is O(1) and there is at most one inventory row per credential.
    #[umbral(index, unique, max_length = 64)]
    pub credential_ref: String,

    /// Human label. Cookie rows default to a parsed browser/OS guess;
    /// Bearer rows carry the token's `name` ("laptop", "CI", "iOS").
    #[umbral(max_length = 120)]
    pub device_label: String,

    /// Client IP captured at creation, refreshed on last-seen. Uses the
    /// framework's trusted-proxy-aware client-ip resolution (audit_2),
    /// NOT the raw socket addr, so it is correct behind a load balancer.
    #[umbral(max_length = 64)]
    pub ip: Option<String>,

    #[umbral(max_length = 400)]
    pub user_agent: Option<String>,

    pub created_at: DateTime<Utc>,

    /// Distinct from expires_at: the last time this credential
    /// authenticated a request. Coalesced (reuse AuthToken::TOUCH_COALESCE
    /// = 60s) so a busy credential is not re-written per request.
    pub last_seen_at: DateTime<Utc>,

    /// Hard expiry. For Bearer this is the NEW token max-age (see below);
    /// for Cookie it mirrors Session.expires_at at creation.
    pub expires_at: DateTime<Utc>,

    /// Soft revoke. Set true (with revoked_at) when the user or an admin
    /// kills the device. The validation path treats a revoked row as
    /// "credential no longer valid" even before the underlying row is
    /// swept. Indexed with user_id so revoke-all can bulk-flip cheaply.
    pub revoked: bool,
    pub revoked_at: Option<DateTime<Utc>>,
}
```

Two deliberate choices:

- The inventory row is a SHADOW of the real credential, not the credential itself. The source of truth for "can this cookie/token authenticate" stays the `Session` row and the `AuthToken` row. `DeviceSession` adds metadata and a fast revoke flag. This keeps the change additive: if the inventory write ever fails, auth still works off the primary rows, and a background reconciler can rebuild inventory rows from the primaries.
- `credential_ref` stores the HASHED identifier that already exists (`Session.id` is `sha256(token)`; `AuthToken.key_hash` is `base64(sha256(plaintext))`). The inventory never holds a plaintext credential, matching the existing hash-at-rest posture (`plugins/umbral-auth/src/token.rs:23-31`, `plugins/umbral-sessions/src/lib.rs:539-555`).

Alternative considered and rejected: extend `Session` and `AuthToken` in place with the metadata columns and union them for the list. Rejected because the list/revoke UI, the admin surface, and the notification triggers would each have to special-case two shapes; a single inventory model is one place to query, one place to admin, one place to test.

### Where creation hooks in

- Bearer: `AuthToken::create_for` (`plugins/umbral-auth/src/token.rs:134`) gains a sibling that also inserts a `DeviceSession` row (kind=Bearer, credential_ref=key_hash, expires_at = now + configured token max-age). The label comes from the token `name`.
- Cookie: session creation goes through `create_session` / `login_user_id` (`plugins/umbral-sessions/src/lib.rs:570`, `984`). Because `umbral-sessions` must stay free of any user-model dependency, the DeviceSession insert is driven from the `umbral-auth` login helper `login_with_request` (which already knows the AuthUser and calls into sessions), not from inside `umbral-sessions`. The ip/user-agent are read from the request headers at that call site.

### Where revocation/validation hooks in (cite the real functions)

- **Bearer validation:** `BearerAuthentication::authenticate` (`plugins/umbral-auth/src/bearer_auth.rs:85`). Today it does: `AuthToken::lookup` -> load active user -> `touch_last_used` -> return Identity. Insert one step after `lookup`: load the `DeviceSession` by `credential_ref = token.key_hash` and reject (return `None`, the existing anonymous-on-failure contract at `bearer_auth.rs:36-45`) when the row is `revoked`, or when `now > expires_at`. This is where the NEW token max-age is enforced, giving bearer tokens the bound that `SessionsPlugin::max_session_age` already gives cookies. The `last_seen_at` bump reuses the coalescing already present in `touch_last_used` (`plugins/umbral-auth/src/token.rs:196`).
- **Cookie validation:** `read_session` (`plugins/umbral-sessions/src/lib.rs:596`) already enforces absolute max age at lines 610-621. The revoke check belongs alongside it, but `umbral-sessions` cannot name the `umbral-auth`-owned `DeviceSession` model. Two options: (a) the cookie-side `Authentication` impl `SessionAuthentication` (in `plugins/umbral-auth/src/session_user.rs`, which CAN name both) performs the revoked-row check after resolving the session, mirroring the bearer path; or (b) revocation for cookies stays implemented by DELETING the `Session` row (the existing `destroy_session` / `revoke_user_sessions` path), so `read_session` returns `None` naturally with no cross-plugin reference. Option (b) is preferred for revoke-all and single-device revoke because it reuses `revoke_user_sessions` (`plugins/umbral-sessions/src/lib.rs:643`) verbatim; the `revoked` flag on `DeviceSession` is then primarily the bearer-side and audit mechanism. This keeps the plugin boundary clean.
- **Logout:** `umbral_auth::logout` (`plugins/umbral-auth/src/lib.rs:1194`) keeps its "end THIS credential" semantics but additionally flips the current credential's `DeviceSession.revoked` (or deletes its inventory row) so the inventory list stops showing a device the user just signed out.

### Self-service APIs

New routes contributed by `AuthPlugin::routes()` (the plugin already contributes `/auth/*` routes), all requiring an authenticated identity via the existing `RequireAuth` extractor (`plugins/umbral-auth/src/extractors.rs`, re-exported at `lib.rs:88`):

- `GET /auth/sessions` -> list the caller's active `DeviceSession` rows (id, device_label, ip, user_agent, created_at, last_seen_at, expires_at, and an `is_current` flag computed by matching the request's own credential_ref). JSON and an HTML variant, matching the existing dual JSON/HTML auth surface.
- `DELETE /auth/sessions/{id}` -> revoke one device. Scoped to the caller's own rows (object-scoping per the audit_2 posture); revoking someone else's row 404s. Revoking a Cookie row calls `destroy_session` on that session; a Bearer row deletes the `AuthToken` and flips the inventory row.
- `POST /auth/sessions/revoke-all` -> revoke every credential EXCEPT the current one (the common "sign out my other devices" button). Cookie side reuses `revoke_user_sessions` then re-establishes the current session; bearer side bulk-deletes the user's other `AuthToken` rows. A `?include_current=true` variant does a full "sign out everywhere".

These compose with `umbral-rest` throttling and CSRF exactly like the existing `/auth/*` routes; no new gate is invented.

### Admin surface

An admin action, not a bespoke page, is the smaller lift. Register `DeviceSession` with `umbral-admin` (list display: user, device_label, ip, last_seen_at, revoked) and add a bulk admin action "Revoke selected sessions" plus a per-user "Revoke all sessions for this user" action. This reuses the admin custom-action/AdminView surface that already shipped (see the admin-custom-views work). Operator revocation of a compromised account is then two clicks and goes through the same `destroy_session` / `AuthToken` delete paths as self-service, so there is exactly one revocation code path.

### Security-event notifications

Emit on: new-device sign-in (a login whose resolved device fingerprint has no prior non-revoked `DeviceSession` for that user), revoke-all, and password change. `umbral-auth` already carries a mailer abstraction (`AuthMailer` / `ConsoleMailer` / `OutgoingMail` / `MailKind`, re-exported at `plugins/umbral-auth/src/lib.rs:77`), so Phase 1 notifications go out through it with new `MailKind` variants. A richer multi-channel path (SMS/push/in-app) waits on `umbral-notifications` (gaps5 #55), which does not exist yet; this doc does not block on it.

### Phasing for #13

- Phase 1 (core value, mostly additive): `DeviceSession` model + migration; wire creation into token mint and cookie login; add the revoked/expiry checks to `BearerAuthentication::authenticate`; add token max-age config on `AuthPlugin` mirroring `SessionsPlugin::max_session_age`; ship `GET /auth/sessions`, `DELETE /auth/sessions/{id}`, `POST /auth/sessions/revoke-all`. Cookie revoke-all reuses the existing `revoke_user_sessions`.
- Phase 2: admin registration + bulk revoke actions; new-device detection.
- Phase 3: security-event emails via `AuthMailer`; a background reconciler that prunes inventory rows whose primary credential is gone (extend the existing `clearsessions` command, `plugins/umbral-sessions/src/lib.rs:443`, and add a `cleartokens`/inventory sweep).

## Part 2: Client integrity / attestation (gaps5 #12)

### Goal and non-goal

Firebase App Check reduces abuse from clients that are not your genuine app by requiring each request to carry a short-lived attestation token that a device/app integrity provider issued. The umbral analog is an OPTIONAL layer that verifies such a token before a request reaches a protected handler. It answers "is this a legitimate client", which is ORTHOGONAL to authentication's "who is this user". A request can be perfectly authenticated (valid bearer token) and still fail attestation (the token was replayed from a scraper). Therefore attestation is NOT an `Authentication` impl; it is a middleware/gate that runs independently and can be composed with or without auth.

Non-goal: attestation is not a WAF, not a bot-score engine, and not a CAPTCHA-everywhere policy. Those are gaps5 #19. Attestation is specifically the "prove the client is a genuine app/device" primitive that the mobile and web integrity providers already offer.

### The Attestation trait

```rust
// plugins/umbral-attest/src/lib.rs (new plugin)
#[async_trait]
pub trait Attestation: Send + Sync {
    /// Verify the attestation material carried on this request. Reads a
    /// provider-specific header (default `X-Umbral-App-Check`) plus any
    /// nonce the provider needs. Returns the outcome; NEVER panics and
    /// NEVER blocks longer than a bounded timeout (a slow provider must
    /// fail-open or fail-closed per config, not hang the request).
    async fn verify(&self, headers: &HeaderMap, ctx: &AttestContext) -> AttestOutcome;

    /// Provider name for logs/metrics ("turnstile", "recaptcha",
    /// "play-integrity", "app-attest", "devicecheck").
    fn provider(&self) -> &'static str;
}

pub enum AttestOutcome {
    Pass { claims: serde_json::Value }, // provider score/claims for logging
    Fail { reason: AttestFailReason },
    /// Provider unreachable/timeout. The MODE (below) decides what a
    /// Skipped outcome does to the request.
    Skipped,
}
```

The trait signature intentionally mirrors `Authentication::authenticate(&self, &HeaderMap)` (`umbral::auth::Authentication`, used at `plugins/umbral-auth/src/bearer_auth.rs:84`), so a developer wiring attestation recognises the shape and the same header-in seam is reused.

### Provider adapters

Each adapter implements `Attestation`. Ship them behind cargo features so an app that only needs web attestation does not pull mobile-provider deps:

- Web: Cloudflare Turnstile and Google reCAPTCHA (v3 score + Enterprise). Both are a server-side "verify this client token against the provider's siteverify endpoint" call. Turnstile/reCAPTCHA are the pragmatic web analog of App Check's web/reCAPTCHA provider.
- Web (Apple): App Attest for web/PWA is not a real thing; the honest web story is Turnstile/reCAPTCHA plus optional origin/`Sec-Fetch` checks.
- Mobile: Google Play Integrity (Android) and Apple App Attest + DeviceCheck (iOS). These are nonce/challenge based: the server issues a nonce, the app produces an attestation over it, the server verifies the signature/assertion and the app's identity (bundle id / package name), and for App Attest tracks the per-key assertion counter to defeat replay.

Because the mobile providers need a server-issued nonce, the plugin also exposes `POST /attest/nonce` (short-lived, single-use nonce) that the mobile SDK calls before attesting. Web providers do not need it.

### The verification middleware and enforcement modes

A tower/axum middleware `AttestLayer` runs the configured provider chain and, on the result, applies the enforcement MODE:

- `Monitor` (default first rollout): verify, emit a metric/log with pass/fail, but NEVER block. This mirrors App Check's "monitor before enforce" so an operator can see how much legitimate traffic would fail before turning enforcement on. Critical for not locking out real users on day one.
- `Enforce`: a `Fail` returns HTTP 403; a `Skipped` (provider down) is governed by a `fail_open`/`fail_closed` sub-setting, defaulting to `fail_open` so a provider outage does not take the app down (a deliberate availability-over-strictness default that an operator can flip).

The layer is configured on the `umbral-attest` plugin with the provider(s), the mode, the header name, and per-route-group opt-in.

### How enforcement composes across surfaces (all via plugin seams, no core changes)

Attestation is applied per surface by wrapping that surface's router or reusing its existing gate. It does not replace any identity check; it runs alongside it.

- REST and auth endpoints: apply `AttestLayer` via the `Plugin::wrap_router` hook (`crates/umbral-core/src/plugin.rs:391`) or `Plugin::middleware` (`crates/umbral-core/src/plugin.rs:413`). Concretely, `umbral-attest` wraps the app router, and `RestPlugin` / `AuthPlugin` grow a `.require_attestation()` opt-in that scopes the layer to their route groups. The highest-value auth targets are `/auth/login`, `/auth/register`, and the password-reset start endpoint, where attestation complements the existing per-IP `Throttle` (`plugins/umbral-auth/src/throttle.rs`, re-exported at `lib.rs:99`) by cutting off automated clients before they consume a throttle budget.
- Realtime: `umbral-realtime` already has an `IdentityResolver` seam invoked at connection setup for `GET /realtime/sse` and `GET /realtime/ws` (`plugins/umbral-realtime/src/lib.rs`, the `IdentityResolver` type and `RealtimePlugin::identity_resolver`). Add a parallel `AttestResolver`/`require_attestation` that runs at the SAME handshake point, rejecting the upgrade before a socket is registered. Attestation at connect time (not per frame) matches the long-lived nature of the connection; the attestation token is checked once at the handshake.
- Storage: `umbral-storage` already runs a `MediaAccessFn` gate on every `GET <mount>/<key>` before any bytes are served (`plugins/umbral-storage/src/lib.rs`, the `MediaAccessFn` type). Attestation composes INTO that callback for downloads, and the upload/presigned-POST endpoints (gaps5 #58) wrap `AttestLayer`. This is the one surface where a per-request check is worth it because each download is an independent, cacheable, abusable resource.
- Auth (token mint / session create): attestation on the login endpoint transitively protects credential issuance; a client that cannot attest cannot obtain a bearer token or a session cookie in the first place.

### Replay and freshness

- Web provider tokens are short-lived and verified server-side against the provider each time; the provider handles freshness.
- Mobile providers use the server nonce from `POST /attest/nonce` (single-use, short TTL, stored briefly) so an assertion cannot be replayed. App Attest additionally exposes a monotonic assertion counter per key; the adapter persists the last counter per (user or device key) and rejects a non-increasing counter.

### Phasing for #12

- Phase 1: `Attestation` trait + `AttestOutcome` + `AttestLayer` with `Monitor`/`Enforce` modes; web adapters (Turnstile, reCAPTCHA); REST + auth-endpoint hooks; metrics/logging of pass/fail. Ship Monitor as the documented default.
- Phase 2: mobile adapters (Play Integrity, App Attest, DeviceCheck) + `POST /attest/nonce` + assertion-counter replay defense.
- Phase 3: realtime handshake hook + storage download/upload hooks + per-provider dashboards (depends on gaps5 #64 metrics for the operator view).

## Cross-cutting: plugin boundaries and testing

- #13 lives in `umbral-auth` (the `DeviceSession` model, the routes, the bearer-side checks) with the cookie side reusing `umbral-sessions`' existing `revoke_user_sessions` / `destroy_session`. No new plugin; no core change.
- #12 is a new optional `umbral-attest` plugin depending only on the `umbral` facade, exactly like every other plugin. Providers are cargo-feature-gated. Enforcement is applied through the public `Plugin::wrap_router` / `Plugin::middleware` hooks and the existing per-surface seams (realtime `IdentityResolver`, storage `MediaAccessFn`), so `umbral-core` never names attestation.
- All row-level reads/writes go through the ORM per the plugin rule (the `DeviceSession` list, revoke, and nonce store use `objects().filter(...)` terminals, never raw `sqlx::query`).
- Behavioral tests (real rows, real request path, read the graph back): mint a token -> confirm a `DeviceSession` row appears -> revoke it via `DELETE /auth/sessions/{id}` -> confirm the next `Authorization: Bearer` request resolves anonymous through `BearerAuthentication::authenticate`. For attestation: a request without a valid attestation header under `Enforce` gets 403; the same request under `Monitor` passes but logs a fail; a valid provider token passes both.

## Open questions for the maintainer

1. #13: should cookie-session revocation be modeled by DELETING the `Session` row (reusing `revoke_user_sessions`, keeping `read_session` unchanged and the plugin boundary clean) or by a `revoked` flag checked in `SessionAuthentication`? This doc recommends delete-based for cookies and the `revoked` flag primarily for bearer tokens and audit.
2. #13: is a unified `DeviceSession` inventory preferred over extending `Session` and `AuthToken` in place? This doc recommends the unified model.
3. #12: default enforcement posture on a provider outage: `fail_open` (availability) or `fail_closed` (strictness)? This doc defaults to `fail_open` with an operator flip.
4. #12: is `umbral-attest` in near-term scope at all, or is it deferred behind the Stage 2 platform posture? Given its provider surface area, this doc treats #12 as later than #13.
