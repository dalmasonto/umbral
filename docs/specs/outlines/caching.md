# Outline — Caching

| | |
|---|---|
| **Status** | Outline. Promotes when a built-in plugin needs caching beyond defaults. |
| **Maps to milestone** | M11+ |
| **Companions** | `01-app-and-settings.md`, `02-plugin-contract.md`, outlines `web-layer.md`, `templates.md`, `signals.md`, `auth-and-sessions.md`, `arch.md §4.7` |

## Purpose

`umbra::cache` is the framework's uniform key/value cache surface, sitting in front of both an in-process backend (`moka`, the default) and a distributed one (`redis`, opt-in via feature). It exists as a core utility — not a plugin — because the same three Django-shaped use cases come up across the built-ins: caching whole responses (per-view), caching rendered HTML fragments (per-fragment), and caching arbitrary computed values (low-level). Sessions, the admin's expensive list queries, and REST's filter/serialize hot paths all want the same primitive; if each plugin shipped its own cache, swapping moka for redis would mean editing every plugin. Centralising the API while keeping backends pluggable is the Django shape, and it's the smallest surface that lets the same code run locally with moka and in production with redis.

## Key concepts

**The `Cache` trait.** A small, async, object-safe trait every backend implements. `get_or_set` is the load-shedding shape callers want most: compute-once-on-miss with the TTL applied at write time.

```rust
#[async_trait]
pub trait Cache: Send + Sync + 'static {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<()>;
    async fn delete(&self, key: &str) -> Result<()>;
    async fn clear(&self) -> Result<()>;
}
```

**Built-in backends.** `MokaCache` wraps `moka::future::Cache` — bounded LRU with TTL, no external dependency, zero-config default. `RedisCache` wraps `redis::aio::ConnectionManager` and ships behind a `redis` feature flag so an app that never installs it pays no compile cost. Both implement the same trait; swapping happens in `Settings`, not in call sites.

**Ambient handle.** The cache lives in the `umbra::cache` `OnceLock` from `01-app-and-settings.md` table — set during `App::build()`, never re-set, read through `umbra::cache::default()`. If no cache backend is configured, the builder publishes a `MokaCache` with framework defaults; the accessor never returns `None`, so plugin code can assume a cache is always present.

**Per-view caching.** A `CacheLayer` tower middleware (cross-link `web-layer.md`) keys responses on the request URL plus a configured `vary_on` set of headers (`Accept`, `Accept-Language`, `Cookie` when relevant), respects `Cache-Control: no-store`, and skips non-200 responses by default. Mounted globally or per-router scope.

**Per-fragment caching.** A `{% cache ttl key %}…{% endcache %}` template tag (cross-link `templates.md`) wraps a block of rendered HTML under a caller-supplied key. The tag goes through the same ambient handle; nothing in the templates engine knows which backend is live.

**Low-level API.** Direct calls from app code that wants explicit control. `get_or_set` is the canonical shape:

```rust
let posts = umbra::cache::default()
    .get_or_set("posts:recent", Duration::from_secs(60), || async {
        Post::objects().order_by("-created_at").limit(10).all().await
    }).await?;
```

**Invalidation patterns.** Three layered options. TTL-based expiry is the default and covers most uses with no extra wiring. Explicit key bumping (`cache.delete("posts:recent")` after a write) covers cases where a known mutation invalidates a known key. Signal-driven invalidation (cross-link `signals.md`) wires a `post_save` handler that deletes a model-keyed entry (`format!("{model}:{id}")`) — the "I changed a `Post`, drop every cache entry mentioning that post" pattern, expressed once and reused per model.

## Promote-to-deep trigger

Promote when the first built-in plugin needs caching beyond the in-process default — most likely `umbra-sessions` reaching for redis, `umbra-admin` caching expensive list queries, or `umbra-rest` caching serializer output. Whichever consumer first forces multi-backend support is the trigger.

## Open questions

- **Serializer strategy.** `serde_json` is portable and debuggable in `redis-cli`; `bincode` is faster and smaller; a per-call codec parameter would let callers opt in to either. The right default depends on whether the dominant payload is JSON-shaped already or arbitrary Rust structs.
- **Key namespacing.** Whether the framework prefixes keys with a plugin name (`auth:user:42`) automatically, or leaves namespacing to the caller. Automatic prefixing prevents collisions between plugins sharing a redis instance but complicates `cache.delete` from outside the owning plugin.
- **Multi-backend (L1/L2).** Some installs want an in-process L1 in front of a redis L2 to dodge a network hop on hot keys. Designed-in (with an explicit `LayeredCache` adapter) or deferred — the call hinges on whether the early built-in consumers actually need it.
- **Multi-database interaction.** A cache entry computed against one DB alias is logically scoped to that alias; whether the cache key carries the alias automatically, or whether callers must include it, mirrors the multi-database routing question from `01-app-and-settings.md`.
- **Cache-stampede protection.** `get_or_set` is the obvious place to add single-flight semantics so a thundering herd doesn't all compute the same miss. moka has primitives for this; redis needs a lock key. Decide whether v1 ships the protection or documents the gap.

## Cross-links

- Deep specs that constrain this: `01-app-and-settings.md` (the ambient cache handle and the `OnceLock` pattern it rides on), `02-plugin-contract.md` (a plugin may register its own cache backend through the builder, the same shape as any other ambient contribution).
- Sibling outlines: `web-layer.md` (per-view caching middleware mounts here), `templates.md` (the `{% cache %}` tag is a template-engine extension), `signals.md` (`post_save` is the canonical invalidation trigger), `auth-and-sessions.md` (session storage is itself separate from the cache, but session-derived data — permission sets, the current user — is a natural cache consumer).
- `arch.md §4.7`.
