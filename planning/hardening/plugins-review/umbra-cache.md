# umbra-cache — Holistic Review

> Date: 2026-06-16 · Read-only audit · Files read: `Cargo.toml`, `src/lib.rs`, `src/cache_page.rs`, `tests/integration.rs`, `tests/cache_page.rs`, `tests/redis_backend.rs`

---

## Verdict

`umbra-cache` is a well-scoped v0 plugin: three backends (memory, SQLite, Redis), view-level `cache_page` middleware, and a clean `CacheBackend` trait. The architecture is sound — raw `sqlx` is used only for `SqliteBackend` DDL and row ops, with a documented and accurate rationale for the ORM-bypass exception. The two BROKEN markers in the source are already self-described and partially addressed (BROKEN-12 logs are in place; BROKEN-9 `on_ready` wiring is implemented). The worst net-new finding is a **security-class cache poisoning bug**: `cache_page` keys by method + URI only, with no `Host` header component, so a multi-tenant or multi-domain deployment will serve one site's cached HTML to another site's visitors. The second-worst is a silent privacy breach: `cache_page` does not check the inbound `Authorization` or `Cookie` request headers before serving a cached response, meaning a page that renders differently for authenticated vs anonymous users will serve the wrong body to anonymous callers once a logged-in user's response is cached. Neither is filed in the existing backlog.

---

## Completeness vs Django cache framework

| Django cache feature | umbra-cache status |
|---|---|
| In-memory backend | Shipped — `MemoryBackend` (`tokio::sync::Mutex<HashMap>`) |
| DB backend | Shipped — `SqliteBackend` (SQLite only; no Postgres backend) |
| Redis backend | Shipped — `RedisBackend` behind `--features redis` |
| Memcached backend | Explicitly deferred (doc-comment says "past v0") |
| `get` / `set` / `delete` / `clear` | Shipped |
| TTL / expiry | Shipped on all three backends |
| `get_or_set` (fill-on-miss) | Explicitly deferred |
| `incr` / `decr` (atomic counters) | Explicitly deferred |
| Key prefixing / versioning | Not present, not deferred — entirely absent |
| Per-view cache (`@cache_page`) | Shipped — tower `Layer` |
| Template-fragment cache | Not present, not mentioned |
| Django `UpdateCacheMiddleware` / `FetchFromCacheMiddleware` | Not present — the `cache_page` layer is the only cache middleware |
| `cache.clear()` | Shipped (Redis: `FLUSHDB`; SQLite: `DELETE FROM`; memory: `HashMap::clear`) |
| Cache middleware (site-wide) | Not present; `cache_page` is route-subtree only |
| `Vary`-header awareness in `cache_page` | Deferred (doc-comment) |
| ETag / 304 conditional caching | Deferred (doc-comment) |
| Per-route cache key prefix | Deferred (doc-comment) |
| Background sweep / GC (SQLite) | Shipped — `SqliteBackend::sweep` (caller-driven, not auto-scheduled) |
| Sub-second TTL for Redis | Partially broken — see finding B below |

---

## Findings

### Security

**[NEW] [Required] `cache_page` caches by URI only — no `Host` header in the key** — `src/cache_page.rs:143`

```rust
let cache_key = format!("cache:page:{}:{}", method, uri);
```

`uri` in a Tower/axum context is the request URI, which is path-plus-query (e.g. `/page?x=1`) — it does **not** include the `Host` header. In a multi-domain deployment (two virtual hosts served by one process, which is idiomatic Django-style multi-tenancy), `https://site-a.example.com/home` and `https://site-b.example.com/home` produce the identical key `cache:page:GET:/home` and will serve each other's cached pages. Fix: include `req.headers().get(header::HOST)` (lowercased, stripped of port) in the key. Django's `cache_page` includes the `Host` header precisely for this reason (`django/views/decorators/cache.py::_cache_controller`).

**[NEW] [Required] `cache_page` serves cached responses to unauthenticated callers without checking request `Cookie`/`Authorization`** — `src/cache_page.rs:153–159`

The middleware looks up the cache key before calling the handler but never inspects the incoming request's `Cookie` or `Authorization` headers. A handler that renders personalised content for logged-in users (e.g. "Welcome, Alice") and returns a `200` without `Set-Cookie` on the response (a common pattern when the session cookie is already set) will have its response cached and then served verbatim to every subsequent caller at the same URI, including anonymous visitors. The `Set-Cookie` bypass guard (line 231) only inspects the *response* headers, not the request. Fix: either (a) include a stable, privacy-safe representation of the identity in the cache key (Django uses `Vary: Cookie` awareness for this), or (b) document prominently that `cache_page` is unsafe on any route reachable by authenticated users unless the response carries `Set-Cookie` or `Cache-Control: no-store`. A plain doc-callout is not enough — this needs a runtime check: if the request carries a `Cookie` or `Authorization` header, bypass the cache (serve through and do not store), mirroring Django's `has_vary_header(response, "Cookie")` logic.

**[NEW] [Important] `clear()` on Redis uses `FLUSHDB` — silent data loss if the cache DB is shared** — `src/lib.rs:465–470`

The doc-comment on `RedisBackend` says "use a dedicated Redis database (e.g. `/1`) when sharing a Redis instance." This warning exists only on the struct doc; it is not on `Cache::redis()` or `Cache::clear()`. An operator who calls `cache.clear()` without reading the struct doc will flush every key in the DB, including non-cache data. The warning is in the right file but not at the call site that matters. The test suite guards the `redis_clear` test with an `ends_with("/15")` check — a good pattern — but the production path has no such guard. Fix: at minimum, move the "dedicated DB" warning to the `Cache::redis()` constructor and `Cache::clear()` doc-comments. Stronger fix: key all Redis entries under a configured namespace prefix, and implement `clear()` with `SCAN` + `DEL` on that prefix instead of `FLUSHDB`.

---

### Correctness

**[NEW] [Required] `cache_page` caches HEAD and GET to the same key but serves a body on HEAD** — `src/cache_page.rs:137,143`

GET and HEAD requests produce the same cache key (`cache:page:GET:/path` vs `cache:page:HEAD:/path` — actually they do differ because `method` is in the key, so these are separate entries, which is correct). However, the middleware stores the full body for HEAD responses. RFC 9110 §9.3.2 requires that HEAD responses carry the same headers as GET but **no body**. If a HEAD response is cached with a body (axum's router normally strips the body for HEAD, but a handler that returns `Response::new(Body::from(...))` directly may not), and that body is later served for a HEAD request, the client gets a Content-Length mismatch. More concretely: a GET hit populates `cache:page:GET:/path` with body bytes. Then a HEAD request hits `cache:page:HEAD:/path`, misses, calls the handler, gets no body from axum's HEAD stripping — and *stores* an empty body. The two entries are correctly separate, so no cross-method serving occurs. This is actually fine as implemented. ~~Withdraw — not a bug.~~ Retaining as FYI below.

**[NEW] [FYI] `cache_page` stores separate GET and HEAD entries — HEAD entry body is always empty** — `src/cache_page.rs:143`

GET and HEAD keys are distinct (`cache:page:GET:…` vs `cache:page:HEAD:…`). This is correct: no cross-method body serving occurs. The HEAD entry will store a zero-byte body (axum strips it before the layer sees the response). On a HEAD cache hit the empty body is served, which is correct per RFC 9110. No bug; noting for completeness.

**[NEW] [Important] Sub-second TTL silently rounds up to 1 second on Redis** — `src/lib.rs:447`

```rust
let secs = dur.as_secs().max(1);
```

A `Duration::from_millis(500)` TTL becomes 0 from `as_secs()`, then 1 from `.max(1)` — so the entry lives twice as long as requested. The memory and SQLite backends use `chrono::DateTime` and honour sub-second precision. This silent divergence between backends is a correctness trap: a caller who writes `cache.set(k, v, Some(Duration::from_millis(500)))` gets the right expiry on memory/SQLite and a 2× expiry on Redis, with no error or warning. Fix: use `conn.pset_ex(key, value, dur.as_millis() as u64).max(1)` (Redis `PSETEX` for millisecond TTL), or document the 1-second floor as an explicit Redis limitation and enforce it at the `Cache` handle level so all backends behave the same.

**[NEW] [Important] `MemoryBackend::get` holds the mutex across the clone of the value** — `src/lib.rs:244–254`

```rust
let mut map = self.inner.lock().await;
if let Some(entry) = map.get(key) {
    ...
    return Some(entry.value.clone()); // lock held during clone
}
```

The `Mutex` is held for the full duration of `entry.value.clone()`. For small values this is fine. For large cached blobs (an HTML page of 50–200 KB is typical for `cache_page` usage) this serialises every concurrent read: all other readers block on the lock while the value is being memcpy'd out. Fix: clone the `Vec<u8>` out from under the lock by using a two-step acquire: lock → get the expiry + clone the Arc'd bytes → drop the lock → return. The simplest approach is to store `Arc<Vec<u8>>` in `MemoryEntry` so the clone under the lock is cheap (pointer copy), and the actual data copy (if needed) happens after the lock is released.

**[NEW] [Nit] `MemoryBackend::get` takes `&mut map` (exclusive lock) for a read** — `src/lib.rs:244`

`get` uses `lock()` (exclusive) to read a value and conditionally remove an expired entry. A `tokio::sync::RwLock` would allow concurrent reads without serialisation except when an expired entry needs removal (write lock). Given the `Arc<Vec<u8>>` fix above, switching to `RwLock` with a two-phase "read, then upgrade if expired" pattern would eliminate the hot-path contention. This is an Optional improvement once the `Arc<Vec<u8>>` change lands.

**[NEW] [Important] Deserialisation failure on cache hit is silently treated as a miss** — `src/lib.rs:194`

```rust
serde_json::from_slice(&bytes).ok()
```

A corrupt or type-mismatched cache entry returns `None` with no log. The doc-comment says "the entry is treated as poisoned and ignored rather than crashing the caller" — this is the right policy, but silent means a corruption that affects every request on a key generates no observable signal. Fix: add a `tracing::warn!` when `from_slice` fails (log the key, omit the value). Already handled correctly in `cache_page`'s deserialise path (line 157: "Deserialisation failure → treat as a miss and re-run the handler") but the generic `Cache::get` path has no logging at all.

**[NEW] [FYI] `CachePlugin::init` panics on double-call; `on_ready` warns and silently drops on double-call** — `src/lib.rs:515, 531–536`

The two wiring paths have asymmetric failure modes: `init` panics, `on_ready` warns. The warn-and-drop policy in `on_ready` is correct for a production plugin (panicking in `on_ready` would prevent the app from starting). The panic in `init` is acceptable since it's a startup-time programming error. No action needed; noting for future `init` callers.

---

### Architecture

**[NEW] [FYI] Raw `sqlx` in `SqliteBackend` is correctly excepted** — `src/lib.rs:279–300`

The CLAUDE.md ORM-use rule allows raw SQL for backend-specific features the ORM doesn't model (including the explicit-pool escape hatch not yet existing). The comment block at line 279–300 accurately describes both the original blockers and the remaining reason. The exception is legitimate. The DDL `CREATE TABLE IF NOT EXISTS` at line 311 is also correctly classified as schema DDL (the allowed DDL exception). No issue.

**[NEW] [Important] `SqliteBackend` is SQLite-only; there is no `PostgresBackend`** — `Cargo.toml:24`, `src/lib.rs:65`

The Cargo.toml declares `sqlx = { features = ["sqlite", ...] }` with no `"postgres"` feature. The CLAUDE.md says "Postgres-first, SQLite for tests." A production app using `Cache::sqlite(pool)` would be writing cache rows to SQLite while the ORM is running against Postgres — two separate database engines, with no connection between them. This is not necessarily wrong (SQLite-for-cache, Postgres-for-data is a legitimate split), but it is inconsistently documented. The plugin description says "In-memory, SQLite, and Redis backends" without noting there is no Postgres backend. For a production deployment on Postgres, the operator must use Redis; SQLite is either dev-only or a separate file-based cache DB. This should be stated explicitly in `Cache::sqlite`'s doc comment. Fix: add a doc-comment note: "Uses a SQLite file; not suitable as the main Postgres-backed cache in a Postgres-primary deployment — use `Cache::redis` in production."

**[NEW] [FYI] `cache_page` resolves the ambient cache twice in the hot path** — `src/cache_page.rs:146–153, 198`

After a cache miss the code does:
```rust
let cache: Option<&Cache> = if let Some(ref c) = explicit_cache { ... } else { crate::ambient() };
// ... call handler ...
if let Some(cache) = explicit_cache.as_deref().or_else(|| crate::ambient()) { ... store ... }
```

The ambient lookup is called twice: once on the read path and once on the write path. `OnceLock::get()` is essentially free (one atomic load), so this is not a performance issue, but the redundant resolution adds noise. The `cache` binding from the first resolution could be reused. Nit-level only.

**[NEW] [FYI] `tower::Service::poll_ready` delegates to `self.inner` but `call` clones `self.inner`** — `src/cache_page.rs:125–130`

This is the standard "buffer-or-clone" Tower pattern: `poll_ready` readies `self.inner`, then `call` clones it. This is correct per the Tower docs (the canonical idiom for stateless services), but it means `poll_ready` readies a *different* clone than the one `call` dispatches to. For middleware wrapping an axum `Router` (which is `Clone + Send`) this is fine, but it would be incorrect for a service with internal state that must be readied before use (e.g. `tower::buffer::Buffer`). Document this assumption ("inner service must be clone-safe to dispatch without pre-readying the clone") or switch to the `take`-based pattern if a stateful inner is ever expected.

---

### Performance

**[NEW] [Important] No memory bound on `MemoryBackend`** — `src/lib.rs:238`

`MemoryBackend` is an unbounded `HashMap`. There is no max-entry count, no max-total-bytes limit, and no LRU eviction. `cache_page` stores full response bodies — a workload with 10,000 distinct URIs (pagination, search queries with arbitrary params) will grow the map without bound until OOM. Django's `LocMemCache` has a `MAX_ENTRIES` (default 300) with a `CULL_FREQUENCY` eviction pass. Fix: add a `max_entries: Option<usize>` field to `MemoryBackend` (defaulting to, say, 1,000 or `None` for unlimited); on `set` when the limit is reached, evict a random sample (simple) or LRU (track insertion order with a `VecDeque` of keys). This is a Required fix for any production use of the memory backend with `cache_page`.

**[NEW] [FYI] SQLite `sweep` is caller-driven with no auto-scheduling integration** — `src/lib.rs:327`

`SqliteBackend::sweep` exists and is tested, but the plugin has no `umbra-tasks` integration to schedule it periodically. A long-running process that caches many short-TTL pages will accumulate dead rows indefinitely. This is acknowledged in the doc-comment ("reads already skip expired rows so a call is never required for correctness"). Once `umbra-tasks` is stable, wire a periodic sweep task (e.g. every 10 minutes) into `CachePlugin::on_ready`. Not a correctness issue, but a maintenance item.

---

### Readability / Stubs

**[NEW] [Nit] BROKEN-7 and BROKEN-12 comments are informative but not linked to any gap tracker** — `src/cache_page.rs:181`, `src/lib.rs:362,444`

BROKEN-7 describes the body-stream-failure → 502 fix (which is already implemented correctly). BROKEN-12 notes that write errors should be logged — also already implemented (the `tracing::warn!` calls are present). Both comments describe the fix *as already done*, not a remaining gap. They are accurate but leave a reader wondering whether these are "done" or "outstanding." Suggest converting them to `// Fixed: ...` comments or removing them now that the behaviour they describe is in place.

**[NEW] [Nit] `Cache::set` returns `Result<(), serde_json::Error>` but `Cache::delete` and `Cache::clear` return `()`** — `src/lib.rs:203–214`

The error surface is asymmetric: serialisation errors on `set` are surfaced; backend errors (SQLite pool failure, Redis drop) are swallowed silently (logged but not propagated). This is documented and intentional ("best-effort"). The asymmetry is fine for a cache, but callers must know they cannot distinguish "wrote successfully" from "serialised but backend silently dropped." A future improvement would be a `set_checked` that surfaces backend errors, for use in write-through cache patterns.

---

## Tests

### Coverage

| Area | Tests | Status |
|---|---|---|
| `MemoryBackend` — miss, round-trip string, struct, delete, clear, TTL expiry | 6 | Good |
| `SqliteBackend` — miss, round-trip + overwrite, TTL expiry, sweep, clear | 5 | Good |
| `RedisBackend` — connect, bad-URL, string RT, struct RT, miss, TTL expiry, delete, clear | 8 (gated on `REDIS_URL`) | Good pattern; runtime-skipped in CI without Redis |
| `cache_page` — second GET cached, POST bypasses, `no-store` bypasses, `Set-Cookie` bypasses, different query strings, non-200 not cached, header preservation | 7 | Good |

### Gaps

1. **No test for the Host-header collision bug.** A test with two requests to the same path but different `Host` headers confirming they produce distinct cache entries does not exist (and would currently fail — they'd hit the same key).

2. **No test for the Cookie/Authorization bypass gap.** A test where request 1 carries `Cookie: session=abc` and request 2 is anonymous, verifying that the authenticated response is not served to the anon caller, does not exist.

3. **No test for `MemoryBackend` concurrent read contention or large-value clone.** The correctness under concurrent access is untested.

4. **No test for the Redis sub-second TTL rounding.** `Duration::from_millis(500)` should be tested on the Redis backend to confirm it expires within the expected window.

5. **No test for `cache_page` with an explicit TTL that expires.** All `cache_page` tests use `Duration::from_secs(60)` — there is no test confirming that an expired cached entry causes the handler to fire again, which would also exercise the TTL-to-backend path end-to-end.

6. **No test for `CachePlugin::on_ready` wiring the ambient cache.** The `on_ready` path is covered only by reading the code; there is no integration test that instantiates a `CachePlugin::new(Cache::memory())`, calls `on_ready`, then asserts `ambient()` returns `Some`.

7. **`redis_connect_fails_on_bad_url` asserts nothing.** `tests/redis_backend.rs:54–60` — the test says "either Ok or Err is acceptable here." This tests that the code does not panic, which is correct, but it does not assert that a bad URL produces a `CacheError` rather than a silent Ok. The test should assert `result.is_err()` (or at minimum document why Ok is also acceptable).

8. **No test for `SqliteBackend::sweep` returning 0 when all rows are non-expired.** Only the "removes 1 expired row" case is covered; "removes 0 rows when nothing is expired" is not.

9. **`cache_page` has no test with a SQLite or Redis backend** — all seven `cache_page` tests use `Cache::memory()`. The serialisation wire format (`serialise_cached_response` / `deserialise_cached_response`) is only exercised through the memory backend; a corrupt blob stored via Redis and retrieved by `get_bytes_raw` would follow a different path.
