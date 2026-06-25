# SessionStore 2a — Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Route all umbral session storage through a `SessionStore` trait via a **request-scoped session** (task-local: load at `session_layer` entry, mutate in memory, save once at exit), with `DbStore` reproducing today's behavior **byte-for-byte**. No new backend yet — this is the seam that 2b (CookieStore) and 2c (RedisStore) plug into. **Strictly behavior-preserving: every existing umbral-sessions and umbral-auth test must stay green.**

**Architecture:** A `SessionStore` trait (`load`/`save`/`destroy`, keyed by the cookie token) with one impl, `DbStore`, wrapping today's SQL. The active store is an ambient `OnceLock<Arc<dyn SessionStore>>` installed by `SessionsPlugin`. A `RequestSession` (token + loaded record + dirty/fresh flags) lives on a `tokio::task_local!`, scoped by `session_layer`. The public functions (`set_data`/`get_data`/`current_session`/`current_user_id_str`/`login_user_id`) operate on that task-local instead of hitting the DB directly; `session_layer` saves the record via the store at response exit, preserving lazy creation (#46).

**Tech Stack:** Rust, axum 0.8 `from_fn` middleware, `async-trait`, `tokio::task_local!`, `serde_json`, the existing ORM (`Session` model) for `DbStore`.

## Global Constraints (preserve EXACTLY — from the internals map)

- **Token at rest:** `DbStore` keys rows by `hash_token(raw)` = SHA-256 hex; the raw token never touches the DB. (`hash_token`, lib.rs:197.)
- **`read_session` lazy expiry:** on read, if `expires_at < now()`, delete the row and return `None`. (lib.rs:243.)
- **Data-write TTL:** materializing/updating a session sets `expires_at = now + DEFAULT_TTL_SECONDS` (14 days), `user_id = NULL` on the lazy INSERT path. (`upsert_session_data_key`, lib.rs:414.)
- **`login_user_id`:** (1) always `destroy_session(old)` even if the read failed (fixation defense); (2) mint new session; (3) carry the **entire `data` string** to the new row, but **only if `data != "{}"`**; (4) `Set-Cookie` written to `response_headers`, not returned. (lib.rs:493.)
- **`session_layer` lazy creation (#46):** NEVER writes a row on entry; mints an in-memory candidate token when there's no live session; emits `Set-Cookie` at exit only if **(a)** the token was fresh AND **(b)** the response has no existing `Set-Cookie` AND **(c)** a row now exists. (lib.rs:844.)
- **Cookie flags:** `Path=/; HttpOnly; [Secure; ]SameSite=Lax; Max-Age=<secs>` — `Secure` omitted only in `Environment::Dev`. (`set_cookie_header_named`, lib.rs:302.)
- **`COOKIE_NAME = "umbral_session"`, `DEFAULT_TTL_SECONDS = 14*24*3600`.**
- The `Session` model + its `ModelMeta` migration stay; `DbStore` uses the same table.
- Public re-exports (`set_data`, `get_data`, `read_session`, `current_session`, `current_user_id_str`, `login_user_id`, `SessionToken`, `COOKIE_NAME`, …) keep their signatures so umbral-auth and the shop compile unchanged.

---

## File Structure

- `plugins/umbral-sessions/src/store.rs` (new): `SessionStore` trait, `SessionRecord` struct, `DbStore`, the ambient `OnceLock` + `install_store`/`active_store`.
- `plugins/umbral-sessions/src/request_session.rs` (new): `RequestSession` holder + `CURRENT_SESSION` task-local + `scope`/`current`/`with_loaded` helpers.
- `plugins/umbral-sessions/src/lib.rs` (modify): `pub mod store; pub mod request_session;`; rewrite `session_layer` to load/scope/save; rewrite `set_data`/`get_data`/`current_session`/`current_user_id_str`/`login_user_id` to use the task-local; add `SessionsPlugin.store()` builder + install in the plugin lifecycle.
- Tests: `plugins/umbral-sessions/tests/store_dbstore.rs`, `plugins/umbral-sessions/tests/request_session_layer.rs` (new); existing `tests/lazy_session.rs` and `tests/integration.rs` must still pass.

---

## Task 1: `SessionStore` trait + `SessionRecord` + `DbStore` + ambient install

**Files:** Create `plugins/umbral-sessions/src/store.rs`; modify `plugins/umbral-sessions/src/lib.rs` (`pub mod store;`, re-exports). Test: `plugins/umbral-sessions/tests/store_dbstore.rs`.

**Interfaces — Produces:**
```rust
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub user_id: Option<String>,
    pub data: String,                 // JSON object string, "{}" when empty
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[async_trait::async_trait]
pub trait SessionStore: Send + Sync + std::fmt::Debug {
    /// Load the record for a raw cookie token; None if absent or expired
    /// (expired sessions are deleted as a side effect, preserving today's
    /// lazy-expiry behavior).
    async fn load(&self, token: &str) -> Result<Option<SessionRecord>, SessionError>;
    /// Create-or-update the record under `token`. Returns the cookie value to
    /// set (equal to `token` for DbStore; differs for the future CookieStore).
    async fn save(&self, token: &str, record: &SessionRecord) -> Result<String, SessionError>;
    /// Delete the session. Idempotent.
    async fn destroy(&self, token: &str) -> Result<(), SessionError>;
}

pub fn install_store(store: std::sync::Arc<dyn SessionStore>); // sets the ambient OnceLock (idempotent: warn+ignore if already set, like umbral's pool)
pub fn active_store() -> std::sync::Arc<dyn SessionStore>;     // returns the installed store, or a default DbStore if none installed
```

`DbStore` reproduces today's SQL: `load` = the body of `read_session` (hash, `Session::objects().filter(session::ID.eq(hash)).first()`, lazy-delete-if-expired); `save` = the `upsert_session_data_key` upsert BUT writing the FULL record (id=hash(token), user_id, data, created_at, expires_at) via an `INSERT ... ON CONFLICT(id) DO UPDATE SET user_id=, data=, expires_at=` (sqlite + postgres branches, mirroring the existing dispatch on `pool_dispatched()`), returning `token` unchanged; `destroy` = `destroy_session_by_hash`.

- [ ] **Step 1: Write failing tests** — `tests/store_dbstore.rs`: boot an app with a sqlite pool + `session` table (copy the boot+CREATE TABLE harness from `tests/lazy_session.rs`), then: `DbStore.save("tok", rec)` then `load("tok")` returns the record (user_id, data round-trip); `load("missing")` → None; a record with `expires_at` in the past → `load` returns None AND the row is gone; `destroy("tok")` → subsequent `load` None. Assert the DB `id` column equals `hash_token("tok")` (token hashed at rest).
- [ ] **Step 2: Run, confirm FAIL** — `cd crates && cargo test -p umbral-sessions --test store_dbstore` → compile error (types missing).
- [ ] **Step 3: Implement `store.rs`** — define `SessionRecord`, `SessionStore`, `DbStore` (move/borrow the SQL from `read_session`/`upsert_session_data_key`/`destroy_session_by_hash`; keep `hash_token` reachable — make it `pub(crate)`). Add the `OnceLock<Arc<dyn SessionStore>>` + `install_store` (idempotent) + `active_store` (default `DbStore` if unset). Wire `pub mod store;` and re-export `SessionStore`, `SessionRecord`, `DbStore` from `lib.rs`.
- [ ] **Step 4: Run, confirm PASS** — `cargo test -p umbral-sessions --test store_dbstore`.
- [ ] **Step 5: Commit** — `feat(sessions): SessionStore trait + DbStore + ambient install`.

---

## Task 2: Request-scoped session + `session_layer` load/save (lazy creation preserved)

**Files:** Create `plugins/umbral-sessions/src/request_session.rs`; modify `lib.rs` (`session_layer`). Test: `plugins/umbral-sessions/tests/request_session_layer.rs`.

**Interfaces — Consumes** Task 1 (`active_store`, `SessionRecord`). **Produces:**
```rust
pub struct RequestSession { /* token: String, fresh: bool, record: Option<SessionRecord>, dirty: bool */ }
impl RequestSession {
    pub fn user_id(&self) -> Option<&str>;
    pub fn get_raw(&self, key: &str) -> Option<serde_json::Value>;     // read a data key from the in-memory record
    pub fn set_raw(&mut self, key: &str, val: serde_json::Value);      // mutate record.data + mark dirty (materializes record if None, user_id=None, 14d TTL)
    pub fn rotate(&mut self, user_id: Option<String>, carry_data: bool); // login: new token, new record, optional carry of old data string
    pub fn token(&self) -> &str;
}
pub fn current<R>(f: impl FnOnce(&RequestSession) -> R) -> Option<R>;   // read the task-local if scoped
pub fn current_mut<R>(f: impl FnOnce(&mut RequestSession) -> R) -> Option<R>; // RefCell-backed; None if not in a request scope
```

`session_layer` becomes:
1. Entry: `token = cookie_from_headers(req)`; `record = match token { Some(t) => active_store().load(&t).await?, None => None }`. `fresh = record.is_none()`; if `fresh`, `token = Uuid::new_v4()`.
2. Build `RequestSession { token, fresh, record, dirty: false }`, scope it on `CURRENT_SESSION` (a `task_local!` holding `RefCell<RequestSession>`), still insert `SessionToken(token)` + `SessionFresh` extensions for back-compat.
3. `response = scope(rs, next.run(req)).await` — after the future resolves, recover the (possibly mutated) `RequestSession`.
4. Exit: if `dirty`, `let cookie_val = active_store().save(rs.token(), rs.record()).await?`. Then preserve the Set-Cookie guards: emit `set_cookie_header(&cookie_val, None)` only if `fresh && !response.has(SET_COOKIE) && dirty` (a dirty fresh session = a row now exists — replaces the old "read_session confirms a row" probe with the in-memory `dirty` flag, equivalent and DB-free).

- [ ] **Step 1: Write failing tests** — `tests/request_session_layer.rs` (own binary, sqlite boot harness): (a) a handler that does NOT write the session → response has **no** `Set-Cookie` and the `session` table has **0 rows** (lazy creation preserved); (b) a handler that calls `current_mut(|s| s.set_raw("k", json!(1)))` → response **has** `Set-Cookie` and **1 row** exists with that data; (c) a request carrying a live session cookie + a non-writing handler → no new row, no Set-Cookie.
- [ ] **Step 2: Run, confirm FAIL.**
- [ ] **Step 3: Implement** `request_session.rs` (the holder + `CURRENT_SESSION` task-local with `RefCell`, `scope`/`current`/`current_mut`) and rewrite `session_layer` per the flow above. Keep `SessionToken`/`SessionFresh` insertion.
- [ ] **Step 4: Run, confirm PASS** + run `cargo test -p umbral-sessions --test lazy_session` (the #46 regression test) — MUST stay green.
- [ ] **Step 5: Commit** — `feat(sessions): request-scoped session + lazy load/save in session_layer`.

---

## Task 3: Route the public functions through the request-scoped session

**Files:** Modify `lib.rs` (`set_data`, `get_data`, `current_session`, `current_user_id_str`, `login_user_id`, `Messages`). Test: extend `tests/integration.rs` usage + a new `tests/request_session_data.rs`.

**Interfaces — Consumes** Tasks 1+2. Signatures of the public functions are **unchanged** (callers untouched); only bodies change.

Behavior:
- `set_data(token, key, value)`: if a request scope is active AND its token matches, `current_mut(|s| s.set_raw(key, json))`; else (legacy/out-of-request) fall back to `active_store().save` after loading (preserve direct-write semantics for non-request callers). **In-request path does NOT hit the DB** — the save happens at layer exit.
- `get_data(session, key)`: unchanged signature (takes a `&Session`); still parses `session.data`. (Reads that go through `current_session` get the in-memory record — see below.)
- `current_session(headers)`: if a request scope is active, build a `Session` view from the task-local `RequestSession.record` (no DB); else fall back to today's `read_session`. Preserves the return type.
- `current_user_id_str(headers)`: `current(|s| s.user_id())` if scoped (no DB — this is the hot path the benchmark hit); else fall back.
- `login_user_id(req, resp, user_id)`: if scoped, `current_mut(|s| s.rotate(user_id, carry))` + write the Set-Cookie to `resp` using `s.token()`; preserve the **destroy-old + carry-if-not-"{}"** semantics. Else fall back to today's create+carry path.

- [ ] **Step 1: Write failing test** — `tests/request_session_data.rs`: a handler that `set_data("k", 5)` then a SECOND request (same cookie) whose handler reads `get_data` / `current_user_id_str` sees the persisted value; assert the in-request `set_data` performed the write at exit (1 row), and a login rotates the cookie token (new `Set-Cookie`, old session destroyed). Use a query counter or row-count assertions to confirm the in-request read path does not re-query mid-request.
- [ ] **Step 2: Run, confirm FAIL.**
- [ ] **Step 3: Implement** the body rewrites with the in-request / fallback branches above.
- [ ] **Step 4: Run the WHOLE umbral-sessions + umbral-auth suites** — `cd crates && cargo test -p umbral-sessions && cargo test -p umbral-auth`. Every existing test MUST pass (behavior-preserving is the gate). Also `cargo build -p umbral` (facade).
- [ ] **Step 5: Commit** — `refactor(sessions): route public API through the request-scoped session`.

---

## Task 4: `SessionsPlugin.store()` builder + install in lifecycle

**Files:** Modify `lib.rs` (`SessionsPlugin` struct + `Default` + builder + `Plugin` lifecycle). Test: `plugins/umbral-sessions/tests/plugin_store.rs`.

**Interfaces — Produces:** `SessionsPlugin::default().store(impl SessionStore + 'static)`; default = `DbStore`. The store is `install_store`'d during the plugin's build/`on_ready` lifecycle so `active_store()` returns it.

- [ ] **Step 1: Write failing test** — boot an app with `SessionsPlugin::default().store(DbStore::default())`; assert `active_store()` is installed and a round-trip through `session_layer` + `set_data` persists via it.
- [ ] **Step 2–4:** add `store: Arc<dyn SessionStore>` to the struct, update the hand-written `Default` to `DbStore`, add `.store()` builder, install in the plugin lifecycle; run the test + the full suites.
- [ ] **Step 5: Commit** — `feat(sessions): SessionsPlugin.store() builder + lifecycle install`.

---

## Self-Review

- **Spec coverage (spec §3 Component 2, foundation half):** the trait + DbStore + the request-scoped seam are Tasks 1–4; CookieStore (2b) and RedisStore (2c) are separate plans that implement `SessionStore` against this seam. Not a gap.
- **Behavior preservation is the load-bearing gate:** every task ends by running the existing `lazy_session.rs` / `integration.rs` / umbral-auth suites. The riskiest invariants (lazy creation #46, login rotation + carry, cookie flags) each have an explicit preserve-this line in Global Constraints and a covering assertion.
- **Type consistency:** `SessionRecord` is the single record type across `store`/`request_session`/the public functions; `save` returns the cookie value (token for DbStore) consistently in Tasks 1–3.
- **Known risk:** the in-request-vs-fallback branch in Task 3 (callers inside a request use the task-local; callers outside fall back to direct store I/O). The fallback path keeps non-request callers (e.g. background jobs creating sessions) working; the test must cover both.
