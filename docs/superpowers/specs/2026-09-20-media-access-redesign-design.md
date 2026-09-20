# Media access redesign — cached, signal-invalidated, role-aware

Date: 2026-09-20
Status: design approved, pending spec review → implementation plan
Tracks: gaps6 #9. Related: gaps4 #56–58 (the existing media gate — signed FS URLs, file→owner schema, non-FS proxy), gaps5 #101 (IDOR object-level authorization family), gaps6 #7 (materialized/computed invalidation — shares the tag-invalidation primitive introduced here).

## Problem

umbral already ships a media gate: `StoragePlugin::media_access(|headers, key| -> bool)`, `media_access_identity(|caller, key| -> bool)` (hands the resolved umbral-auth caller), `media_access_owner()`, and `media_signed_urls()`. Enforcement runs for both the filesystem guard layer and the non-FS proxy route.

The reference consumer `/home/dalmas/E/projects/local_task_tracker/backend/src/media_access.rs` (161 lines) shows where the weight actually is:

- ~40 lines resolving a **dual** human-or-agent caller (`resolve_identity` OR an agent key) — umbral's single-identity `media_access_identity` doesn't cover the app's second identity kind.
- ~80 lines of **app-specific authorization** (channel roster vs. project membership) — this is the app's data model and umbral cannot own it.
- **0 lines of caching** — every `<img src>` load re-runs all membership queries. On a 20M-row membership table this is the latency nightmare #9 calls out.

The three levers umbral can actually pull: **(a)** cache the decision by a reproducible key, **(b)** caller-resolution ergonomics, **(c)** role/visibility presets. The membership *logic* stays with the developer. This design does all three as one cohesive surface.

## Decisions (locked with the requester)

1. **Scope:** full cohesive redesign — caching + caller ergonomics + role presets together.
2. **Invalidation:** signal-driven auto-invalidation on developer-declared source tables, with a TTL backstop.
3. **Caller:** a rich umbral identity (user id, `is_superuser`, roles). Not a new pluggable resolver chain (deferred). *Revised at implementation:* the raw `HeaderMap` escape hatch originally planned here was dropped before shipping — see the shipped-surface note in §1 — because it would have let a closure branch on header state the cache key doesn't capture, reopening the caller-in-key footgun (§5) the rest of this design closes. App-specific identities (agent keys) go through a custom `Authentication` backend instead.
4. **Mechanism:** Approach A — tag-based cached decisions. A decision is cached under a framework-owned per-`(file, caller)` key (read before the DB work) and carries a set of dependency tags; a source-row change busts every key indexed under the affected tag. Chosen over hierarchical-prefix keys (invalidation shape chained to the key; multi-dependency doesn't fit one path) and full declarative inference (over-engineered for v1).

## Design

### 1. Developer-facing API

```rust
StoragePlugin::new()
    .media("/media", "./media")
    .media_access_cached(|caller: MediaCaller, key: &str| async move {
        if caller.is_superuser { return Decision::allow(); }               // cheap
        let Some(uid) = caller.user_id() else { return Decision::deny(); }; // anon → deny
        let (channel_id, allowed) = my_membership_check(uid, key).await;    // runs ONLY on cache miss
        Decision::of(allowed).depends_on([format!("chan:{channel_id}")])    // invalidation tags
        // .ttl(Duration::from_secs(30))  // optional; default applies. .no_cache() to opt out.
    })
    // declare how a source-row change maps to tags to bust; umbral wires the signal:
    .media_invalidate_on::<ChannelMember>(|row| [format!("chan:{}", row.channel)])
```

Types:

- `MediaCaller` (shipped as identity-only — see the note below):
  - `user_id(&self) -> Option<&str>` — the resolved umbral-auth pk as a string (`None` = anonymous).
  - `is_superuser: bool`.
  - `is_authenticated(&self) -> bool`.
  - `roles: Vec<String>` and `has_role(&self, &str) -> bool` — the caller's groups/roles, sourced from the same umbral-auth groups the permission layer reads. If the resolved identity object does not already carry them, the resolver loads them once per request (implementation verifies the exact field/query against umbral-auth); this load is itself a candidate for the same cache. Powers the role presets.
  - **Shipped surface note:** the raw `headers: &'a HeaderMap` field described in an earlier draft of this spec was dropped. `MediaCaller` is identity-only. This is a deliberate safety choice, not an oversight: the cache key is derived from the resolved `Identity` (`user_id()`/`anon`), and if the closure could also see raw headers, a developer could branch cacheable behavior on header state the key doesn't capture — caching one caller's allow and serving it to a different caller that presents different headers but resolves to the same (or no) identity. A non-session identity, such as an agent API key, is handled by resolving it to its own distinct `Identity` in a custom `Authentication` backend (`user_id = "agent:<id>"`), not by a raw-headers hatch in `MediaCaller`.
- `Decision`:
  - Constructors `allow()`, `deny()`, `of(bool)`.
  - `depends_on(tags: impl IntoIterator<Item = String>)` — invalidation tags, discovered during the (miss-only) DB work and indexed at store time.
  - `ttl(Duration)` — optional; a plugin default (~60s) applies when omitted.
  - `no_cache()` — opt this decision out of caching (for genuinely volatile decisions). **Decisions are cached by default**; the framework owns the cache key so there is nothing to name.
- **Cache key is framework-owned, never developer-supplied.** The wrapper derives `mediaacc:{file_key}:{caller_id|anon}` — the exact identity of "can this caller read this file" — and reads the cache *before* invoking the closure (see the caching flow below). This is what makes the cache read-first, and it removes the caller-in-key footgun entirely (the key always includes the caller). There is no `cache_as` / `cache_per_caller`; a per-file, per-caller key is the only shape, which is correct for media authorization.
- Role presets (zero closure):
  - `media_access_roles(roles: impl IntoIterator<Item = &str>)` — authenticated AND holds one of the roles.
  - existing `media_access_owner()` / `media_signed_urls()` kept.
- Invalidation declaration:
  - `media_invalidate_on::<M: Model + DeserializeOwned>(map_fn: Fn(&M) -> impl IntoIterator<Item = String>)` — umbral subscribes to `post_save:<M::TABLE>` / `post_delete:<M::TABLE>` and busts the returned tags on each change. Optional: TTL-only freshness works without any `media_invalidate_on`.

`media_access_cached` is a **new additive method**. The existing `media_access` / `media_access_identity` / `media_access_owner` / `media_signed_urls` are unchanged, so no current consumer breaks.

### 2. Cache tag-index primitive (new in `umbral-cache`, generic)

Two entries per cached decision:
1. the value: `key → bool` with TTL.
2. a tag index: `utag:<tag> → Set<key>` for each tag the decision depends on.

New primitive on `Cache` (and the `CacheBackend` trait), reusable by gaps6 #7:

```rust
Cache::get_or_compute_tagged(key, &tags, ttl, || async { /* the DB check */ }) -> bool
Cache::bust_tag(tag)   // delete every key indexed under tag, then the tag set
```

Backend implementations (all three built-ins):
- **Redis:** `SET key … EX ttl` + `SADD utag:<tag> key`; `bust_tag` = `SMEMBERS utag:<tag>` → `DEL` each key + `DEL` the set (pipelined, O(members)). Key refs left dangling by TTL expiry are harmless (a `DEL` of a gone key is a no-op) and self-heal on the next `bust_tag`.
- **Memory:** a `HashMap<tag, HashSet<key>>` beside the value map, under the same lock.
- **Sqlite:** a `cache_tag(tag, key)` table; `bust_tag` deletes matching cache rows + tag rows.

This primitive lives in `umbral-cache` (not storage) because it is the general "source-change → bust derived state" seam; media access is its first consumer and gaps6 #7 (materialized fields) is the intended second.

### 3. Signal-driven invalidation wiring

At `StoragePlugin` build / `on_ready`, each `media_invalidate_on::<M>(map_fn)` registration installs `signals::subscribe_async("post_save:<M::TABLE>", …)` and `post_delete:<M::TABLE>`. The handler deserializes the row payload JSON to `M`, runs `map_fn(&row)`, and calls `Cache::bust_tag` for each returned tag.

v1 wires the per-row `post_save` / `post_delete` signals. The bulk paths (`bulk_post_save` / `bulk_post_delete`, whose payloads carry ids, not full rows) are not fine-grained-invalidated in v1 and fall back to the TTL backstop — documented.

### 4. Enforcement path

The gate (`media_gate`) already resolves a decision by invoking `MediaAccessFn` for both the FS guard layer and the non-FS proxy (gaps4 #58). The cached resolver slots in at that single point:

1. signed-URL short-circuit — unchanged.
2. resolve `MediaCaller` from headers + ambient auth (reuse the `media_access_identity` resolution, enriched with roles).
3. derive the framework-owned key `mediaacc:{file_key}:{caller_id|anon}` and **read the cache first**. On hit → return the cached bool with no DB work. On miss → invoke the closure (which does the DB work), then, unless the returned `Decision` is `no_cache()`, store `key→bool` with the TTL and index the `depends_on` tags. This cache-first ordering is the point of the redesign; the closure runs only on a miss.
4. allow ⇒ serve (FS `ServeDir` / non-FS proxy stream); deny ⇒ `forbidden_media()` (403).

No new enforcement surface — both backends inherit caching for free.

### 5. Security semantics

- **Decisions cached by default; `no_cache()` opts out.** Both allow and deny are cached (Approach A), bounded by TTL + signal bust. Genuinely volatile decisions use `no_cache()` to stay live.
- **Fail closed on auth, fail-to-compute on cache.** Anonymous → deny unless the closure allows. Cache miss or cache error → run the live check. A DB error inside the closure is the developer's to convert to `deny()`. Bytes are never served on an infrastructure error.
- **No caller-in-key footgun.** The framework owns the key and always includes the resolved caller id, so one caller's `allow` can never be served to another — the developer cannot get the key wrong because they never write it.
- **Signed-URL mode composes** unchanged.
- **No cache configured ⇒ always compute.** `Cache::ambient() == None` means the closure runs every time; caching is a pure optimization whose failure mode is "recompute," never "allow."

### 6. Configuration ergonomics

The simple case must need almost nothing:
- **Zero-config caching:** if `CachePlugin` is installed, media caching works via `Cache::ambient()` with no wiring in `StoragePlugin`.
- **Default TTL** (~60s) so `.ttl()` is optional; `.cache_per_caller(…)` alone is enough to opt in.
- **`media_invalidate_on` is optional** — TTL-only freshness works out of the box; add invalidation declarations only when sub-TTL freshness matters.
- Role presets need zero closure.

### 7. Testing (behavioral — real rows, real path, read back)

- **Cache hit skips the DB:** using the repo's query-counter pattern, the first access runs the closure (1 DB check), a second access with the same key does 0 DB checks.
- **Signal invalidation:** seed a membership row → access allowed (cached) → delete/modify the membership row **through the ORM** (fires `post_delete` / `post_save`) → next access recomputes → denied.
- **TTL backstop:** short TTL → the closure runs again after expiry.
- **Role preset:** `media_access_roles(["admin"])` — admin caller allowed, non-admin denied.
- **No CachePlugin installed:** still enforces correctly (closure runs every time).
- **Security:** anonymous denied; superuser bypass uncached; a cached deny is busted on grant.
- **Both backends:** FS guard + non-FS proxy both enforce (mirror the gaps4 #58 tests).
- **Cache primitive:** `get_or_compute_tagged` / `bust_tag` unit + behavioral tests per backend in `umbral-cache` (Memory + Sqlite always; Redis behind the existing `UMBRAL_TEST_*` gate).
- Tests run on SQLite + memory cache.

### 8. Scope / files

- `plugins/umbral-cache/src/lib.rs` — `get_or_compute_tagged`, `bust_tag`, `CacheBackend` tag methods, the three backend implementations + the `cache_tag` sqlite table.
- `plugins/umbral-storage/src/lib.rs` — `MediaCaller`, `Decision`, `media_access_cached`, `media_access_roles`, `media_invalidate_on`, the resolver + signal registration.
- `plugins/umbral-storage/src/media_gate.rs` — slot the cached resolver into the existing decision point.
- Tests: `plugins/umbral-cache/tests/`, `plugins/umbral-storage/tests/`.
- Docs: `documentation/docs/v0.0.1/storage/media-access.mdx` (purpose, one example, link to this spec); a short `documentation/docs/v0.0.1/cache/` note for the tag primitive.

### 9. Out of scope / deferred

- **Pluggable caller-resolver chain** (unify session/bearer/agent behind one `Caller`) — not built. The shipped `MediaCaller` stays identity-only (see the shipped-surface note in §1); the dual-identity case (a human session vs. an agent key) is handled today by resolving the agent key to its own `Identity` in a custom `Authentication` backend, not by a raw-headers hatch on `MediaCaller`.
- **Fine-grained bulk-signal invalidation** — bulk writes fall back to TTL in v1.
- **Unifying the invalidation primitive with gaps6 #7 (materialized fields)** — this design deliberately builds `bust_tag` generically in `umbral-cache` so #7 can reuse it, but #7's declaration surface and recompute strategy remain its own effort.
