# Lazy Middleware & Request Identity — Design

**Status:** approved (brainstorm), pending implementation
**Date:** 2026-06-17
**Goal:** Make umbra's auth/session middleware cheap by turning it from *eager per-request I/O* into *lazy capability injection*: a request pays for identity only if it actually uses it, resolves it at most once, and never blocks the response on bookkeeping writes.

---

## 1. Motivation (measured)

With proper load tools (`wrk`/`oha`, core-pinned, warmed), minimal-vs-maximalist umbra diverges hard:

| | umbra-hello (model only) | shop (full stack) |
|---|---|---|
| `/bench/text` | 640k req/s | 189k |
| `/bench/notes/read` (40k rows) | 18.2k | **1.3k** |

The shop's collapse is **not** the framework or ORM (hello ≈ cot at every endpoint). It is the plugin middleware doing **eager, duplicated, on-critical-path DB work**:

- `user_context_layer` (`session_user.rs:263`) resolves the user **eagerly on every request** — *including JSON responses that never read `user`* — at a cost of **2 + N** queries (session + user + up to `USER_RELATION_DEPTH=2` relation hops).
- Identity is re-resolved **independently in 5+ places** (`user_context_layer`, `resolve_identity`, `current_user`, `resolve_user<U>`, each `Authentication` impl). A templated page that also uses a `LoggedIn<U>` extractor does the **session+user lookup twice**.
- Side-effect **writes sit on the response path**: bearer `last_used_at` UPDATE (`token.rs`), `last_login` UPDATE (`session_user.rs:108`).
- All of the above contend for a ~10-connection SQLite/PG pool under load.

**Target:** an anonymous/JSON request does **0** identity queries; an authenticated request resolves identity **once**; bookkeeping writes happen **after** the response is flushed. Expected: shop read ~1.3k → ~15k+ (toward hello's 18k).

---

## 2. Unifying principle

**Middleware injects lazy capabilities; it does not do eager work.** Mounting the auth plugin costs ~nothing. The cost of "who is this user?" appears only when a handler/template/permission check actually asks — and then exactly once, from a per-request cache, with side-effects deferred off the write path.

---

## 3. Components

Four components, built and shipped in dependency order. Each is independently valuable and benchmarked.

### Component 1 — `RequestIdentity`: lazy, memoized, shared *(keystone)*

**Where it lives.** The memoized identity lives in **umbra-auth**, not umbra-core. `RouteContext` (core) demonstrates the pattern (per-request task-local + typed extensions + a scope layer), but `Identity` is an umbra-auth type and core must not depend on a plugin. umbra-auth therefore owns a sibling task-local that mirrors `RouteContext`'s mechanics.

**Types (umbra-auth):**
- `ResolvedIdentity` — reuse the existing `Identity` (`user_id: String` polymorphic PK, `is_staff`, `is_authenticated`, `extra`). Model-agnostic, cheap.
- `LazyIdentity` — `Arc<tokio::sync::OnceCell<Option<Identity>>>` plus the request `HeaderMap` (clone) needed to resolve. Resolution runs the existing chain: session-first (`current_user_id_str` → user existence/staff), then bearer fallback. The `OnceCell` guarantees **at most one** resolution per request.

**Reachability (two mirrors, one value):**
1. A task-local `CURRENT_IDENTITY: Arc<LazyIdentity>`, scoped per request by a new `identity_layer` (umbra-auth middleware), so deep code (templates, ORM-adjacent) can call `umbra_auth::identity::current().await` and get the memoized value.
2. The same `Arc<LazyIdentity>` inserted into `request.extensions()` so `FromRequestParts` extractors (which see `Parts`, not the task-local cleanly) read the same handle.

**Consumers rewired to read the memoized handle first, query only on miss:**
- `user_context_layer` → **lazy**: stop eagerly resolving. Provide the template `user` as a value that resolves through `LazyIdentity` on **first access** (see §6 Open Questions for the minijinja lazy-value mechanism). A JSON response never triggers it.
- `resolve_identity`, `current_user`, `OptionalIdentity`/`CurrentIdentity`, the `Authentication` impls → consult `CURRENT_IDENTITY` before issuing their own query.
- `LoggedIn<U>` (needs the full `U` row, not just `Identity`): reuse the memoized `user_id` to skip the session lookup, then fetch the row once; memoize the typed row in `request.extensions()` as `insert::<U>()` so repeated `LoggedIn<U>` extractions in one request hit cache.

**Result:** session lookup happens at most once; user-row fetch at most once per model type; zero work when unused.

### Component 2 — `SessionStore` trait: pluggable backend

Today session storage is hardcoded to DB rows via the ambient ORM pool (`umbra-sessions/src/lib.rs`). Introduce:

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn load(&self, token_hash: &str) -> Result<Option<SessionData>, SessionError>;
    async fn store(&self, token_hash: &str, data: &SessionData, ttl: Duration) -> Result<(), SessionError>;
    async fn touch(&self, token_hash: &str, ttl: Duration) -> Result<(), SessionError>;
    async fn destroy(&self, token_hash: &str) -> Result<(), SessionError>;
}
```

Backends:
- **`CookieStore`** — session data signed (and optionally encrypted) and stored *in the cookie*. **Zero backend I/O.** Bounded ~4 KB; no server-side force-invalidation (documented trade-off). Reuses the security plugin's signing key.
- **`RedisStore`** — separate connection pool (no app-DB contention); ~µs lookups. Behind a `redis` cargo feature so a DB-only app pulls no redis dep.
- **`DbStore`** — the current behavior, refactored behind the trait (default).

App selects: `SessionsPlugin::default().store(RedisStore::new(url))` / `.cookie_store(...)`. Component 1 resolves through the active store, so a cookie store ⇒ zero-roundtrip identity.

### Component 3 — Deferred side-effects: off the response path

A small primitive in umbra-core:

```rust
/// Run `fut` after the current response is produced, without blocking it.
pub fn defer<F: Future<Output = ()> + Send + 'static>(fut: F);
```

Implemented by spawning on the runtime (the work is best-effort and already swallows errors). Apply to: bearer `last_used_at`, `last_login` (`session_user.rs:108`), session expiry-`touch`. The response never awaits these. Errors are logged, never surfaced.

### Component 4 — Per-route middleware scoping *(lowest priority)*

Let route groups declare a middleware profile so public/static/health routes get a thin chain (no session/auth layers installed at all). **Caveat:** once Components 1+3 land, an *unused* auth layer already costs near-zero (lazy, no I/O), so this is the smallest remaining win — included for completeness and for the case where even a layer's `poll`/extension-insert is measurable. Mechanism: a routes-builder grouping that applies a layer set to a subset, layered in `app.rs` alongside the existing `wrap_router` walk.

---

## 4. Phasing

1. **Component 1** (lazy memoized identity) — biggest win. Prove shop `/bench/notes/read` ~1.3k → ~15k+ with wrk.
2. **Component 3** (defer) — small; removes write-path stalls (login, bearer touch).
3. **Component 2** (`SessionStore` + cookie + redis) — unlocks zero-I/O sessions.
4. **Component 4** (per-route scoping) — polish.

Each phase: implement → unit/behavioral tests → wrk benchmark on the shop → commit.

---

## 5. Testing strategy

- **DB-round-trip counter (behavioral, the core proof).** A test-only ambient query counter (increment on each ORM terminal). Assert:
  - anonymous request to a JSON endpoint → **0** identity queries.
  - authenticated templated page + `LoggedIn<U>` extractor → **1** session lookup + **1** user fetch (not 2× each).
  - `user_context_layer` mounted but template doesn't read `user` → **0** identity queries.
- **wrk before/after** each phase on the shop read/write (real-world proof, the numbers in §1).
- **Correctness regressions:** identity is consistent within a request (same value on repeated access); logout/destroy invalidates; cookie store round-trips a session across requests; deferred effects actually execute (await a signal in a test); bearer + session precedence unchanged.
- **No-plugin regression:** umbra-hello numbers must not drop (the lazy machinery is auth-plugin-only; core stays free).

---

## 6. Open questions / risks (decided defaults)

1. **Lazy template `user`.** minijinja renders synchronously, but resolution is async. Default approach: `user_context_layer` resolves the `LazyIdentity` **once, lazily, just-in-time before the first template render in the handler** via a render hook that checks whether the template references `user`; if the engine can't tell, fall back to resolving on first `get` of the `user` global through a custom `Object`/dynamic value that triggers a `block_in_place`-free pre-resolution. **Fallback if neither is clean:** keep `user_context_layer` resolving eagerly *but only when the response is HTML* (cheap content-type gate) — still removes the cost from all JSON/API routes, which is where the measured collapse is. This fallback alone captures most of the win; the fully-lazy template value is a stretch goal.
2. **Generic user model.** Identity is model-agnostic (good). `LoggedIn<U>` row memoization is keyed by `U` via `request.extensions().insert::<U>()`.
3. **Redis dependency.** Gated behind a `redis` feature on umbra-sessions; not pulled by default.
4. **Cookie store size/invalidation.** Documented limits; not the default. Signing key sourced from the security plugin; error if absent.
5. **Always-on identity scope layer.** `identity_layer` is installed by the auth plugin's `wrap_router` (only when the auth plugin is present), so non-auth apps (hello) are untouched.

---

## 7. Crate touchpoints

- **umbra-auth:** `identity` module (task-local + `LazyIdentity` + `identity_layer`); rewire `session_user.rs` (`user_context_layer`, `current_user`), `extractors.rs` (`resolve_identity`, `Optional/CurrentIdentity`), `login_required.rs` (`LoggedIn<U>`, `resolve_user`), `bearer_auth.rs` (consult memo + defer touch).
- **umbra-sessions:** `SessionStore` trait + `DbStore`/`CookieStore`/`RedisStore`; route `set_data`/`read_session`/`create_session`/`touch` through the active store; defer expiry-touch.
- **umbra-core:** `defer()` primitive (Component 3); per-route middleware grouping (Component 4). No dependency on any plugin.
- **examples/shop:** the benchmark target; no code change required to measure (uses the plugins).

---

## 8. Non-goals

- Caching identity **across** requests (token→user LRU/Redis cache with TTL) — a valuable later optimization, explicitly out of scope here to keep correctness simple (no cross-request staleness/invalidation to reason about yet).
- Rearchitecting the `Authentication` trait surface — it stays; we only make its result memoized and its callers lazy.
