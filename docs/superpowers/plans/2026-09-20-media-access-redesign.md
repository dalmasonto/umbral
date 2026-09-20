# Media Access Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `StoragePlugin` a cache-first, signal-invalidated, role-aware media-access gate so a developer expresses a per-file authorization decision once and repeat accesses skip the DB.

**Architecture:** Phase 1 adds a generic tagged-cache primitive (get-or-compute keyed value + tag→keys index + bust-by-tag) reachable via the facade. Phase 2 builds the media surface on it: a `MediaCaller` + `Decision` closure whose result is cached under a framework-owned `mediaacc:{file_key}:{caller_id}` key read *before* the closure runs, with invalidation wired from developer-declared source tables' ORM signals and a TTL backstop.

**Tech Stack:** Rust, axum, sqlx, `#[async_trait]`, serde_json, umbral-core signals, umbral-cache backends (Memory/Sqlite/Redis), umbral-auth `Authentication`/`Identity`.

**Spec:** `docs/superpowers/specs/2026-09-20-media-access-redesign-design.md`

## Global Constraints

- The ORM is the single DB interface; plugin row reads/writes go through it, never raw `sqlx::query`. The narrow exception here is schema DDL inside a cache backend (the `umbral_cache_tag` table), which mirrors the existing `umbral_cache` table's raw DDL in `plugins/umbral-cache/src/lib.rs`.
- Plugins import the facade, not each other. `umbral-storage` must reach the tagged cache without depending on the `umbral-cache` crate (see Task 1).
- Additive only: existing `media_access` / `media_access_identity` / `media_access_owner` / `media_signed_urls` stay unchanged.
- Lean code, minimal comments (only where non-obvious). Build/test only the crates a task touches — never the whole workspace (full build ~78G).
- Never `git stash` / `git reset` / `git checkout --` / `git restore`; never wipe a DB.
- Default media-access TTL: `const MEDIA_ACCESS_DEFAULT_TTL: Duration = Duration::from_secs(60);`
- Cache key shape: `format!("mediaacc:{file_key}:{caller}")` where `caller` is the resolved user id or the literal `anon`.

---

## Phase 1 — Generic tagged-cache primitive

### Task 1: Cache contract seam reachable from the facade

**Files:**
- Inspect: `plugins/umbral-cache/src/lib.rs` (the `Cache` + `ambient()` + `CacheBackend`), `crates/umbral-core/src/` (does a cache seam already exist? `grep -rn "ambient.*[Cc]ache\|CacheContract\|dyn.*Cache" crates/umbral-core/src`).
- Likely modify: `plugins/umbral-cache/src/lib.rs`, `crates/umbral/src/lib.rs` (facade re-export).

**Decision to make first (report it in the task's commit message):** how `umbral-storage` reaches the tagged cache without a plugin→plugin dep. Preferred: the *contract* is an object-safe trait the facade re-exports, implemented by `umbral-cache`.

- [ ] **Step 1: Determine the seam.** If `umbral-core` already exposes an ambient cache contract used by sessions/throttle, reuse it. Otherwise the tagged-cache methods added in Tasks 2–4 live on the existing `umbral-cache` `Cache`, and Phase 2 reaches them by adding `umbral-cache` to `umbral-storage`'s `Cargo.toml` as an **optional** dependency gated behind a `cache` feature (the pragmatic fallback if no core seam exists). Record which path you took.

- [ ] **Step 2: No code change if reusing an existing seam.** If introducing the feature-gated optional dep, add to `plugins/umbral-storage/Cargo.toml`:

```toml
[dependencies]
umbral-cache = { path = "../umbral-cache", optional = true }

[features]
cache = ["dep:umbral-cache"]
```

- [ ] **Step 3: Commit** (only if a Cargo.toml/facade change was made).

```bash
git add plugins/umbral-storage/Cargo.toml
git commit -m "chore(storage): optional umbral-cache dep for media-access caching"
```

Note: this task carries no test of its own; it exists to lock the structural decision the later tasks consume. Its verification is that Tasks 2 and 7 compile.

### Task 2: `Computed<T>` / `StoreSpec` types + `CacheBackend` tag methods (default impls)

**Files:**
- Modify: `plugins/umbral-cache/src/lib.rs`
- Test: `plugins/umbral-cache/tests/tagged.rs` (create)

**Interfaces:**
- Produces: `Computed<T>`, `StoreSpec`, `CacheBackend::set_tagged`, `CacheBackend::bust_tag`, `Cache::get_or_compute_tagged`, `Cache::bust_tag`.

- [ ] **Step 1: Write the failing test**

```rust
// plugins/umbral-cache/tests/tagged.rs
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use umbral_cache::{Cache, Computed, StoreSpec};

#[tokio::test]
async fn get_or_compute_tagged_computes_once_then_hits_cache() {
    let cache = Cache::memory();
    let calls = AtomicUsize::new(0);
    let compute = || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Computed { value: true, store: Some(StoreSpec { tags: vec!["t:1".into()], ttl: Some(Duration::from_secs(60)) }) }
    };
    let a: bool = cache.get_or_compute_tagged("k", compute).await;
    let b: bool = cache.get_or_compute_tagged("k", compute).await;
    assert!(a && b);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "second call must hit cache");

    cache.bust_tag("t:1").await;
    let _c: bool = cache.get_or_compute_tagged("k", compute).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2, "bust_tag must force recompute");
}

#[tokio::test]
async fn no_store_computed_is_never_cached() {
    let cache = Cache::memory();
    let calls = AtomicUsize::new(0);
    let compute = || async { calls.fetch_add(1, Ordering::SeqCst); Computed { value: false, store: None } };
    let _: bool = cache.get_or_compute_tagged("k2", compute).await;
    let _: bool = cache.get_or_compute_tagged("k2", compute).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2, "store: None must never cache");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-cache --test tagged`
Expected: FAIL — `Computed` / `get_or_compute_tagged` not found.

- [ ] **Step 3: Add the types and methods.** In `plugins/umbral-cache/src/lib.rs`:

```rust
/// Result of a `get_or_compute_tagged` miss-compute.
pub struct Computed<T> {
    pub value: T,
    /// `None` opts this value out of caching (e.g. a volatile decision).
    pub store: Option<StoreSpec>,
}

pub struct StoreSpec {
    pub tags: Vec<String>,
    pub ttl: Option<std::time::Duration>,
}
```

Add to the `CacheBackend` trait (default impls keep third-party backends compiling):

```rust
    /// Store a value AND index its key under each tag for later `bust_tag`.
    /// Default: ignore tags (TTL-only invalidation).
    async fn set_tagged(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>, _tags: &[String]) {
        self.set(key, value, ttl).await;
    }
    /// Delete every key indexed under `tag`, then the tag itself.
    /// Default: no-op (backend relies on TTL).
    async fn bust_tag(&self, _tag: &str) {}
```

Add to `impl Cache`:

```rust
    pub async fn bust_tag(&self, tag: &str) {
        self.backend.bust_tag(tag).await;
    }

    pub async fn get_or_compute_tagged<T, Fut>(&self, key: &str, compute: impl FnOnce() -> Fut) -> T
    where
        T: Serialize + DeserializeOwned,
        Fut: std::future::Future<Output = Computed<T>>,
    {
        if let Some(hit) = self.get::<T>(key).await {
            return hit;
        }
        let c = compute().await;
        if let Some(spec) = c.store {
            if let Ok(bytes) = serde_json::to_vec(&c.value) {
                self.backend.set_tagged(key, bytes, spec.ttl, &spec.tags).await;
            }
        }
        c.value
    }
```

- [ ] **Step 4: Run test — still fails** on `bust_tag` (MemoryBackend default no-ops it). That is expected; the `MemoryBackend` real impl lands in Step 5 of this task.

- [ ] **Step 5: Give `MemoryBackend` real tag support.** Restructure its state to hold both maps under one lock:

```rust
#[derive(Default)]
struct MemoryState {
    values: HashMap<String, MemoryEntry>,
    tags: HashMap<String, std::collections::HashSet<String>>,
}

#[derive(Default)]
pub struct MemoryBackend {
    inner: Mutex<MemoryState>,
}
```

Update the existing `get`/`set`/`delete`/`clear` to use `inner.lock().await.values` (same logic, one field deeper). Then add:

```rust
    async fn set_tagged(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>, tags: &[String]) {
        let expires_at = ttl.and_then(|d| chrono::Duration::from_std(d).ok().and_then(|cd| Utc::now().checked_add_signed(cd)));
        let mut st = self.inner.lock().await;
        st.values.insert(key.to_string(), MemoryEntry { value, expires_at });
        for t in tags {
            st.tags.entry(t.clone()).or_default().insert(key.to_string());
        }
    }
    async fn bust_tag(&self, tag: &str) {
        let mut st = self.inner.lock().await;
        if let Some(keys) = st.tags.remove(tag) {
            for k in keys { st.values.remove(&k); }
        }
    }
```

- [ ] **Step 6: Run tests — pass**

Run: `cargo test -p umbral-cache --test tagged`
Expected: PASS (both tests).

- [ ] **Step 7: Verify + commit**

```bash
cargo fmt && cargo clippy -p umbral-cache --all-targets
git add plugins/umbral-cache/src/lib.rs plugins/umbral-cache/tests/tagged.rs
git commit -m "feat(cache): tagged get_or_compute + bust_tag primitive (memory backend)"
```

### Task 3: SqliteBackend tag support

**Files:**
- Modify: `plugins/umbral-cache/src/lib.rs` (`SqliteBackend`)
- Test: `plugins/umbral-cache/tests/tagged_sqlite.rs` (create)

**Interfaces:**
- Consumes: `Computed`/`StoreSpec` (Task 2).

- [ ] **Step 1: Write the failing test**

```rust
// plugins/umbral-cache/tests/tagged_sqlite.rs
use std::time::Duration;
use umbral_cache::{Cache, Computed, StoreSpec};

#[tokio::test]
async fn sqlite_bust_tag_removes_indexed_keys() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let cache = Cache::sqlite(pool).await.unwrap();
    let compute = |v: bool| move || async move {
        Computed { value: v, store: Some(StoreSpec { tags: vec!["chan:9".into()], ttl: Some(Duration::from_secs(60)) }) }
    };
    let _: bool = cache.get_or_compute_tagged("mediaacc:f:u1", compute(true)).await;
    // hit:
    assert_eq!(cache.get::<bool>("mediaacc:f:u1").await, Some(true));
    cache.bust_tag("chan:9").await;
    assert_eq!(cache.get::<bool>("mediaacc:f:u1").await, None, "busted key gone");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-cache --test tagged_sqlite`
Expected: FAIL (key still present — default `bust_tag` no-op).

- [ ] **Step 3: Add the tag table + impls.** In `SqliteBackend::new`, alongside the existing `umbral_cache` DDL, add:

```rust
sqlx::query(
    "CREATE TABLE IF NOT EXISTS umbral_cache_tag (
        tag TEXT NOT NULL,
        key TEXT NOT NULL,
        PRIMARY KEY (tag, key)
    )",
)
.execute(&pool)
.await?;
```

Add to `impl CacheBackend for SqliteBackend`:

```rust
    async fn set_tagged(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>, tags: &[String]) {
        self.set(key, value, ttl).await;
        for t in tags {
            let _ = sqlx::query(
                "INSERT INTO umbral_cache_tag (tag, key) VALUES (?, ?) ON CONFLICT DO NOTHING",
            )
            .bind(t)
            .bind(key)
            .execute(&self.pool)
            .await;
        }
    }
    async fn bust_tag(&self, tag: &str) {
        let _ = sqlx::query(
            "DELETE FROM umbral_cache WHERE key IN (SELECT key FROM umbral_cache_tag WHERE tag = ?)",
        )
        .bind(tag)
        .execute(&self.pool)
        .await;
        let _ = sqlx::query("DELETE FROM umbral_cache_tag WHERE tag = ?")
            .bind(tag)
            .execute(&self.pool)
            .await;
    }
```

- [ ] **Step 4: Run test — pass**

Run: `cargo test -p umbral-cache --test tagged_sqlite`
Expected: PASS.

- [ ] **Step 5: Verify + commit**

```bash
cargo fmt && cargo clippy -p umbral-cache --all-targets
git add plugins/umbral-cache/src/lib.rs plugins/umbral-cache/tests/tagged_sqlite.rs
git commit -m "feat(cache): sqlite backend tag index + bust_tag"
```

### Task 4: RedisBackend tag support (feature-gated)

**Files:**
- Modify: `plugins/umbral-cache/src/lib.rs` (`RedisBackend`, behind `#[cfg(feature = "redis")]`)
- Test: `plugins/umbral-cache/tests/tagged_redis.rs` (create; skips when `UMBRAL_TEST_REDIS_URL` unset)

**Interfaces:**
- Consumes: `Computed`/`StoreSpec`.

- [ ] **Step 1: Write the test (env-gated skip)**

```rust
// plugins/umbral-cache/tests/tagged_redis.rs
#![cfg(feature = "redis")]
use std::time::Duration;
use umbral_cache::{Cache, Computed, StoreSpec};

#[tokio::test]
async fn redis_bust_tag_removes_indexed_keys() {
    let Ok(url) = std::env::var("UMBRAL_TEST_REDIS_URL") else { eprintln!("skip: no UMBRAL_TEST_REDIS_URL"); return; };
    let cache = Cache::redis(&url).await.unwrap();
    let compute = || async { Computed { value: true, store: Some(StoreSpec { tags: vec!["chan:redis".into()], ttl: Some(Duration::from_secs(60)) }) } };
    let _: bool = cache.get_or_compute_tagged("mediaacc:rf:u1", compute).await;
    assert_eq!(cache.get::<bool>("mediaacc:rf:u1").await, Some(true));
    cache.bust_tag("chan:redis").await;
    assert_eq!(cache.get::<bool>("mediaacc:rf:u1").await, None);
}
```

- [ ] **Step 2: Run test** (`cargo test -p umbral-cache --features redis --test tagged_redis`) — compiles; skips or fails on the missing methods.

- [ ] **Step 3: Add impls** to `impl CacheBackend for RedisBackend`:

```rust
    async fn set_tagged(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>, tags: &[String]) {
        use redis::AsyncCommands;
        self.set(key, value, ttl).await; // reuse existing SET/EX
        let mut conn = self.client.clone();
        for t in tags {
            let _: Result<(), _> = conn.sadd(self.k(&format!("utag:{t}")), self.k(key)).await;
        }
    }
    async fn bust_tag(&self, tag: &str) {
        use redis::AsyncCommands;
        let mut conn = self.client.clone();
        let set_key = self.k(&format!("utag:{tag}"));
        let members: Vec<String> = conn.smembers(&set_key).await.unwrap_or_default();
        for m in members {
            let _: Result<(), _> = conn.del(m).await;
        }
        let _: Result<(), _> = conn.del(&set_key).await;
    }
```

Note: `self.k(key)` already namespaces; storing the already-namespaced member means `del(m)` uses the full key. Verify `self.k` is idempotent-safe here (member stored is `self.k(key)`, deleted directly — do NOT re-apply `k`).

- [ ] **Step 4: Run test** with a live Redis if available, else confirm it compiles and skips.

- [ ] **Step 5: Verify + commit**

```bash
cargo fmt && cargo clippy -p umbral-cache --features redis --all-targets
git add plugins/umbral-cache/src/lib.rs plugins/umbral-cache/tests/tagged_redis.rs
git commit -m "feat(cache): redis backend tag index + bust_tag"
```

---

## Phase 2 — Media access surface

### Task 5: `MediaCaller` type + resolver

**Files:**
- Modify: `plugins/umbral-storage/src/lib.rs`
- Test: `plugins/umbral-storage/tests/media_caller.rs` (create)

**Interfaces:**
- Produces: `MediaCaller`, `MediaCaller::resolve(&HeaderMap) -> MediaCaller`, `user_id()`, `is_authenticated()`, `has_role()`, fields `is_superuser`, `is_staff`, `roles`.
- Consumes: `umbral::auth::default_authentication()` → `.authenticate(&headers)` → `Option<Identity>` (`user_id: String`, `is_staff`, `is_superuser`, `extras: HashMap<String, Value>`).

- [ ] **Step 1: Write the failing test** (anonymous resolves to no user; roles read from a synthesized identity is covered in Task 8's integration — here assert the anonymous/`headers` path):

```rust
// plugins/umbral-storage/tests/media_caller.rs
use http::HeaderMap;
use umbral_storage::MediaCaller;

#[tokio::test]
async fn anonymous_headers_resolve_to_unauthenticated_caller() {
    let caller = MediaCaller::resolve(&HeaderMap::new()).await;
    assert!(!caller.is_authenticated());
    assert_eq!(caller.user_id(), None);
    assert!(!caller.is_superuser);
    assert!(caller.roles.is_empty());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p umbral-storage --test media_caller`
Expected: FAIL — `MediaCaller` not found.

- [ ] **Step 3: Implement**

```rust
pub struct MediaCaller {
    user_id: Option<String>,
    pub is_superuser: bool,
    pub is_staff: bool,
    pub roles: Vec<String>,
}

impl MediaCaller {
    pub async fn resolve(headers: &http::HeaderMap) -> Self {
        let identity = match umbral::auth::default_authentication() {
            Some(auth) => auth.authenticate(headers).await,
            None => None,
        };
        match identity {
            Some(id) => {
                // Role names come from an `extras["roles"]` convention the auth
                // layer may populate; absent that, `roles` is empty and callers
                // use `media_access_staff` / an in-closure check.
                let roles = id.extras.get("roles")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                Self { user_id: Some(id.user_id), is_superuser: id.is_superuser, is_staff: id.is_staff, roles }
            }
            None => Self { user_id: None, is_superuser: false, is_staff: false, roles: Vec::new() },
        }
    }
    pub fn user_id(&self) -> Option<&str> { self.user_id.as_deref() }
    pub fn is_authenticated(&self) -> bool { self.user_id.is_some() }
    pub fn has_role(&self, role: &str) -> bool { self.roles.iter().any(|r| r == role) }
}
```

Re-export from the facade if storage types are surfaced there (match how `MediaFile` is exported).

- [ ] **Step 4: Run test — pass**

Run: `cargo test -p umbral-storage --test media_caller`
Expected: PASS.

- [ ] **Step 5: Verify + commit**

```bash
cargo fmt && cargo clippy -p umbral-storage --all-targets
git add plugins/umbral-storage/src/lib.rs plugins/umbral-storage/tests/media_caller.rs
git commit -m "feat(storage): MediaCaller resolved from ambient auth"
```

### Task 6: `Decision` type

**Files:**
- Modify: `plugins/umbral-storage/src/lib.rs`
- Test: unit test inline in the same file (or `tests/decision.rs`)

**Interfaces:**
- Produces: `Decision`, `allow()`, `deny()`, `of(bool)`, `depends_on(iter)`, `ttl(Duration)`, `no_cache()`; fields readable by the wrapper (`allow: bool`, `tags: Vec<String>`, `ttl: Option<Duration>`, `cache: bool`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn decision_builders_default_to_cacheable() {
    let d = umbral_storage::Decision::of(true).depends_on(["chan:1".to_string()]);
    assert!(d.is_allow());
    assert!(d.is_cacheable());
    assert_eq!(d.tags(), &["chan:1".to_string()]);
    let n = umbral_storage::Decision::allow().no_cache();
    assert!(!n.is_cacheable());
}
```

- [ ] **Step 2: Run to verify it fails.** Run: `cargo test -p umbral-storage decision_builders`. Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
pub struct Decision {
    allow: bool,
    tags: Vec<String>,
    ttl: Option<std::time::Duration>,
    cache: bool,
}
impl Decision {
    pub fn allow() -> Self { Self { allow: true, tags: Vec::new(), ttl: None, cache: true } }
    pub fn deny() -> Self { Self { allow: false, tags: Vec::new(), ttl: None, cache: true } }
    pub fn of(allow: bool) -> Self { Self { allow, tags: Vec::new(), ttl: None, cache: true } }
    pub fn depends_on(mut self, tags: impl IntoIterator<Item = String>) -> Self { self.tags.extend(tags); self }
    pub fn ttl(mut self, ttl: std::time::Duration) -> Self { self.ttl = Some(ttl); self }
    pub fn no_cache(mut self) -> Self { self.cache = false; self }
    // accessors used by the wrapper + tests:
    pub fn is_allow(&self) -> bool { self.allow }
    pub fn is_cacheable(&self) -> bool { self.cache }
    pub fn tags(&self) -> &[String] { &self.tags }
    pub(crate) fn ttl_or_default(&self) -> std::time::Duration { self.ttl.unwrap_or(MEDIA_ACCESS_DEFAULT_TTL) }
}
```

Add `const MEDIA_ACCESS_DEFAULT_TTL: std::time::Duration = std::time::Duration::from_secs(60);`.

- [ ] **Step 4: Run test — pass.** Run: `cargo test -p umbral-storage decision_builders`. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p umbral-storage --all-targets
git add plugins/umbral-storage/src/lib.rs
git commit -m "feat(storage): Decision type for cached media access"
```

### Task 7: `media_access_cached` — the cache-first wrapper

**Files:**
- Modify: `plugins/umbral-storage/src/lib.rs`
- Test: `plugins/umbral-storage/tests/media_access_cached.rs` (create)

**Interfaces:**
- Consumes: `MediaCaller` (Task 5), `Decision` (Task 6), `Cache::get_or_compute_tagged` + `Computed`/`StoreSpec` (Tasks 1–2), the ambient cache accessor decided in Task 1.
- Produces: `StoragePlugin::media_access_cached(closure) -> Self`.

- [ ] **Step 1: Write the failing test** (query-counter proves cache-first: closure runs once per distinct caller):

```rust
// plugins/umbral-storage/tests/media_access_cached.rs
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use http::HeaderMap;
use umbral_storage::{Decision, MediaCaller, StoragePlugin};

// Build a plugin, install a memory cache as ambient, drive the resolved
// MediaAccessFn twice for the same (headers,key) and assert the closure body
// ran once. (Helper to reach the built MediaAccessFn: expose a
// `#[doc(hidden)] pub fn resolve_access(&self) -> Option<MediaAccessFn>` on
// StoragePlugin for tests, or drive it through the media route — mirror the
// harness in the existing gaps4 #58 media tests: `ls plugins/umbral-storage/tests`.)

#[tokio::test]
async fn cache_first_runs_closure_once_per_caller() {
    // install ambient memory cache (mirror how CachePlugin sets ambient in tests)
    umbral_cache::install_ambient_for_test(umbral_cache::Cache::memory());
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let plugin = StoragePlugin::new().media("/media", "./media").media_access_cached(move |caller: MediaCaller, _key: &str| {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            Decision::of(caller.user_id().is_some()).depends_on(["t:x".to_string()])
        }
    });
    let access = plugin.resolve_access().expect("access fn");
    let h = HeaderMap::new();
    let _ = access(&h, "invoices/1.pdf").await;
    let _ = access(&h, "invoices/1.pdf").await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "second access must hit cache");
}
```

If `install_ambient_for_test` / `resolve_access` don't exist, add them as `#[doc(hidden)]` test helpers in this task (a one-line `pub fn install_ambient_for_test(c: Cache)` that sets the `AMBIENT_CACHE` OnceLock, and `pub fn resolve_access(&self) -> Option<MediaAccessFn>` returning `self.media_access.clone()`).

- [ ] **Step 2: Run to verify it fails.** Run: `cargo test -p umbral-storage --features cache --test media_access_cached`. Expected: FAIL — `media_access_cached` not found.

- [ ] **Step 3: Implement `media_access_cached`**

```rust
pub fn media_access_cached<F, Fut>(mut self, f: F) -> Self
where
    F: Fn(MediaCaller, &str) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Decision> + Send + 'static,
{
    let f = Arc::new(f);
    self.media_access = Some(Arc::new(move |headers: &http::HeaderMap, key: &str| {
        let f = f.clone();
        let headers = headers.clone();
        let key = key.to_string();
        Box::pin(async move {
            let caller = MediaCaller::resolve(&headers).await;
            let caller_id = caller.user_id().map(str::to_owned).unwrap_or_else(|| "anon".into());
            let cache_key = format!("mediaacc:{key}:{caller_id}");
            let run_key = key.clone();
            let compute = move || async move {
                let d = f(caller, &run_key).await;
                let store = d.is_cacheable().then(|| umbral_cache::StoreSpec {
                    tags: d.tags().to_vec(),
                    ttl: Some(d.ttl_or_default()),
                });
                umbral_cache::Computed { value: d.is_allow(), store }
            };
            match umbral_cache::ambient() {
                Some(cache) => cache.get_or_compute_tagged::<bool, _>(&cache_key, compute).await,
                None => compute().await.value, // no cache configured → always compute
            }
        })
    }));
    self
}
```

(If Task 1 chose a core cache seam instead of the `umbral-cache` crate, swap `umbral_cache::ambient()` / `Computed` / `StoreSpec` for the facade path. Keep everything else identical.)

- [ ] **Step 4: Run test — pass.** Run: `cargo test -p umbral-storage --features cache --test media_access_cached`. Expected: PASS (closure ran once).

- [ ] **Step 5: Verify + commit**

```bash
cargo fmt && cargo clippy -p umbral-storage --features cache --all-targets
git add plugins/umbral-storage/src/lib.rs plugins/umbral-storage/tests/media_access_cached.rs
git commit -m "feat(storage): media_access_cached — cache-first per-caller decision"
```

### Task 8: Role presets — `media_access_staff` + `media_access_roles`

**Files:**
- Modify: `plugins/umbral-storage/src/lib.rs`
- Test: `plugins/umbral-storage/tests/media_role_presets.rs` (create)

**Interfaces:**
- Consumes: `media_access_cached`, `MediaCaller`.
- Produces: `StoragePlugin::media_access_staff() -> Self`, `StoragePlugin::media_access_roles(roles) -> Self`.

- [ ] **Step 1: Write the failing test** (drive the resolved access fn with a fabricated identity via a test auth backend — mirror how existing storage identity tests install `default_authentication`; `grep -rn "default_authentication\|set_default_authentication\|install.*auth" plugins/umbral-storage/tests crates/umbral-core/src/auth_contract.rs`):

```rust
// plugins/umbral-storage/tests/media_role_presets.rs
// Install a stub auth backend returning an Identity with is_staff=true, assert
// media_access_staff() allows; with is_staff=false assert it denies. Follow the
// existing storage test harness for installing a stub Authentication.
```

- [ ] **Step 2: Run to verify it fails.** Run: `cargo test -p umbral-storage --features cache --test media_role_presets`. Expected: FAIL.

- [ ] **Step 3: Implement (sugar over `media_access_cached`)**

```rust
pub fn media_access_staff(self) -> Self {
    self.media_access_cached(|caller: MediaCaller, _key: &str| async move {
        Decision::of(caller.is_staff || caller.is_superuser)
    })
}

pub fn media_access_roles<I, S>(self, roles: I) -> Self
where I: IntoIterator<Item = S>, S: Into<String> {
    let allow: Vec<String> = roles.into_iter().map(Into::into).collect();
    self.media_access_cached(move |caller: MediaCaller, _key: &str| {
        let allow = allow.clone();
        async move {
            let ok = caller.is_superuser || allow.iter().any(|r| caller.has_role(r));
            Decision::of(ok)
        }
    })
}
```

- [ ] **Step 4: Run test — pass.**

- [ ] **Step 5: Verify + commit**

```bash
cargo fmt && cargo clippy -p umbral-storage --features cache --all-targets
git add plugins/umbral-storage/src/lib.rs plugins/umbral-storage/tests/media_role_presets.rs
git commit -m "feat(storage): media_access_staff + media_access_roles presets"
```

### Task 9: `media_invalidate_on::<M>` + signal wiring

**Files:**
- Modify: `plugins/umbral-storage/src/lib.rs` (struct field, builder, `on_ready`)
- Test: `plugins/umbral-storage/tests/media_invalidation.rs` (create)

**Interfaces:**
- Consumes: `umbral::signals::subscribe_async`, `Cache::bust_tag`, the ambient cache.
- Produces: `StoragePlugin::media_invalidate_on::<M>(map_fn) -> Self`, where `M: Model + DeserializeOwned` and `map_fn: Fn(&M) -> Vec<String>`.

- [ ] **Step 1: Write the failing test** (behavioral — cache a decision, then fire `post_delete` by deleting the source row through the ORM, assert the next access recomputes):

```rust
// plugins/umbral-storage/tests/media_invalidation.rs
// 1. install ambient memory cache + a model M (e.g. a test Membership with a `chan` field)
// 2. plugin.media_access_cached(|c,k| ... Decision::of(true).depends_on(["chan:9"]))
//        .media_invalidate_on::<Membership>(|row| vec![format!("chan:{}", row.chan)])
// 3. build the App so on_ready registers the subscribers; drive access → cached
// 4. delete/save a Membership row with chan=9 THROUGH THE ORM (fires post_delete:membership)
// 5. next access must recompute (counter increments) → proves the tag was busted
```

- [ ] **Step 2: Run to verify it fails.** Expected: FAIL — `media_invalidate_on` not found.

- [ ] **Step 3: Implement.** Add a field to `StoragePlugin`:

```rust
    media_invalidators: Vec<(&'static str, Arc<dyn Fn(&serde_json::Value) -> Vec<String> + Send + Sync>)>,
```

(Add `media_invalidators: Vec::new()` to the plugin constructor. `StoragePlugin` derives `Clone`; `Arc<dyn Fn…>` is `Clone`, so the `Vec` is fine.)

Builder:

```rust
pub fn media_invalidate_on<M, F>(mut self, map_fn: F) -> Self
where
    M: umbral::migrate::ModelMetaProvider + serde::de::DeserializeOwned + 'static, // use M::TABLE; verify the exact trait exposing TABLE (grep `const TABLE` in umbral-core orm)
    F: Fn(&M) -> Vec<String> + Send + Sync + 'static,
{
    let erased = Arc::new(move |payload: &serde_json::Value| -> Vec<String> {
        // signal payload is { "instance": <row json>, ... }
        let Some(instance) = payload.get("instance") else { return Vec::new(); };
        match serde_json::from_value::<M>(instance.clone()) {
            Ok(row) => map_fn(&row),
            Err(_) => Vec::new(),
        }
    });
    self.media_invalidators.push((M::TABLE, erased));
    self
}
```

In `on_ready`, register subscribers for each invalidator (both save and delete):

```rust
for (table, map) in &self.media_invalidators {
    for event in ["post_save", "post_delete"] {
        let map = map.clone();
        umbral::signals::subscribe_async(&format!("{event}:{table}"), move |payload: &serde_json::Value| {
            let tags = map(payload);
            async move {
                if let Some(cache) = umbral_cache::ambient() {
                    for t in tags { cache.bust_tag(&t).await; }
                }
            }
        });
    }
}
```

Verify the exact `TABLE` accessor and Model bound during Step 3 (`grep -n "const TABLE" crates/umbral-core/src/orm`); adjust the `M` bound to the real trait that exposes `TABLE` and is object-safe-free here (it is a plain generic, not a trait object).

- [ ] **Step 4: Run test — pass.**

Run: `cargo test -p umbral-storage --features cache --test media_invalidation`
Expected: PASS (access recomputes after the ORM delete).

- [ ] **Step 5: Verify + commit**

```bash
cargo fmt && cargo clippy -p umbral-storage --features cache --all-targets
git add plugins/umbral-storage/src/lib.rs plugins/umbral-storage/tests/media_invalidation.rs
git commit -m "feat(storage): media_invalidate_on — signal-driven cache busting"
```

### Task 10: End-to-end enforcement test (FS + non-FS) + no-cache fallthrough

**Files:**
- Test: `plugins/umbral-storage/tests/media_access_e2e.rs` (create)
- No production change expected (media_gate already routes both backends through the `MediaAccessFn`).

- [ ] **Step 1: Write tests** covering, via the real media route (mirror the gaps4 #58 harness — `grep -rln "forbidden\|/media/\|retrieve_stream" plugins/umbral-storage/tests`):
  - FS guard: a denied caller gets 403, an allowed caller gets the bytes, and a second allowed request is served from cache (assert via the query counter as in Task 7).
  - No ambient cache installed: enforcement still correct (closure runs every time; allowed served, denied 403).
  - Superuser `Decision::allow()` and anonymous `Decision::deny()` behave (cached but correct).

- [ ] **Step 2: Run — expect PASS** (no prod change needed). If a test reveals `media_gate` does need a touch, that is a real finding — fix the gate, do not weaken the test.

Run: `cargo test -p umbral-storage --features cache --test media_access_e2e`

- [ ] **Step 3: Commit**

```bash
git add plugins/umbral-storage/tests/media_access_e2e.rs
git commit -m "test(storage): e2e media-access caching across FS and non-FS backends"
```

### Task 11: Docs

**Files:**
- Create: `documentation/docs/v0.0.1/storage/media-access.mdx`
- Create: `documentation/docs/v0.0.1/cache/tagged-invalidation.mdx` (+ `_category_.json` if the `cache` area folder is new)

- [ ] **Step 1: Write `media-access.mdx`** — frontmatter (title/description/sidebar_position), one paragraph purpose, the `media_access_cached` + `media_invalidate_on` example from the spec, the `media_access_staff`/`media_access_roles` presets, the `extras["roles"]` convention note, and a link to the spec.

- [ ] **Step 2: Write `tagged-invalidation.mdx`** — purpose of `get_or_compute_tagged` / `bust_tag`, one example, link to the spec.

- [ ] **Step 3: Commit**

```bash
git add documentation/docs/v0.0.1/storage/media-access.mdx documentation/docs/v0.0.1/cache/
git commit -m "docs(storage,cache): media access + tagged cache invalidation pages"
```

---

## Self-review notes (author)

- **Spec coverage:** API (Tasks 5–9), cache tag primitive across 3 backends (Tasks 2–4), signal invalidation + TTL backstop (Tasks 2/9), enforcement path (Task 10, no gate change), security semantics (Tasks 7/10 — fail-to-compute when no cache, framework-owned key), config ergonomics (default TTL Task 6, optional invalidation Task 9), testing (each task), docs (Task 11). All covered.
- **Open verification items handed to the executor (not placeholders — each names the exact grep + fallback):** Task 1 cache-seam location; Task 5 facade re-export parity; Task 7 `install_ambient_for_test`/`resolve_access` test helpers; Task 9 the `M::TABLE` accessor trait bound. Each has a concrete resolution path in-task.
- **Type consistency:** `Computed`/`StoreSpec` (Task 2) consumed unchanged in Tasks 3/4/7; `Decision` accessors (`is_allow`/`is_cacheable`/`tags`/`ttl_or_default`, Task 6) consumed in Task 7; `MediaCaller` API (Task 5) consumed in Tasks 7/8.
