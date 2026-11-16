# review_4 · umbral-cache

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 3,478 LOC (`plugins/umbral-cache`) |
| **Reviewer** | claude (review_4 wave agent) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 1 medium · 3 low · 1 info — 5 new, 3 known |

## Summary

- `umbral-cache` ships the memory / SQLite / Redis byte backends behind an infallible `CacheBackend`, the typed `Cache` handle, the tag-invalidation machinery (`TaggedCache`, the process-wide `BustLog` epoch counter, the guarded store), and the `cache_page` response-caching tower layer.
- Overall state is **healthy**: the two hard problems in this space — cache-page cross-user leakage and the miss-compute/bust write-skew — are both closed, with the right shape (a personalised-request bypass; an epoch stamped *before* eviction so the guarded store's post-check can never miss a racing bust) and with tests. The raw-SQL exception in `SqliteBackend` is documented in-code in the exact form CLAUDE.md asks for; it is only the promised *tracker entry* that is missing.
- The one medium finding is a contract mismatch: `get_or_compute_tagged` performs the naive unguarded store — the precise race the docs' own next section warns about and fixes elsewhere — while the documentation's lead example teaches the unguarded call.
- The rest is a drift batch at LOW: two backends' read paths still swallow errors silently after the write paths were fixed (BROKEN-12's fix covered writes only), Redis rounds sub-second TTLs up, and plain `set` never prunes stale tag membership (over-invalidation, never stale-serve).
- Fix order: CACHE-1 (guarded `get_or_compute_tagged` or a rustdoc warning + docs example fix) → CACHE-2 (log the two read paths) → CACHE-4 (untag in plain `set`).

## Findings

### CACHE-1 · MEDIUM · correctness/docs · self-verified

**`get_or_compute_tagged` — the public, documented-blessed primitive — does the naive unguarded store: a `bust_tag` landing between the compute and the store is overwritten by the stale just-computed value, which then survives (until TTL — and the documented pattern uses `ttl: None`, so until the *next* bust of that tag).**

- **Where**: `plugins/umbral-cache/src/lib.rs:302-320`, with the guarded alternative at `:374-393` and the docs at `documentation/docs/v0.0.1/cache/tagged-invalidation.mdx`
- **Evidence**:
  ```rust
  // lib.rs:302-320 — no epoch capture, no guard, no rustdoc warning
  pub async fn get_or_compute_tagged<T, Fut>(&self, key: &str, compute: impl FnOnce() -> Fut) -> T {
      if let Some(hit) = self.get::<T>(key).await { return hit; }
      let c = compute().await;
      if let Some(spec) = c.store {
          if let Ok(bytes) = serde_json::to_vec(&c.value) {
              self.backend.set_tagged(key, bytes, spec.ttl, &spec.tags).await;
          }
      }
      c.value
  }
  ```
  The docs' own example (lines 14-33) is built on this call — `cache.get_or_compute_tagged("membership:42:7", …)` with `ttl: None` — and the very next section (lines 38-43) tells the reader: "A cache miss runs your closure (the DB work) and then stores the result — and a `bust_tag` can land in that gap… The cache closes this with a **guarded store**. Capture the cache's monotonic bust epoch *before* computing, then store through `set_tagged_bool_guarded(key, value, ttl, tags, since_epoch)`."
- **Impact**: Concrete trace: a membership check via the documented example; `bust_tag("channel:7")` (a revocation) lands after `compute()` returns but before the store; the store writes `allow=true` tagged `channel:7` *after* the bust already evicted; with `ttl: None` nothing expires it, so the revoked member keeps access until some other write busts `channel:7` again. The guard that closes exactly this exists (`set_tagged_bool_guarded`, `lib.rs:374-393`) and `media_access_cached` uses it (gaps6 #12) — but the generic primitive the docs teach does not, so the trap is reachable by any user following the cache documentation verbatim.
- **Fix**: give `get_or_compute_tagged` the same dance it prescribes (it owns `spec.tags`, so it can capture `bust_epoch()` before `compute()` and store through `set_tagged_bool_guarded`, deleting on `false`); failing that, rustdoc-warn on the method and switch the docs example to the epoch-capture + guarded-store sequence. Cross-ref: gaps6 #12 — that entry's scope is `media_access_cached`; this is the unguarded generic sibling (class tracker noted in Known, defect kept numbered).

### CACHE-2 · LOW · observability/drift · self-verified

**The `CacheBackend` trait doc promises backends "swallow errors internally *and log them*", and BROKEN-12 is stamped fixed — but the fix covered only the write paths; `SqliteBackend::get` and `RedisBackend::get` still discard backend errors silently, while their write siblings log.**

- **Where**: `plugins/umbral-cache/src/lib.rs:131-133` (the promise), `:569-575` and `:762-769` (the silent reads), `:591-594` and `:775-776` (the fixed writes)
- **Evidence**:
  ```rust
  // lib.rs:131-133 — names get_bytes explicitly
  /// `get_bytes` / `set_bytes` / `delete` / `clear` are infallible at the
  /// trait level — backends swallow errors internally and log them rather
  /// than propagating.
  ```
  ```rust
  // lib.rs:571-575 — SqliteBackend::get: a dead/locked pool is a silent miss
  sqlx::query_as("SELECT value, expires_at FROM umbral_cache WHERE key = ?")
      .bind(key).fetch_optional(&self.pool).await.ok()?;
  ```
  ```rust
  // lib.rs:765-768 — RedisBackend::get: same, one `.ok().flatten()`
  conn.get::<_, Option<Vec<u8>>>(self.k(key)).await.ok().flatten()
  ```
  ```rust
  // lib.rs:591-594 — the write path got the fix (BROKEN-12)
  // BROKEN-12: log swallowed write errors. A cache backend is
  // best-effort (a failed write must not break the request), but a
  // locked SQLite / dead pool that no-ops every write forever should
  // not be invisible — the trait doc promised "and log them".
  ```
- **Impact**: With `Cache::sqlite(pool)` against a pool that goes bad (file moved, permissions, connection exhaustion) the app silently runs 100% uncached on reads — every request recomputes and hits the DB — with the only signal being the write-side warnings (which do fire in the common case where both fail together, which is what keeps this LOW). A read-only workload over a pre-warmed table shows nothing at all. `planning/review/broken-features.md:97` stamps BROKEN-12 "✅ FIXED (93bf205)" scoped to "`SqliteBackend::set/delete/clear` and all `RedisBackend` methods" — "all RedisBackend methods" is contradicted by `RedisBackend::get` at this commit.
- **Fix**: `tracing::warn!` with the error and key in both read paths (matching the tested write-side wording), then update the BROKEN-12 Known entry.

### CACHE-3 · LOW · drift

**Redis `set` rounds every sub-second TTL up to at least 1 second (`dur.as_secs().max(1)`), while the memory and SQLite backends honour exact expiry — the same cache entry lives up to ~1s longer on Redis.**

- **Where**: `plugins/umbral-cache/src/lib.rs:777-779`
- **Evidence**:
  ```rust
  let res: Result<(), _> = if let Some(dur) = ttl {
      let secs = dur.as_secs().max(1);
      conn.set_ex(&key, value, secs).await
  ```
  (Redis's `SET EX` takes whole seconds, so the clamp is a real API constraint; memory/SQLite compute exact `DateTime` expiries — `expiry()`, `lib.rs:404-410`.)
- **Impact**: A caller choosing `Duration::from_millis(200)` gets exact semantics on two backends and 1s (5×) on Redis — a debugging trap when the same code runs against different dev/prod backends. Bounded (≤1s), no correctness consequence for anything with a sane TTL.
- **Fix**: either document the rounding on the trait (`ttl` is honoured to whole seconds on Redis), or use `PSETEX`/`SET PX` for sub-second precision so the three backends agree.

### CACHE-4 · LOW · drift (fails safe)

**Plain `set` never prunes a key's old tag membership — on all three backends — so a value overwritten outside `set_tagged` keeps its previous tags, and a later `bust_tag` of an old tag wrongly deletes the newer value.**

- **Where**: `plugins/umbral-cache/src/lib.rs:451-458` (memory `set`) vs `:472-484` (`set_tagged` prunes), `:585-608` (SQLite `set`, value row only) vs `:651` (`set_tagged` prunes `umbral_cache_tag`), `:780-781` (Redis plain `SET`)
- **Evidence**:
  ```rust
  // lib.rs:451-458 — memory set: value only, no st.untag(key)
  async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) {
      let expires_at = expiry(ttl);
      self.inner.lock().await
          .values.insert(key.to_string(), MemoryEntry { value, expires_at });
  }
  ```
  ```rust
  // lib.rs:475-477 — set_tagged DOES prune the old memberships first
  st.values.insert(key.to_string(), MemoryEntry { value, expires_at });
  st.untag(key);
  ```
- **Impact**: A key written via `set_tagged(key, v1, …, ["t"])` then overwritten via plain `set(key, v2)` (the API the generic `Cache::set` and `cache_page`'s `set_bytes_raw` use) still lists under tag `t`; a later `bust_tag("t")` removes the *newer* value. The failure direction is safe (needless recompute / cache miss, never serve-stale), which is why this is LOW — but it makes tag semantics depend on which write API touched a key last, and in-tree the two namespaces (`cache:page:…` vs decision keys) only accidentally never collide.
- **Fix**: call the backend's `untag(key)` from plain `set` on all three backends (memory `st.untag(key)`; one `DELETE FROM umbral_cache_tag WHERE key = ?` on SQLite; one SREM against `ukeytags:{key}` on Redis), or document that plain `set` clears tag membership.

### CACHE-5 · INFO · ergonomics

**`cache_page`'s key is delimiter-ambiguous in the same class gaps6 #13 fixed for `media_access_cache`**: `format!("cache:page:{}:{}:{}", method, host, uri)` (`cache_page.rs:191`) puts the unvalidated, colon-capable `Host` value (`cache_page.rs:184-189` takes it raw from `headers()`) ahead of the URI, so two different (host, uri) splits can produce one key. Practically unreachable through a normal server (the URI must start with `/`, so the boundary is only shiftable by a non-origin-form request-target, which depends on hyper's acceptance — static analysis cannot confirm), but a length-prefixed key (the shape `media_cache_key` now uses, storage `lib.rs:103-110`, with its injectivity test) would make it unarguable. No action required.

## Strengths

- **The guarded store is the right design, implemented at the right layer.** `set_tagged_bool_guarded` (`lib.rs:374-393`) is a pre-check + post-check pair above `CacheBackend`, so memory/SQLite/Redis are all covered with zero per-backend code; the correctness argument rests on `bust_tag` stamping the `BustLog` epoch *before* evicting (`lib.rs:296-299`), and that ordering is stated in the docstring and enforced by tests (`tests/tagged_write_skew.rs`). Failure mode is "recompute, never serve-stale".
- **`BustLog` is bounded by construction**: `AtomicU64` + per-tag `last_bust` map, `BUST_LOG_CAP = 8192`, compaction to the most-recent busts at 2× (`lib.rs:170-208`), with in-crate tests that the map never exceeds the cap and that a recent bust survives compaction (`lib.rs:1150-1175+`) — plus an explicit note of the astronomically-unlikely worst case.
- **`cache_page`'s safety posture is careful**: personalised requests (Authorization / Proxy-Authorization / session cookie / identity-varying headers) bypass before lookup (`cache_page.rs:285-307`, tests at `:424-491`); only `GET`/`HEAD` 200s are stored; `private`, `no-store` and `Set-Cookie` responses bypass (`:309-357`); a body-collect failure returns a clean 502 and caches nothing (`:227-242`); oversized bodies are served but never stored (`:245-248`).
- **Redis namespacing is disciplined**: a single prefix, `SCAN` + `UNLINK` by prefix (never `FLUSHDB`), and a reverse tag-index maintained on delete/retag via the `unprefixed` helper (`lib.rs:751-755`) so namespaced members prune the raw-keyed index correctly (gaps6 #11).
- **The raw-SQL exception comment (`lib.rs:496-518`) is the best-practice *form* of the CLAUDE.md workaround**: it names the exact missing ORM API (`Manager::upsert_with(&pool, …)`) and why it is needed (explicit, possibly non-ambient pool). Its only defect is the missing tracker entry (Known).

## Clean areas checked

- **Injection**: all 13 live `sqlx::query*` sites (`lib.rs:529` … `:682`) are constant strings with `?` placeholders and `.bind(…)` values — zero interpolation of keys, values or tags; `SqliteBackend::new`'s `CREATE TABLE IF NOT EXISTS` is constant.
- **Key isolation**: `cache_page` keys include the raw `Host` (module doc `cache_page.rs:20`, code `:191`) so multi-tenant virtual hosts don't share entries; the personalised-request bypass closes the audit_2 cross-user route (verified in code + tests).
- **TTL enforcement**: memory checks expiry on read and removes the stale entry (`lib.rs:440-445`); SQLite re-checks `expires_at` on every get (`:577-581`); Redis delegates to `SET EX`. No path serves an expired value.
- **Redis clear scope**: clearing is prefix-scoped `SCAN`+`UNLINK` on the connection's own DB (URL-selected), never `FLUSHDB` — the review_3 #15 concern is genuinely closed.
- **Concurrency**: `MemoryBackend` uses `tokio::sync::Mutex` (async-aware; no poisoning abort), and no cache lock is held across a foreign await — the only `await` under a lock is acquiring it. `BustLog`'s `Mutex` is synchronous and never awaited across.
- **Panic surface**: no `unsafe` (grep: 0); no `unwrap`/`expect` on values reachable from a request path; the only `unwrap`-family calls are infallible conversions (`Duration → chrono`, `serde_json::to_vec` guarded by `if let Ok`).
- **Feature gating**: the Redis backend is entirely behind `#[cfg(feature = "redis")]`; a default build compiles zero Redis code.
- **Ergonomics of the contract**: `TaggedCache` is object-safe and implemented by the plugin so no plugin depends on `umbral-cache` (the core-slice consumer boundary is clean).

## Known issues already tracked elsewhere (not counted in the totals)

- `review_1 broken-features.md:95` (BROKEN-10) — `MemoryBackend` never evicts expired entries except on read; `sweep()` exists only on `SqliteBackend`, no purge-on-set, no entry cap. **Stamped DEFERRED; verified still true at this commit** (`lib.rs:437-449` removes only the requested key; `:451-458` inserts unbounded) — a dated, deliberately-deferred item, not a new finding `[open]`
- `review_1 broken-features.md:104` / `query-api-sufficiency.md:116` — the `SqliteBackend` raw-SQL workaround is "borderline-allowed" pending a *logged* `Manager::upsert_with(&pool)` ORM gap. **Verified at this commit: still unlogged** (grep over `planning/` finds only review_1's own files and this sweep's core scratch/notes; no `gaps*.md` entry) and `upsert_with` exists nowhere under `crates/`; 13 raw-SQL sites remain `[open]`
- `review_1 broken-features.md:114` — `cache_page` has no stampede protection (recorded as a documented deferral) `[open — documented deferral]`
- `review_1 broken-features.md:97` (BROKEN-12) — stamped ✅ FIXED for write-side logging; the read-side residual is CACHE-2 above, not absorbed `[partially held — see CACHE-2]`
- `review_1 broken-features.md` (BROKEN-7) — a body-collect failure used to fabricate a 200; now returns a clean 502 and caches nothing (`cache_page.rs:227-242`, comment in place, no test for the arm) `[fixed/verified held]`
- `gaps6 #11` — Redis reverse tag-index pruning including the `unprefixed` helper `[fixed/verified held]`
- `gaps6 #12` — write-skew guarded store shipped (epoch-before-evict; tests `tagged_write_skew.rs`); CACHE-1 is the unguarded generic sibling this entry does not cover (explicit relationship, defect kept numbered) `[fixed/verified held]`
- `gaps6 #19` — `BustLog` compaction bounded (`lib.rs:170-208`) with in-crate tests `[fixed/verified held]`
- `gaps4 #21` (review_3 codex #15) — Redis clear scope: `SCAN`+`UNLINK` by prefix instead of `FLUSHDB` `[fixed/verified held]`
- `gaps4 #23` (review_3 codex #18) — `cache_page` never stores an oversized body; the residual full-body buffering is acknowledged in the code comment (`cache_page.rs:221-224`) and deliberately not re-filed `[fixed/verified held]`
- `audit_2 plugin-realtime-comms #1/#8` — `cache_page` cross-user poisoning fixed (personalised bypass + `private`/`no-store` handling + docs key includes Host and the bypass list) `[fixed/verified held]`

## Out-of-scope notes

- **Core `TaggedCache` defaults**: `bust_epoch()` defaults to `0` and `set_tagged_bool_guarded`'s default body is an *unguarded* store, documented as "backward-compatible for caches that don't track a bust epoch" (`crates/umbral-core/src/cache.rs:24-51`) — a consumer that implements the trait without overriding both silently gets the write-skew race; same drift shape as CACHE-1, core's call. Routing to the core slice.
- **Core slice 1's QW lead (sessions/storage as cache consumers)**: from the cache side, no `umbral-cache` defect surfaced — storage's media path uses the guarded `set_tagged_bool_guarded` (verified held), and sessions keeps its own store and does not route through `umbral-cache`. Nothing to file against this crate; passing back to the core slice.
