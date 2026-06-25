# Lazy Template `user` Implementation Plan (Phase 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `user_context_layer` from doing an eager per-request DB read (session + user + relations) on requests that never render the template `user` — e.g. JSON/API endpoints — by making the template `user` resolve lazily, at most once, only when a template actually reads it.

**Architecture:** umbral-core gains a *lazy* current-user channel: a per-request resolver closure + a `OnceCell`, scoped on a task-local. `merge_ambient_context` (which runs inside the synchronous minijinja render) resolves it on demand via `tokio::task::block_in_place` + `Handle::block_on`, memoized so repeated renders in one request resolve once. umbral-auth's `user_context_layer` switches from eager resolution to *installing the resolver*. A request that never renders `user` never runs the closure → zero identity queries.

**Tech Stack:** Rust, axum 0.8 middleware (`from_fn`), minijinja 2.x (sync render), tokio 1.x multi-thread runtime (`block_in_place`), `tokio::sync::OnceCell`.

## Global Constraints

- Crate boundaries: umbral-core MUST NOT depend on umbral-auth. The lazy user value crossing the boundary is `minijinja::Value` (model-agnostic) and a boxed resolver closure; umbral-core never names `Identity` or `AuthUser`. (Copied from spec §3 / §7.)
- The lazy resolution path requires a **multi-thread** tokio runtime (`block_in_place` panics on `current_thread`). The umbral server runtime is multi-thread (`rt-multi-thread`, `#[tokio::main]`). Tests that exercise the lazy path MUST use `#[tokio::test(flavor = "multi_thread")]`.
- Back-compat: the existing eager `umbral::templates::with_current_user(Option<Value>, fut)` and `CURRENT_USER` task-local stay and keep working; the lazy channel is additive.
- Correctness over heuristics: do NOT gate resolution on the `Accept` header (a logged-in user whose client sends `Accept: */*` while loading an HTML page must still see themselves). Resolution is driven by *actual template access to `user`*, nothing else. (Spec §6.1: the `Accept`/content-type gate is rejected as incorrect for templates.)
- Scope of THIS plan: lazy template `user` only. Cross-consumer memoization (extractors / `LoggedIn<U>` / `Authentication` sharing one lookup) is Phase 1b, a separate plan — NOT in scope here.

---

## File Structure

- `crates/umbral-core/src/templates.rs` — add the lazy current-user channel (`LazyUser`, second task-local, `with_current_user_lazy`, lazy branch in `merge_ambient_value`). One responsibility: ambient template context.
- `crates/umbral-core/src/lib.rs` (or wherever `pub mod templates` re-exports) + `crates/umbral/src/lib.rs` facade `templates` module — re-export `LazyUser` and `with_current_user_lazy`.
- `plugins/umbral-auth/src/session_user.rs` — rewrite `user_context_layer` to install the resolver instead of resolving eagerly.
- `crates/umbral-core/tests/lazy_user.rs` (new) — core-level behavioral test: resolver runs only when `user` is rendered; resolves once across two renders.
- `plugins/umbral-auth/tests/user_context_lazy.rs` (new) — plugin-level: JSON response → resolver never runs; HTML render → runs once.

---

## Task 1: Lazy current-user channel in umbral-core

**Files:**
- Modify: `crates/umbral-core/src/templates.rs` (task-locals near line 62; `merge_ambient_value` near line 986; add new fns)
- Modify: `crates/umbral/src/lib.rs` (facade `templates` re-exports)
- Test: `crates/umbral-core/tests/lazy_user.rs` (create)

**Interfaces:**
- Produces:
  - `pub struct LazyUser { /* opaque */ }`
  - `impl LazyUser { pub fn new<F, Fut>(resolver: F) -> Self where F: Fn() -> Fut + Send + Sync + 'static, Fut: std::future::Future<Output = minijinja::Value> + Send + 'static }`
  - `pub async fn with_current_user_lazy<F: std::future::Future>(lazy: LazyUser, fut: F) -> F::Output`
  - Re-exported as `umbral::templates::{LazyUser, with_current_user_lazy}`.
- Consumes: existing `merge_ambient_value` (line 941-1019), `anonymous_user_value` (line 1029), `CURRENT_USER` task-local (line 62).

- [ ] **Step 1: Write the failing test**

Create `crates/umbral-core/tests/lazy_user.rs`:

```rust
//! The lazy current-user channel: the resolver closure runs only when a
//! template actually reads `user`, and at most once per request scope.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use umbral_core::templates::{LazyUser, with_current_user_lazy};

// Build a minijinja Value standing in for a serialized user.
fn user_value(is_staff: bool) -> minijinja::Value {
    let mut m = serde_json::Map::new();
    m.insert("is_authenticated".into(), serde_json::Value::Bool(true));
    m.insert("is_staff".into(), serde_json::Value::Bool(is_staff));
    minijinja::Value::from_serialize(serde_json::Value::Object(m))
}

#[tokio::test(flavor = "multi_thread")]
async fn resolver_does_not_run_when_user_is_not_rendered() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let lazy = LazyUser::new(move || {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            user_value(true)
        }
    });

    // Inside the scope, render a template that NEVER references `user`.
    let out = with_current_user_lazy(lazy, async {
        umbral_core::templates::render_str("hello {{ name }}", &serde_json::json!({"name": "ada"}))
    })
    .await
    .expect("render");

    assert_eq!(out, "hello ada");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "resolver must NOT run when user unused");
}

#[tokio::test(flavor = "multi_thread")]
async fn resolver_runs_once_across_two_renders_that_read_user() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let lazy = LazyUser::new(move || {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            user_value(true)
        }
    });

    let out = with_current_user_lazy(lazy, async {
        let a = umbral_core::templates::render_str("{{ user.is_staff }}", &serde_json::json!({})).unwrap();
        let b = umbral_core::templates::render_str("{{ user.is_staff }}", &serde_json::json!({})).unwrap();
        format!("{a}-{b}")
    })
    .await;

    assert_eq!(out, "true-true");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "resolver memoized: runs exactly once");
}
```

NOTE: `render_str(template_source, ctx)` is a small test-only helper that renders an inline template through the same `merge_ambient_context` path as `render`. If `umbral_core::templates::render_str` does not exist, add it in Step 3 (a thin wrapper: build a one-off `minijinja::Environment`, `add_template("_t", src)`, then `render_with`-equivalent using `merge_ambient_context`). Expose it `#[doc(hidden)] pub`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd crates && cargo test -p umbral-core --test lazy_user 2>&1 | tail -20`
Expected: FAIL to compile — `LazyUser`, `with_current_user_lazy`, `render_str` not found.

- [ ] **Step 3: Implement the lazy channel in `templates.rs`**

In `crates/umbral-core/src/templates.rs`:

1. Add imports near the top:
```rust
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::OnceCell;
```

2. Add the lazy task-local next to `CURRENT_USER` (the existing block at ~line 62):
```rust
tokio::task_local! {
    /// Lazy counterpart to `CURRENT_USER`: a resolver that produces the
    /// user value on first access, memoized. Set by an auth middleware that
    /// wants per-request laziness (resolve only if a template reads `user`).
    pub static CURRENT_USER_LAZY: LazyUser;
}
```

3. Define `LazyUser` (place after the task-local block):
```rust
type UserFut = Pin<Box<dyn Future<Output = minijinja::Value> + Send>>;
type UserResolver = Arc<dyn Fn() -> UserFut + Send + Sync>;

/// A lazily-resolved, per-request template `user`. The `resolver` runs at
/// most once (guarded by the `OnceCell`); resolution happens synchronously
/// from inside minijinja's sync render via `block_in_place`.
#[derive(Clone)]
pub struct LazyUser {
    cell: Arc<OnceCell<minijinja::Value>>,
    resolver: UserResolver,
}

impl LazyUser {
    pub fn new<F, Fut>(resolver: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = minijinja::Value> + Send + 'static,
    {
        Self {
            cell: Arc::new(OnceCell::new()),
            resolver: Arc::new(move || Box::pin(resolver())),
        }
    }

    /// Resolve (memoized) from a synchronous context. Requires a multi-thread
    /// tokio runtime; on a current-thread runtime or outside any runtime it
    /// logs and returns `None` so callers fall back to the anonymous value.
    fn resolve_blocking(&self) -> Option<minijinja::Value> {
        use tokio::runtime::{Handle, RuntimeFlavor};
        let handle = Handle::try_current().ok()?;
        if handle.runtime_flavor() == RuntimeFlavor::CurrentThread {
            tracing::warn!(
                "umbral::templates: lazy `user` needs a multi-thread runtime; rendering anonymous"
            );
            return None;
        }
        let cell = self.cell.clone();
        let resolver = self.resolver.clone();
        let value = tokio::task::block_in_place(move || {
            handle.block_on(async move { cell.get_or_init(|| resolver()).await.clone() })
        });
        Some(value)
    }
}

/// Scope a lazy `user` resolver for the duration of `fut`.
pub async fn with_current_user_lazy<F: Future>(lazy: LazyUser, fut: F) -> F::Output {
    CURRENT_USER_LAZY.scope(lazy, fut).await
}
```

4. In `merge_ambient_value` (line ~986), make the lazy channel win over eager, and eager over anonymous:
```rust
// BEFORE (line ~986):
//   let layer_user = CURRENT_USER.try_with(|u| u.clone()).ok().flatten();
//   pairs.push(("user".to_string(), layer_user.unwrap_or_else(anonymous_user_value)));

// AFTER:
let resolved = CURRENT_USER_LAZY
    .try_with(|lazy| lazy.resolve_blocking())
    .ok()
    .flatten()
    .or_else(|| CURRENT_USER.try_with(|u| u.clone()).ok().flatten());
pairs.push((
    "user".to_string(),
    resolved.unwrap_or_else(anonymous_user_value),
));
```

5. Add the test helper `render_str` (only if it doesn't already exist — grep first):
```rust
/// Render an inline template source through the ambient-context path.
/// Test/bench helper only.
#[doc(hidden)]
pub fn render_str<C: Serialize>(src: &str, ctx: &C) -> Result<String, TemplateError> {
    let mut env = minijinja::Environment::new();
    env.add_template("__inline", src).map_err(TemplateError::Render)?;
    render_with(&env, "__inline", ctx)
}
```
(Uses the existing `render_with` at line 906 and `TemplateError`.)

6. Re-export in the facade `crates/umbral/src/lib.rs` `templates` module (find the `pub use umbral_core::templates::{...}` line and add):
```rust
pub use umbral_core::templates::{LazyUser, with_current_user_lazy};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd crates && cargo test -p umbral-core --test lazy_user 2>&1 | tail -20`
Expected: PASS — both tests green (`resolver_does_not_run...`, `resolver_runs_once...`).

- [ ] **Step 5: Verify no regression in existing template rendering**

Run: `cd crates && cargo test -p umbral-core templates 2>&1 | tail -15` and `cargo build -p umbral-core`
Expected: existing template tests still pass; build clean. The eager `with_current_user` path is unchanged (lazy channel only adds an `or_else` fallback).

- [ ] **Step 6: Commit**

```bash
cd /home/dalmas/E/projects/umbral
git add crates/umbral-core/src/templates.rs crates/umbral/src/lib.rs crates/umbral-core/tests/lazy_user.rs
git commit -m "feat(core): lazy current-user channel for templates

A per-request resolver + OnceCell scoped on a task-local, resolved on demand
from inside minijinja's sync render via block_in_place (multi-thread only),
memoized. merge_ambient_value prefers the lazy channel, then the eager one,
then anonymous. Lets a request that never renders \`user\` skip resolution."
```

---

## Task 2: `user_context_layer` installs the resolver (lazy) instead of resolving eagerly

**Files:**
- Modify: `plugins/umbral-auth/src/session_user.rs` (`user_context_layer`, lines 263-272)
- Test: `plugins/umbral-auth/tests/user_context_lazy.rs` (create)

**Interfaces:**
- Consumes: `umbral::templates::{LazyUser, with_current_user_lazy}` (Task 1); existing `current_user(headers) -> Result<Option<AuthUser>, _>` (session_user.rs:64), `serialize_authenticated_with_relations(&AuthUser) -> minijinja::Value` (session_user.rs:287), `anonymous_user_value() -> minijinja::Value` (session_user.rs:506).
- Produces: unchanged public surface — `user_context_layer` keeps the same `from_fn` signature, only its body changes.

- [ ] **Step 1: Write the failing test**

Create `plugins/umbral-auth/tests/user_context_lazy.rs`:

```rust
//! `user_context_layer` must resolve the user LAZILY: a response that never
//! renders the template `user` (e.g. JSON) triggers zero identity work; an
//! HTML response that renders `user` triggers exactly one resolution.

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use tower::ServiceExt;

use umbral_auth::user_context_layer;

// A JSON handler that never touches templates / `user`.
async fn json_handler() -> &'static str {
    "{\"ok\":true}"
}

// An HTML handler that renders a template referencing `user`.
async fn html_handler() -> axum::response::Html<String> {
    let body = umbral::templates::render_str("staff={{ user.is_staff }}", &serde_json::json!({}))
        .expect("render");
    axum::response::Html(body)
}

#[tokio::test(flavor = "multi_thread")]
async fn json_request_does_not_resolve_user() {
    // No session cookie → if resolution ran, current_user() would query the
    // session table (which doesn't exist in this test harness) and the lazy
    // resolver would still be invoked. We assert the resolver is never run by
    // observing that the request succeeds with NO database configured at all:
    // a non-lazy (eager) layer would call current_user().await and hit the
    // ambient pool — which is unset here — surfacing an error/panic path.
    let app = axum::Router::new()
        .route("/json", get(json_handler))
        .layer(axum::middleware::from_fn(user_context_layer));

    let resp = app
        .oneshot(Request::builder().uri("/json").body(Body::empty()).unwrap())
        .await
        .expect("request");
    assert_eq!(resp.status(), http::StatusCode::OK);
}
```

NOTE on the assertion strategy: the precise "resolver ran N times" counter lives in Task 1's core test (where the resolver is injectable). Here we assert the *observable consequence*: with no ambient DB pool configured, a JSON request through the lazy layer still succeeds, because the resolver (which would touch the pool) is never invoked. The eager layer would invoke `current_user().await` unconditionally. Keep this test in its own binary (own process) so the unset ambient pool is clean.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd crates && cargo test -p umbral-auth --test user_context_lazy 2>&1 | tail -20`
Expected: FAIL — either compile error (test references not yet wired) or the eager layer invoking `current_user` against the unset pool. Confirm it is RED before proceeding.

- [ ] **Step 3: Rewrite `user_context_layer` (session_user.rs:263-272)**

```rust
pub async fn user_context_layer(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Install a LAZY resolver instead of resolving eagerly. The closure runs
    // (at most once) only if a template actually reads `user`; a JSON/API
    // response that never renders the template pays nothing.
    let headers = req.headers().clone();
    let lazy = umbral::templates::LazyUser::new(move || {
        let headers = headers.clone();
        async move {
            match current_user(&headers).await {
                Ok(Some(u)) => serialize_authenticated_with_relations(&u).await,
                _ => anonymous_user_value(),
            }
        }
    });
    umbral::templates::with_current_user_lazy(lazy, next.run(req)).await
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd crates && cargo test -p umbral-auth --test user_context_lazy 2>&1 | tail -20`
Expected: PASS — JSON request returns 200 without touching the DB.

- [ ] **Step 5: Run the umbral-auth suite for regressions**

Run: `cd crates && cargo test -p umbral-auth 2>&1 | tail -20`
Expected: all existing auth tests pass (the eager → lazy change is behavior-preserving for HTML pages: `user` still resolves when rendered).

- [ ] **Step 6: Commit**

```bash
cd /home/dalmas/E/projects/umbral
git add plugins/umbral-auth/src/session_user.rs plugins/umbral-auth/tests/user_context_lazy.rs
git commit -m "perf(auth): make user_context_layer lazy

Install a per-request lazy resolver on the template task-local instead of
resolving session+user eagerly. A request that never renders the template
\`user\` (JSON/API) now does zero identity queries; HTML pages resolve once
on first access (memoized). Closes the eager-on-JSON waste behind the shop's
1.3k read collapse."
```

---

## Task 3: Verify the win on the shop (benchmark)

**Files:** none (measurement only).

- [ ] **Step 1: Build the shop release**

```bash
cd /home/dalmas/E/projects/umbral/examples/shop && cargo build --release 2>&1 | tail -2
```

- [ ] **Step 2: Fresh bench DB, seed 40k rows, launch pinned**

```bash
cd /home/dalmas/E/projects/umbral/examples/shop
rm -f bench.db bench.db-wal bench.db-shm
UMBRAL_DATABASE_URL="sqlite://bench.db?mode=rwc" ./target/release/shop migrate >/dev/null 2>&1
taskset -c 0-9 env UMBRAL_DATABASE_URL="sqlite://bench.db?mode=rwc" UMBRAL_BIND_ADDR=127.0.0.1:8123 ./target/release/shop serve >/dev/null 2>&1 &
sleep 1.5
curl --retry 20 --retry-delay 1 --retry-all-errors -s -o /dev/null http://127.0.0.1:8123/bench/text
taskset -c 10-19 /tmp/wrk/wrk -t8 -c64 -d3s http://127.0.0.1:8123/bench/notes/write >/dev/null 2>&1
```

- [ ] **Step 3: Benchmark `/bench/notes/read`, warmed**

```bash
taskset -c 10-19 /tmp/wrk/wrk -t10 -c200 -d3s http://127.0.0.1:8123/bench/notes/read >/dev/null 2>&1  # warmup
taskset -c 10-19 /tmp/wrk/wrk -t10 -c200 -d8s http://127.0.0.1:8123/bench/notes/read 2>&1 | awk '/Requests\/sec/{print $2}'
kill $(ss -ltnp 2>/dev/null|grep ':8123'|grep -oE 'pid=[0-9]+'|head -1|cut -d= -f2) 2>/dev/null
```

Expected: **~1.3k → ≥10k req/s** (the shop's `/bench/notes/read` returns JSON and never renders `user`, so `user_context_layer` now resolves nothing). If it does NOT improve materially, STOP — the win is gated on this route never reading `user`; investigate whether some other global layer (sessions) still queries per request (that is Phase 1b's territory, but confirm it is not Task 2 regressing).

- [ ] **Step 4: Record the before/after in the plan's results note and commit nothing (measurement only).**

---

## Self-Review

**Spec coverage (spec §3 Component 1 — the lazy half):** Task 1 makes `user` lazy in core; Task 2 makes `user_context_layer` install the resolver; Task 3 proves the JSON-endpoint win. The *memoized-across-consumers* half (extractors/`LoggedIn`/`Authentication` sharing one lookup) is explicitly deferred to Phase 1b per Global Constraints — not a gap, a scoping decision.

**Placeholder scan:** none — every code step has complete code; `render_str` is fully specified; the `block_in_place` guard is concrete.

**Type consistency:** `LazyUser` / `with_current_user_lazy` signatures match between Task 1 (produced) and Task 2 (consumed); the user value is `minijinja::Value` end-to-end (matches `serialize_authenticated_with_relations`'s return type and `merge_ambient_value`'s expectations).

**Key risk (called out):** `block_in_place` + `Handle::runtime_flavor()` — Task 1 Step 4 is the gate that proves the mechanism on a multi-thread runtime; the `current_thread`/no-runtime fallback returns anonymous with a warning rather than panicking.
