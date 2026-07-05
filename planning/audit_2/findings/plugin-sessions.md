# Audit — `plugins/umbral-sessions/`

Scope: session ID generation, cookie flags, expiry/renewal, revocation, store integrity (DB / Redis / cookie), tampering/deserialization, fixation, concurrency at scale. Files read in full: `src/lib.rs`, `src/store.rs`, `src/cookie_store.rs`, `src/redis_store.rs`, `src/request_session.rs`, `Cargo.toml`, and the user doc `documentation/docs/v0.0.1/plugins/sessions.mdx`.

---

## A. Executive summary

The core primitives are mostly sound: session tokens are 122-bit UUIDv4 values from the OS CSPRNG, stored only as `SHA-256(token)` server-side (a DB leak cannot replay live sessions), cookies carry `HttpOnly; Secure; SameSite=Lax; Path=/` and default to `Secure` even before settings resolve, and `CookieStore` uses a real AEAD (XChaCha20Poly1305) so tampering fails closed. Login rotates the token (fixation defense).

The serious problems are architectural, not primitive-level. **The pluggable `SessionStore` abstraction is only half-wired: `active_store()` is consulted in exactly one place — `session_layer` (lib.rs:1210) — while every other session operation (`read_session`, `destroy_session`, `revoke_user_sessions`, `set_data` fallback, `Messages`) bypasses the store and hits the SQL `session` table directly.** For the default `DbStore` this is invisibly correct. For the two *documented and recommended* alternative stores (`RedisStore`, `CookieStore`) it silently breaks: (1) **`revoke_user_sessions` — the "log out everywhere" primitive that auth's password-reset flow calls (`umbral-auth/src/challenge.rs:470`) — deletes from an empty SQL table and leaves the real Redis sessions live**, so a password reset does not invalidate a stolen session (finding #1, HIGH); (2) logout has the same defect; (3) `CookieStore` login is functionally broken (finding #3). Separately, `CookieStore`'s empty-`secret_key` guard only logs — the hard boot-fail it references lives in the *optional* `umbral-security` plugin, so a CookieStore app deployed without that plugin serves trivially-forgeable sessions (finding #2, HIGH).

The three most urgent items: (1) route `revoke_user_sessions`/`destroy_session`/`read_session` through `active_store()`; (2) make `CookieStore` refuse to boot on an empty key regardless of whether `umbral-security` is registered; (3) fix or gate `CookieStore` login.

Could not assess: whether every production deployment registers `umbral-security` (would downgrade #2); the actual query planner behavior / index set materialized by the migration engine for the `session` table (assessed from the model definition only); real-world write throughput under sliding expiry.

---

## B. Findings table

| # | Severity | Area | Location | Finding | Impact | Fix | Status |
|---|----------|------|----------|---------|--------|-----|--------|
| 1 | HIGH | Revocation / store integrity | lib.rs:440-446, 452-464, 415-428; store.rs:237 | `revoke_user_sessions`, `destroy_session`/logout, and `read_session` operate on the SQL `session` table directly, never through `active_store()`. Under `RedisStore` or `CookieStore` these are no-ops against empty/nonexistent SQL rows. | Password-reset revocation (auth `challenge.rs:470`) and logout do NOT invalidate server-side sessions on Redis; a stolen cookie stays valid until natural TTL (up to 14 days, unbounded with sliding expiry). Defeats the documented "server-side revocation: Yes" for RedisStore. | Route all lifecycle ops through the installed store; add `SessionStore::destroy_user(user_id)` and call it from `revoke_user_sessions`. | ✅ done (audit_2 H7) — `SessionStore::destroy_user` added; `read_session` / `revoke_user_sessions` / `destroy_session` now route through `active_store()` (lib.rs:466/494/508). DbStore/RedisStore implement `destroy_user`; a stateless CookieStore returns `RevocationUnsupported` so the caller logs it rather than silently succeeding. |
| 2 | HIGH | Secrets / store integrity | cookie_store.rs:123, 143-174; lib.rs:243-260 | `CookieStore` with an empty `secret_key` only emits `tracing::error!` and boots. The referenced hard-fail ("the SessionsPlugin boot check is the hard-fail point") does not exist in `SessionsPlugin::on_ready`; the real check is `umbral_security::validate_secret_key`, in a separate opt-in plugin. Core `check.rs` only catches the *dev-default* string, not an empty key. | An app using `CookieStore` without registering `umbral-security` and with an empty/unset `secret_key` in prod serves sessions encrypted under `SHA-256("")` — trivially forgeable → full auth bypass / account takeover by forging any `user_id`. | Have `CookieStore` (or `SessionsPlugin::on_ready` when a `CookieStore` is installed) hard-fail boot on an empty `secret_key` in `Prod`, independent of `umbral-security`. | ✅ done |
| 3 | MEDIUM | Store correctness | lib.rs:802-815, 1300-1331; request_session.rs:146-164 | In-request login (`login_user_id_in_request`) rotates to a **UUID** token and `login_user_id` writes `Set-Cookie` with that UUID. Under `CookieStore` the cookie value must be the encrypted blob (only `store.save` produces it), but the handler-set-cookie guard (lib.rs:1300, 1325) suppresses the layer's blob re-set because `SET_COOKIE` is already present. | After login under `CookieStore`, the browser holds a bare UUID that decodes to no session; the next request resolves anonymous — login silently fails (fails closed, so not a bypass, but the documented feature is unusable with the login helper). | Have login defer the `Set-Cookie` to the layer for stateless stores, or make the layer overwrite its own login-rotation cookie with the store's returned value. | deferred: not contained — the fix reworks the login helper / session_layer Set-Cookie handshake shared by every store, with real risk of regressing the working DbStore login path. Needs the same store-abstraction reconciliation as #1. |
| 4 | MEDIUM | Data layer / scale | lib.rs:97-104 (model), 440-446 (revoke), 287-291 (clearsessions) | The `Session` model declares no index on `user_id` or `expires_at`. `revoke_user_sessions` filters on `user_id`; `clearsessions` filters on `expires_at`. | At 10M rows both are full table scans. A password-reset burst or the periodic `clearsessions` sweep does sequential scans of the whole session table, contending with the per-request PK reads. | Add indexes on `user_id` and `expires_at` in the model / migration. | ✅ done — `Session.expires_at` now carries `#[umbral(index)]` (`user_id` already did). The autodetector blocker is fixed too: a single-column `#[umbral(index)]` flip now emits a proper `AddIndex`/`DropIndex` (backend-correct) instead of being folded into an `AlterColumn` that created NO index on Postgres and forced a needless SQLite table rebuild. An existing app's `makemigrations` now generates the `expires_at` index migration correctly on both backends. Tests `migrate_index_autodetect::single_column_index_flag_flip_emits_add_and_drop_index`. |
| 5 | MEDIUM | Expiry policy | lib.rs:1250-1252, 199-202 | Sliding expiry has no absolute-lifetime cap: each request bumps `expires_at` to `now + 14d`. There is no idle-vs-absolute distinction. | A session (or a stolen cookie) that is used at least once per 14 days never expires. No server-side maximum session age. | Add an absolute cap (e.g. reject when `now - created_at > MAX_AGE`) alongside the sliding window. | ✅ done — `SessionsPlugin::max_session_age(secs)` sets an absolute cap; `read_session` rejects AND destroys any session older than that from `created_at`, regardless of `expires_at`. Behavior-preserving: **off by default** (a framework decision to opt-in, since a hard cap would otherwise expire live sessions). Tests `max_session_age` (enforces + destroys) and `max_session_age_off_by_default` (unset → old session still valid). |
| 6 | LOW | Store correctness | lib.rs:714-765 | `set_data`'s out-of-request fallback (`upsert_session_data_key`) writes raw SQL to the `session` table regardless of the active store; on Redis/Cookie stores that write is never read back. | Background/out-of-request `set_data` and `Messages` (which route through `read_session`) silently no-op or lose data on non-DB stores. | Route `set_data`/`Messages` through `active_store().load/save`. | deferred: architectural — same root cause as #1 (the raw-ORM-vs-store split). Rerouting these paths belongs with the store-abstraction reconciliation, not a standalone patch. |
| 7 | LOW | Transport | lib.rs:513-519, 529-534 | Cookie uses no `__Host-` prefix and no configurable `Domain` (host-only, which is fine) but `SameSite=Lax` is fixed (not configurable to `Strict`). | Lax permits the cookie on top-level cross-site GET navigations; acceptable default but not tunable for high-security deployments. | Optional: allow `SameSite` and a `__Host-` prefix via plugin config. | deferred: LOW / enhancement — the current `SameSite=Lax` default is safe; making it configurable is a new plugin-config surface (framework decision), not a bug fix. |

No other issues found in the provided artifacts for token entropy, hashing, AEAD tamper handling, SQL-injection in the JSON upserts (all parameterized), or deserialization safety (serde_json, no code-exec surface).

---

## C. Detailed findings (HIGH)

### Finding #1 — Revocation and logout bypass the installed session store (HIGH)

`active_store()` is used in exactly one production path — `session_layer` load/save (lib.rs:1210). Every other lifecycle operation talks to the SQL table directly via the ORM:

```rust
// lib.rs:440 — revoke ALL of a user's sessions (password reset / "log out everywhere")
pub async fn revoke_user_sessions(user_id_str: &str) -> Result<u64, SessionError> {
    let removed = Session::objects()
        .filter(session::USER_ID.eq(user_id_str))   // <-- SQL `session` table, not active_store()
        .delete()
        .await?;
    Ok(removed)
}

// lib.rs:459 — destroy one session (logout)
async fn destroy_session_by_hash(stored_id: &str) -> Result<(), SessionError> {
    Session::objects()
        .filter(session::ID.eq(stored_id))           // <-- SQL, not active_store()
        .delete()
        .await?;
    Ok(())
}
```

`revoke_user_sessions` is the revocation primitive auth relies on after a credential change:

```
plugins/umbral-auth/src/challenge.rs:470
    if let Err(e) = umbral_sessions::revoke_user_sessions(&user_id.to_string()).await { ... }
```

**Attack scenario.** A production app is deployed with `SessionsPlugin::default().store(RedisStore::connect(...))` — the configuration the docs explicitly recommend "when you want instant server-side revocation (force-logout, security events)". An attacker phishes a user's session cookie. The user (or an admin responding to the incident) resets the password. Auth calls `revoke_user_sessions(user_id)`, which runs `DELETE FROM session WHERE user_id = ?` against the SQL table — but under `RedisStore` there are **zero** rows in that table; the live sessions are Redis keys `umbral:session:<hash>`. The delete returns `Ok(0)`, the reset flow reports success, and the attacker's stolen cookie keeps authenticating until the Redis TTL fires (up to 14 days, or never with sliding expiry). Logout has the identical defect: `destroy_session` deletes from SQL, so the Redis key survives; only the victim's own cookie is cleared client-side.

**Corrected direction.** Add a user-scoped destroy to the trait and route all lifecycle ops through the store:

```rust
#[async_trait::async_trait]
pub trait SessionStore: Send + Sync + std::fmt::Debug {
    async fn load(&self, token: &str) -> Result<Option<SessionRecord>, SessionError>;
    async fn save(&self, token: &str, record: &SessionRecord) -> Result<String, SessionError>;
    async fn destroy(&self, token: &str) -> Result<(), SessionError>;
    /// Revoke every session owned by `user_id`. DbStore: DELETE ... WHERE user_id.
    /// RedisStore: SCAN + DEL (or a per-user index set). CookieStore: not supported —
    /// return an explicit `Err`/`Unsupported` so callers don't believe revocation happened.
    async fn destroy_user(&self, user_id: &str) -> Result<u64, SessionError>;
}

pub async fn revoke_user_sessions(user_id_str: &str) -> Result<u64, SessionError> {
    active_store().destroy_user(user_id_str).await
}

pub async fn destroy_session(token: &str) -> Result<(), SessionError> {
    active_store().destroy(token).await
}
```

For `CookieStore`, `destroy_user` must surface that server-side revocation is impossible (return an error the caller can log) rather than silently succeeding — the current `CookieStore::destroy` returning `Ok(())` is itself part of the illusion.

---

### Finding #2 — `CookieStore` empty-key guard only logs; the claimed hard-fail is in a different, optional plugin (HIGH)

`CookieStore` derives its AEAD key as `SHA-256(secret_key)` (cookie_store.rs:106-114). With an empty key it warns/errors but proceeds:

```rust
// cookie_store.rs:143 — resolve_ambient_key()
if secret.trim().is_empty() {
    match umbral::settings::get_opt().map(|s| &s.environment) {
        Some(umbral::Environment::Prod) => tracing::error!("... TRIVIALLY FORGEABLE ..."),
        _ => tracing::warn!("..."),
    }
}
derive_key(&secret)   // <-- still derives SHA-256("") and keeps serving
```

The doc-comment claims the safety net is elsewhere: *"No hard-fail here (the SessionsPlugin boot check owns that)"* (cookie_store.rs:154-155) and *"the [`crate::SessionsPlugin`] boot check is the hard-fail point"* (cookie_store.rs:123). **`SessionsPlugin::on_ready` (lib.rs:243-260) contains no such check** — it only seals the sliding-expiry flag and installs the store. The real empty-key hard-fail is `umbral_security::validate_secret_key`, which lives in the separate, opt-in `umbral-security` plugin (confirmed: `plugins/umbral-security/src/lib.rs`, tested in `empty_secret_key.rs`). Core's own `check.rs` (line 201) only rejects the *dev-default* string, not an empty key.

**Attack scenario.** An operator wires `SessionsPlugin::default().store(CookieStore::new())` for a stateless edge deployment, does not register `umbral-security` (it is optional and easy to omit), and ships with `secret_key` unset (empty). Boot succeeds with a single `error!` line buried in logs. Every session cookie is now AEAD-sealed under `SHA-256("")`, a key any attacker can reproduce. The attacker forges a `SessionRecord { user_id: Some("1"), ... }`, encrypts it under the known key, base64url-encodes `nonce||ct`, and sets it as the `umbral_session` cookie — instant authentication as user 1 (typically the superuser). Full auth bypass.

This violates the framework's "secure by default" principle: the secure path depends on a *second* plugin being present. The fix is to make `CookieStore` itself refuse an empty key in `Prod`:

```rust
fn resolve_ambient_key(&self) -> [u8; 32] {
    *self.ambient_key.get_or_init(|| {
        let settings = umbral::settings::get_opt();
        let secret = settings.as_ref().map(|s| s.secret_key.clone()).unwrap_or_default();
        let is_prod = matches!(settings.as_ref().map(|s| &s.environment), Some(umbral::Environment::Prod));
        if secret.trim().is_empty() {
            if is_prod {
                // A stateless store keyed off an empty secret is an auth bypass.
                // Fail closed instead of serving forgeable sessions.
                panic!("CookieStore: secret_key is empty in Prod — refusing to derive a forgeable session key");
            }
            tracing::warn!("CookieStore: empty secret_key — sessions are forgeable (dev only)");
        }
        derive_key(&secret)
    })
}
```

Better still, validate at boot in `SessionsPlugin::on_ready` when the installed store is a `CookieStore`, so the failure is at startup rather than first request. (Panicking on first request is still fail-closed, but a boot check is louder.)

---

## D. Blind spots

- **Index materialization.** Findings #4 assessed from the `Session` struct only; I did not read the migration the derive emits, so I cannot confirm whether the engine auto-indexes `user_id`/`expires_at`. If it does, #4 downgrades.
- **`umbral-security` ubiquity.** #2's severity assumes an app may omit `umbral-security`. If the framework's app builder forces that plugin (or core `check.rs` is extended to reject empty keys), #2 downgrades to MEDIUM/LOW. I only verified core `check.rs` rejects the *dev-default* string, not empty.
- **Redis deployment hardening.** No visibility into Redis auth/TLS/network exposure — the `redis://` URL in docs is plaintext; a Redis reachable without auth is a session store an attacker can read/forge, but that's infra, not this crate.
- **CSRF.** `SameSite=Lax` is the only in-crate CSRF mitigation visible; the actual CSRF token machinery lives in `umbral-security`, out of scope here.
- **Concurrency under load.** Sliding-expiry write amplification (one UPSERT/request/session) and PK-read hot path were reasoned from code, not measured.
- **`current_user` / `LoggedIn` enforcement.** Lives in `umbral-auth`; whether protected routes actually call it is that plugin's audit.

---

## E. Prioritized action plan

**Quick wins (< 1 day)**
1. Make `CookieStore` fail closed on empty `secret_key` in `Prod` (finding #2) — a few lines in `resolve_ambient_key` / `on_ready`.
2. Add indexes on `session.user_id` and `session.expires_at` (finding #4).
3. Fix the misleading doc-comments in `cookie_store.rs` that point at a nonexistent `SessionsPlugin` boot check.

**Short term (< 2 weeks)**
4. Route `revoke_user_sessions`, `destroy_session`/`logout`, `read_session`, `set_data`, and `Messages` through `active_store()`; add `SessionStore::destroy_user` (finding #1, #6). Make `CookieStore::destroy_user` return an explicit "unsupported" error.
5. Fix or explicitly gate `CookieStore` + the login helper (finding #3) — either defer the login `Set-Cookie` to the layer for stateless stores or document that CookieStore requires a different login entry point.
6. Add an absolute session-lifetime cap for sliding expiry (finding #5).

**Structural (needs design work)**
7. Reconcile the store abstraction so there is exactly one path for every session operation. The current split — layer uses the store, everything else uses raw ORM — is the root cause of #1, #3, and #6, and will keep producing store-specific correctness bugs. Consider making the free functions thin wrappers over `active_store()` and deleting the parallel DB-direct helpers.
