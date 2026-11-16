# review_4 · umbral-sessions

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 5848 LOC (`plugins/umbral-sessions`) |
| **Reviewer** | review_4 wave 1 (sessions + oauth) |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 2 high · 2 medium · 3 low · 1 info — 8 new, 3 known |

## Summary

- The crate is the swappable session layer: a `SessionStore` trait with Db (default), Cookie (stateless XChaCha20Poly1305), and Redis (feature-gated) implementations, a `session_layer` middleware with lazy row materialisation, request-scoped session access, login/logout/revocation primitives, sliding + absolute expiry policy, and flash `Messages`. The store abstraction and token-at-rest hygiene are in genuinely good shape — every audit_2 store-routing item I re-verified holds.
- The two HIGHs sit at the edges of the newest surfaces. SESS-1: the `max_session_age()` absolute cap (audit_2 #5) is enforced in exactly one reader, `read_session`, while every authed handler resolves through the in-request fast paths and `session_layer`'s entry load — so the cap never fires on the hot path, and `umbral-auth`'s `current_session_user_pk` docstring claims it does. SESS-2: a failed `store.save` at the layer's single exit point is `tracing::warn!`-ed and the request completes, so the browser is handed a cookie for a session that was never persisted — contradicting the "fails loudly" contract on `SessionError::CookieTooLarge`.
- The MEDIUMs are a real in-request read/write split-brain in `Messages` (reads hit the persistent store while writes land in the in-memory record, so two `add`s in one request lose the first), and two session *creation* sites that bypass `active_store()` — the read/destroy/set_data sites were routed by audit_2, these were missed.
- Fix order for the maintainer: SESS-1 (security control that is currently dead code in practice) → SESS-2 (silent loss of a login/session) → SESS-3/SESS-4 (bounded), then the LOW/INFO cleanup.
- Tests are structurally strong (per-store behavioral suites, expiry, revocation, cookie flags) but systematically miss both HIGHs: `max_session_age.rs` calls `read_session` directly, and no test exercises two `Messages::add`s inside one request scope.

## Findings

### SESS-1 · HIGH · security · self-verified
**`max_session_age()` is enforced only in `read_session`, but the request hot path (`session_layer` entry load + the in-request fast paths of `current_session`/`current_user_id_str`) never consults it, so an over-age session keeps authenticating for the life of its cookie.**

- **Where**: enforcement `plugins/umbral-sessions/src/lib.rs:613-624`; bypassing entry load `lib.rs:1408-1410`; fast paths `lib.rs:790-806` and `lib.rs:855-858`; consumer claim `plugins/umbral-auth/src/login_required.rs:304-310`
- **Evidence**:
  ```rust
  // lib.rs:613 — the ONLY read of max_session_age() in the crate:
  if let Some(max_age) = max_session_age() {
      let age = Utc::now() - rec.created_at;
      if age > Duration::seconds(max_age) { ... return Ok(None); }
  }
  // lib.rs:855-858 — current_user_id_str, what every extractor resolves through:
  if let Some(uid) = handled {
      return Ok(uid);
  }
  // lib.rs:1408 — session_layer entry, no cap check either:
  Some(t) => match store.load(&t).await {
      Ok(Some(rec)) => (t, false, Some(rec)),
  ```
  And the consumer docstring it contradicts (`plugins/umbral-auth/src/login_required.rs:304-310`): "resolves via the active store and applies both checks" — the cap half is false on this path.
- **Impact**: Trace it concretely. An app calls `SessionsPlugin::default().max_session_age(3600)`. A browser holds a 10-hour-old session cookie; sliding expiry has kept `expires_at` fresh so the store still loads the record. A request arrives: `session_layer` loads it (no cap), bumps expiry in memory, scopes `CURRENT_SESSION`; the handler's `LoginRequired` extractor calls `umbral_sessions::current_user_id_str(...)` inside that scope → the `handled` fast path returns the user id with no cap check → the request is authenticated. Nothing in the normal request path ever reaches line 613; the cap is effectively dead unless a caller invokes `read_session`/`current_session` out-of-request (or with a mismatched cookie). Worse, enforcement is path-dependent *within* one request: if the handler reads flash messages, `Messages::read` → `read_session` destroys the over-age row mid-request, and the exit-time sliding save (dirty record, `upsert`) then re-creates it. The `max_session_age` test calls `read_session` directly (`tests/max_session_age.rs`), which is why this shipped green.
- **Fix**: enforce the cap once where the record enters the request — in `session_layer` right after `store.load` (destroy + fall through to the fresh-token branch), so both fast paths and `read_session` inherit it — and make the fast-path readers fall back to `read_session` when `max_session_age().is_some()`. Then fix the docstring in `umbral-auth` to match.

### SESS-2 · HIGH · correctness · self-verified
**`session_layer` swallows a failed `store.save` (`tracing::warn!` only) and still returns a normal response — including a Set-Cookie built from the raw token — so a request reports success while its session was never persisted.**

- **Where**: `plugins/umbral-sessions/src/lib.rs:1470-1477` (swallow), `lib.rs:1499-1513` (raw-token fallback emit), contract at `lib.rs:476-480`
- **Evidence**:
  ```rust
  // lib.rs:1473-1475
  match store.save(rs.token(), rec).await {
      Ok(value) => cookie_value = Some(value),
      Err(e) => tracing::warn!("session_layer: store.save failed: {e}"),
  }
  // lib.rs:1509-1510 — the emit still runs, falling back to the raw token:
  let value_to_set = cookie_value.as_deref().unwrap_or(&token);
  let cookie = set_cookie_header(value_to_set, None);
  ```
  And the contract being broken (`lib.rs:476-480`): "Raised by `CookieStore::save` so an oversized session fails loudly rather than silently producing a cookie the browser drops."
- **Impact**: Two concrete cases. (1) Under `CookieStore`, a session whose serialised record exceeds the 4 KB cap makes `save` return `CookieTooLarge`; the layer logs a warning, `rs.is_dirty()` is still true so `row_exists` is true, `cookie_value` is `None`, and the browser is set a cookie containing the bare UUID — the *unusable* state the docstring says cannot happen silently; the next request decrypts nothing and is anonymous. (2) Under `DbStore`, a transient pool/DB error during the exit save of a login (`login_user_id` rotated the token and set its own Set-Cookie) means the client is authenticated-looking with a token whose row does not exist; the login response is a success. Every handler that wrote session state has already returned 200 by the time the write is attempted, so no caller can observe or compensate for the failure.
- **Fix**: the layer is the last place the failure is still recoverable — when a dirty session fails to persist and the response is otherwise successful, fail the request (5xx) instead of emitting a cookie that resolves nowhere; at minimum raise to `tracing::error!` and attach a response extension so app middleware can detect it. `CookieTooLarge` should surface as a real error response, not a warn.

### SESS-3 · MEDIUM · correctness · self-verified
**In-request `Messages::add` reads through `read_session` (the persistent store) while its write lands in the in-memory request record via `set_data`'s fast path — so two `add`s in one request silently drop all but the last message.**

- **Where**: `plugins/umbral-sessions/src/lib.rs:1215-1225` (`add`), `:1286-1295` (`read`), `:928-943` (`set_data` in-request fast path)
- **Evidence**:
  ```rust
  // lib.rs:1219-1224 — add: read from the store, write to memory
  let mut current = self.read(token).await.unwrap_or_default();
  current.push(Message { level, text: text.into() });
  let _ = super::set_data(token, SESSION_KEY, &current).await;
  // lib.rs:1287 — read: always the persistent store, never the scoped record
  let session = match super::read_session(token).await? { ... };
  ```
- **Impact**: Inside a request, `set_data` short-circuits to `current_mut(...)` and only the exit-time `store.save` persists it — so the store still holds the *pre-request* state at every intermediate `add`. Handler adds `m1`, then (say, auth middleware or a second call) adds `m2`: both reads return the pre-request queue, and the second `set_data` overwrites the record's flash key with `[m2]` — `m1` is gone. The same split makes `add` followed by `peek` in one request show an empty queue. The existing round-trip test (`messages_add_and_drain_round_trip`) runs out-of-request, where the fallback read-modify-write path is correct, so the suite cannot catch this.
- **Fix**: make `Messages::read` consult the request-scoped record when the token matches (the pattern `current_session`/`current_user_id_str` already use) — or route the read through a helper that prefers the in-memory record in-request and the store otherwise. One behavioral test: two `add`s inside a real request scope, then drain.

### SESS-4 · MEDIUM · drift · self-verified
**`create_session` and `login_user_id`'s out-of-request fallback write through `Session::objects()` directly instead of the installed store, so under CookieStore/RedisStore they create a session no store will ever resolve.**

- **Where**: `plugins/umbral-sessions/src/lib.rs:573-591` (`create_session`), `:1031-1045` (fallback in `login_user_id`); the missed-by-audit_2 evidence is `tests/set_data_non_db_store.rs:81-82`
- **Evidence**:
  ```rust
  // lib.rs:580-591 — no active_store() anywhere:
  Session::objects()
      .create(Session { id: stored_id, user_id, data: "{}".to_string(), created_at: now, expires_at })
      .await?;
  // lib.rs:1031 — and the login fallback calls it, then patches via the ORM:
  let token = create_session(user_id_str, None).await?;
  ```
  The crate's own test documents the workaround (`tests/set_data_non_db_store.rs:81-82`): "Seed a session directly in the store (as create_session would, but via the store since there's no SQL table)".
- **Impact**: audit_2 #1/#6 routed load/save/destroy/revoke/set_data through `active_store()` but not these two creation sites. Concrete: an app installs `CookieStore` and logs users in from a path without a request scope (a background/worker login, a test harness, a custom flow) — `login_user_id` falls back to `create_session`, which inserts a SQL row; the returned raw token then resolves to `None` from the active store, so the "successful" login has no session, and under Redis the orphan row is never read or purged. `create_session` is a documented public primitive; nothing in its docs scopes it to DbStore.
- **Fix**: give `SessionStore` a creation path (or have `create_session`/the fallback call `active_store().save(token, &record)`, minting the token first), so every creation goes through the store like every read and destroy now does. If DbStore-only is intended, say so in the docs and assert the installed store — silently writing a row nobody reads is the worst of the three options.

### SESS-5 · LOW · correctness
**`Messages::add` and `peek` still use `unwrap_or_default()` on a store read error — the exact pattern `drain` was fixed to stop using.**
- **Where**: `plugins/umbral-sessions/src/lib.rs:1219` (`add`), `:1279-1284` (`peek`), fixed sibling at `:1256-1269`
- **Evidence**:
  ```rust
  // lib.rs:1279-1284 — peek:
  self.read(token).await.unwrap_or_default()
  ```
- **Impact**: on a store error, `peek` renders "no messages" and `add` silently restarts the queue from empty (the pre-existing messages it failed to read are overwritten). `drain`'s comment at `:1258-1262` explains why that fallback is wrong; these two siblings drifted. Fails closed (no data leak) and the store error is likely already logged downstream, hence LOW.
- **Fix**: mirror `drain`: log the error and return empty *without* writing for `peek`; for `add`, skip the write (or propagate) rather than overwriting a queue that could not be read.

### SESS-6 · LOW · correctness
**`clearsessions` is store-blind: it sweeps the SQL `Session` table regardless of the installed store.**
- **Where**: `plugins/umbral-sessions/src/lib.rs:445-465`
- **Evidence**:
  ```rust
  let deleted = Session::objects()
      .filter(session::EXPIRES_AT.lt(now))
      .delete()
      .await
  ```
- **Impact**: under `CookieStore`/`RedisStore` the command prints "Deleted 0 expired session(s)" against an unused table (Redis keys self-expire via TTL, so nothing leaks); under any store it also skips rows past `max_session_age` whose sliding `expires_at` is still in the future, which linger until sliding expiry. Misleading output, wasted query, no data hazard. Cold path (cron/CLI).
- **Fix**: add a `SessionStore::purge_expired()` with a default no-op (DbStore implements the delete, Redis explains in its output that TTLs own expiry), and have the command report per-store. At minimum, note "DbStore only" in the command help.

### SESS-7 · LOW · docs
**The `Session` model's docstring describes a session id that does not exist anywhere.**
- **Where**: `plugins/umbral-sessions/src/lib.rs:155-160`
- **Evidence**:
  ```rust
  /// `id` is a random UUID written to the client cookie. `user_id` is
  ```
- **Impact**: false twice over — at rest the column holds `sha256(raw token)` (`create_session`'s own docstring at `:560-565` says so), and under `CookieStore` the cookie holds an encrypted record blob, not any id. An app author debugging by matching the cookie value against `session.id` finds nothing.
- **Fix**: rewrite as "`id` is the SHA-256 of the raw session token; the raw token exists only in the client cookie (or inside the CookieStore blob)".

### SESS-8 · INFO · correctness
**Routed core lead, assessed for sessions: `DbStore::save` persists via `Session::objects().upsert()` after every request, and core's `upsert` runs none of the write lifecycle `create` runs — no validators, no M2M seeding, no `pre_save`/`post_save` signals, no audit hook (`crates/umbral-core/src/orm/queryset/mod.rs:5627-5683`, core slice 1 QW-2/QW-4).**
- **Where**: `plugins/umbral-sessions/src/store.rs:183-196`; core site `crates/umbral-core/src/orm/queryset/mod.rs:5627-5683`
- **Observation**: For `Session` specifically nothing is corrupted today: the model has no M2M fields, no validators, and is not audited (verified against the model at `lib.rs:169-215`). The session-side consequence is accounting, not data: creations through `create_session` fire signals, but the per-request upsets that carry essentially all session traffic fire none — so any app subscribing to `post_save` on the session table (or auditing it) silently observes "no session activity". No action for this crate; listed so the core slice's `upsert` fix gets a session-side behavioral test rather than only a queryset-level one.

## Strengths

- **The store abstraction is real, not aspirational.** After audit_2, `load`/`save`/`destroy`/`destroy_user` all route through `active_store()`, `set_data`'s out-of-request fallback does a read-modify-write through the store, and `CookieStore` declares `RevocationUnsupported` instead of pretending a stateless store can revoke. The store-selection matrix is covered behaviorally per store (`tests/cookie_store.rs`, `redis_store.rs`, `store_dbstore.rs`, `plugin_store.rs`), including a HashMap-backed store with no SQL table proving non-DB routing (`set_data_non_db_store.rs`).
- **Token-at-rest hygiene.** The raw token never reaches the database (`id = sha256(token)`), lookup is by hash on an indexed PK, and the `session_cookie_present` guard is deliberately scoped to the session cookie so an unrelated CSRF cookie can't suppress the session Set-Cookie (gaps3 #32, regression-tested).
- **Lazy materialisation is a genuinely good design.** `session_layer` writes nothing on entry; a row appears iff a handler mutates the scoped record, and the sliding-expiry bump shares that one exit-time write by design (comment at `lib.rs:1432-1445` explains why two writes would clobber each other). Anonymous and asset requests leave zero rows.
- **Boot-time secret posture.** `SessionsPlugin::on_ready` hard-fails in Prod on an empty/dev-default secret when the store `requires_ambient_secret()`, independent of umbral-security (audit_2 #2 verified held), and `SameSite=None` forces `Secure` (`cookie_security_attrs`, `lib.rs:134-146`).
- **Fixation defense on both login paths.** The old session row is destroyed before the new token is issued, on the in-request and fallback paths alike, and revocation/`logout` are idempotent.

## Clean areas checked

- **ORM-only / no raw SQL**: `grep -rn "sqlx::query" plugins/umbral-sessions/src/` returns zero hits; every row write goes through `Session::objects()` or the store trait, with the audit_2 #6 comment recording why the old raw-SQL path was deleted.
- **No `unsafe`** anywhere in the crate (`grep -rn "unsafe"` is empty). Panic sites are server-constructed values only: `cookie.parse().expect("cookie value parses")` on headers the crate itself built (`lib.rs:1006/1049/1109`) and the documented OS-CSPRNG `expect` in `cookie_store.rs:310`.
- **Cookie flags**: `HttpOnly` always; `SameSite` configurable (Lax default, Strict, None→Secure-forced); `Secure` on https or None; configurable cookie name (`lib.rs:100-146`, `:704-741`).
- **Sliding vs absolute expiry arithmetic**: sliding bump = `now + DEFAULT_TTL` on live loaded sessions only; absolute cap compares `now - created_at` (not `expires_at`) — the arithmetic is right, the coverage of the cap is the bug (SESS-1).
- **Data key isolation**: flash messages live under their own `SESSION_KEY` inside the `data` JSON map, so app keys and the flash queue cannot collide; app-supplied serde round-trips through `serde_json::to_value`.
- **Middleware ordering vs auth**: session resolves inside the request scope the layer establishes (`app.rs` topological `wrap_router`), and auth's extractors call the sessions API — no circular dep, sessions has no plugin dependency at all.
- **Per-request fast paths are otherwise sound**: the cookie-token equality guard means only the addressed session is served from memory; a mismatched cookie correctly falls through to the store.

## Known issues already tracked elsewhere (not counted in the totals)

- `audit_2 plugin-sessions #3` — CookieStore login Set-Cookie handshake (login emits a raw UUID the store can't decode) — deferred, still open. Related in shape to SESS-2 but a distinct defect: #3 breaks the login helper under CookieStore on the *success* path; SESS-2 is the failed-write path. `[open]`
- `audit_2 plugin-sessions #7` — `__Host-` cookie prefix not adopted ("left as a future enhancement"); the SameSite half shipped. `[open]`
- `gaps6 #8` — framework scaling as a default runtime posture (Redis-capable sessions among the pieces an app must assemble itself). `[open]`
- `audit_2 plugin-sessions #1` — store-routed revocation/read/destroy (H7). Re-verified in tree: `read_session`/`destroy_session`/`revoke_user_sessions` route through `active_store()`; `CookieStore` returns `RevocationUnsupported` and callers log it. `[fixed/verified held]`
- `audit_2 plugin-sessions #2` — CookieStore empty-secret Prod hard-fail. Verified in `SessionsPlugin::on_ready`. `[fixed/verified held]`
- `audit_2 plugin-sessions #4` — indexes on `user_id`/`expires_at`. Both carry `#[umbral(index)]` in the model. `[fixed/verified held]`
- `audit_2 plugin-sessions #5` — absolute max-age cap added, off by default, tested via `read_session`. The cap exists and works on the path it covers, but see SESS-1: the enforcement does not reach the request hot path, so the tracker's "a session past its absolute lifetime can no longer authenticate" claim holds only for out-of-request reads. `[fixed/verified held]` (with SESS-1 as the open residual)
- `audit_2 plugin-sessions #6` — `set_data`/`Messages` routed through the store out-of-request. Verified. `[fixed/verified held]`
- `review_3` — `DbStore::save` writes via `Session::objects().upsert(...)` rather than raw dialect SQL. Verified at `store.rs:174-198`; see SESS-8 for the lifecycle divergence this legitimately inherits from core's `upsert`. `[fixed/verified held]`
- `gaps3 #32` — session Set-Cookie emit guard scoped to the session cookie so it coexists with the CSRF cookie. Verified, regression test present. `[fixed/verified held]`

## Out-of-scope notes

- `crates/umbral-core/src/orm/queryset/mod.rs:5627-5683` — `upsert()` skips validation/M2M seeding/signals/audit that `create()` runs; the session-side consequence is SESS-8 (no live impact), the fix belongs to core slice 1 (QW-2/QW-4).
- `plugins/umbral-auth/src/login_required.rs:304-310` — its docstring claims `current_user_id_str` "applies both checks" (store routing + max-age); SESS-1 shows the max-age half is false on the in-request path. The code fix lands in umbral-sessions; the auth doc needs updating in the same change. Separately, `current_session_user_pk` (`:297-316`) maps any `SessionError` to `None` via `.ok()` at `:313`, so a store failure reads as "anonymous" — fail-closed, but the error is discarded (CLAUDE.md's `.ok()` pattern); flagging for the auth reviewer.
- `plugins/umbral-permissions/src/middleware.rs:200-240` — reads the raw `current_user_id_str` but enforces deactivation itself via `pre_perm_check`; noted so the permissions reviewer doesn't double-report the primitive's missing `is_active` knowledge (it cannot know the user model by design).
- `plugins/umbral-admin/src/lib.rs:1041` — consumes the same `configured_same_site()` accessor for its SameSite=None CSRF warning; cross-checked that the accessor's contract (sealed at sessions `on_ready`, read later via topological on_ready order) matches the consumer's assumption. No defect.
