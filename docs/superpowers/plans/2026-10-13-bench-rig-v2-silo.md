# Bench Rig v2 — Fixed-Resource Silo Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the untrustworthy v1 benchmark methodology with a fixed-resource Docker silo driven by external k6, and produce the first baseline number for a full-stack umbral application that anyone can reproduce.

**Architecture:** `harness/backend` is restructured from a single binary into a library plus two bin targets. `bench-instrumented` keeps the existing telemetry (it becomes the diagnostic tool). `bench-prod` mounts the real application — real session auth, real security headers, real writes — and constructs no instrumentation at all, so a number can never be recorded with measurement scaffolding accidentally in the path. Query logic is shared between them in `src/domain.rs`; only the thin handler wrappers differ, because the two binaries resolve the acting user differently (a bench header vs. a real session). The application then runs inside a 4 vCPU / 4 GB container pinned to four distinct physical P-cores, with k6 outside it on separate cores.

**Tech Stack:** Rust (umbral framework, axum, sqlx/SQLite), Docker (cgroup v2, `--cpuset-cpus`), k6 v2.3.0 (already installed at `~/.local/bin/k6`), bash.

**Spec:** `docs/superpowers/specs/2026-10-13-bench-rig-fixed-resource-silo-design.md` (commit `5c9c9183`)

## Global Constraints

- **No `unsafe`.** No `unsafe` blocks, `unsafe fn`, new `unsafe impl`, or `#[allow(unsafe_code)`. An optimization requiring `unsafe` is out of scope, not deferred. (Spec locked decision #8.) Note: `src/main.rs:56-58` currently contains an `unsafe { std::env::set_var }` — the instrumented binary keeps it because it predates this work and runs before the runtime starts, but no new `unsafe` may be added anywhere.
- **Phases 1 and 2 must not modify `crates/` or `plugins/`.** A measurement that changed the thing it measures is worthless. If a task appears to need a framework change, the rig is wrong — stop and report.
- **k6 version:** v2.3.0 at `~/.local/bin/k6`. Run via `harness/k6/run.sh` which resolves PATH → `~/.local/bin` → docker.
- **Silo envelope (verbatim):** `--cpuset-cpus=0,2,4,6` `--cpus=4` `--memory=4g` `--memory-swap=4g` `--network host`.
- **k6 core pin:** `taskset -c 8,9,10,11`.
- **Ports:** instrumented = 8113, prod = 8114. Both verified free.
- **Session cookie name:** `umbral_session` (`plugins/umbral-sessions/src/lib.rs:150`).
- **Login endpoint:** `POST {prefix}/login`, body `{"username": ..., "password": ...}` (JSON accepted via `JsonOrForm<LoginIn>`), returns `{user, token}` and a `Set-Cookie` (`plugins/umbral-auth/src/auth_routes.rs:426,821`).
- **Thresholds (both k6 suites):** `http_req_duration: p(95)<500, p(99)<1000`; `http_req_failed: rate<0.01`. Pass/fail is the k6 exit code.
- **Commit cadence:** one logical change per commit. Run `cargo fmt` in `harness/backend` before each commit. Do **not** run the root-workspace `cargo test`/`cargo build` for harness-only changes — the harness is a standalone project excluded from the framework workspace.

## Review Focus

The spec implies conditions no task's tests fully exercise. Each is listed here with the behaviour a reasonable person would expect, and each has a test attached to the task that owns the code.

1. **k6 exhausts host file descriptors at high VU counts, and it looks like a server failure.** The challenge expects ~15,000 concurrent connections. A client-side `EMFILE` produces failed requests, which k6 reports as `http_req_failed` — indistinguishable in the summary from the server rejecting load. Expected: the runner checks and reports `ulimit -n` before a run, and a result records it. → Task 7.
2. **The container is OOM-killed and it looks like a latency failure.** `--memory=4g --memory-swap=4g` means no swap, so exceeding 4 GB kills the process rather than degrading. A dead server mid-hold and a slow server both produce failed requests. Expected: the runner distinguishes them and the result records which happened. → Task 6.
3. **A SQLite WAL checkpoint stall shows up as an umbral p99 spike.** A long read under a write burst can trigger a checkpoint that blocks readers. The user sees "umbral got slower"; the truth is the storage layer. Expected: it is recorded, not averaged away. → Task 8.
4. **Session cookies expire mid-hold.** A 5-minute hold plus a 2–3 minute cooldown plus a limit search can span 90 minutes, so a short-lived pool would start failing partway through and the failure would be attributed to load. Verified as **not a risk at the default**: the login cookie carries `Max-Age=1209600` — 14 days (`DEFAULT_TTL_SECONDS`, `plugins/umbral-sessions/src/lib.rs:153`), and there is no builder to shorten the login TTL. It becomes a risk only if someone adds `.max_session_age(secs)` to `bench-prod`, which caps *server-side* session age independently of the cookie. Expected: the result records the session max-age in force, so a future change to it shows up in the data rather than being inferred from a mid-run failure. → Task 5.
5. **A wrong `created_at` format silently breaks feed ordering without erroring.** The feed orders by `created_at DESC`; SQLite stores it as TEXT. A seed that writes a format which does not sort lexicographically returns a plausible-looking but wrong feed, and no test of status codes would catch it. Expected: the seed's timestamp format is asserted to sort correctly. → Task 5.

---

## File Structure

| Path | Responsibility |
|---|---|
| `harness/backend/src/lib.rs` | **Create.** Crate root exposing all shared modules. |
| `harness/backend/src/bin/bench-instrumented.rs` | **Create** (from `src/main.rs`). The diagnostic app: telemetry, probes, lab, `DevAuth`, port 8113. |
| `harness/backend/src/bin/bench-prod.rs` | **Create.** The measured app: real plugins, no instrumentation, port 8114. |
| `harness/backend/src/domain.rs` | **Create.** Query + serialization logic shared by both binaries. No tracing, no auth resolution. |
| `harness/backend/src/prod_handlers.rs` | **Create.** Thin handlers for `bench-prod`; resolve the user from the real session. |
| `harness/backend/src/api.rs` | **Modify.** Thin handlers for `bench-instrumented`; keep every `trace.*` call, delegate work to `domain`. |
| `harness/backend/src/seed.rs` | **Modify.** Add password setting + a timestamp-format assertion. |
| `harness/backend/Dockerfile` | **Create.** Host-built binary into a slim runtime. |
| `harness/bench/run.sh` | **Create.** Silo launcher. |
| `harness/bench/seed-sessions.sh` | **Create.** Build `session_pool.json`. |
| `harness/bench/search.sh` | **Create.** Warm-up → limit search → confirm. |
| `harness/bench/workload.md` | **Create.** The mix and why. |
| `harness/bench/k6/warmup.js` | **Create.** Phase-1 warm-up. |
| `harness/bench/k6/hold.js` | **Create.** The 5-minute hold. |
| `harness/bench/k6/saturation.js` | **Create.** No-think-time RPS ceiling. |
| `harness/bench/results/` | **Create.** One JSON per run + `compare.sh`. |
| `harness/bench/golden/capture.sh` | **Create.** Characterization net for the refactor. |

---

## Phase 1 — The first trustworthy baseline

### Task 1: Characterization net for the instrumented app

No automated tests exist for `harness/backend`, and Tasks 2 and 3 refactor every handler. Without a net, a silent response-shape regression would pass unnoticed. Capture golden responses first.

**Files:**
- Create: `harness/bench/golden/capture.sh`
- Create: `harness/bench/golden/README.md`
- Modify: `harness/.gitignore`

**Interfaces:**
- Consumes: nothing.
- Produces: `harness/bench/golden/<route>.json` files, each `{"status":N,"timing_keys":[...],"body_sha256":"..."}`; and `capture.sh <port>` used by Tasks 2 and 3 to assert equality.

- [ ] **Step 1: Write the capture script**

Create `harness/bench/golden/capture.sh`:

```bash
#!/usr/bin/env bash
# Capture golden responses from a running harness binary.
#
# Usage: capture.sh <port> <outdir>
#
# Records, per route: HTTP status, the Server-Timing metric NAMES (not the
# durations — those are nondeterministic), and a sha256 of the response body.
# Volatile fields (ids, timestamps) are normalised out before hashing so the
# capture is stable across runs.
set -euo pipefail

PORT="${1:?usage: capture.sh <port> <outdir>}"
OUT="${2:?usage: capture.sh <port> <outdir>}"
BASE="http://127.0.0.1:${PORT}"
mkdir -p "$OUT"

# Route + the x-bench-user id it needs (empty = none).
ROUTES=(
  "/api/ping|"
  "/api/feed|"
  "/api/feed?limit=5|"
  "/api/feed-naive?limit=5|"
  "/api/users/1/profile|"
  "/|"
  "/u/1|"
  "/posts/1|"
)

for entry in "${ROUTES[@]}"; do
  route="${entry%%|*}"
  user="${entry##*|}"
  name="$(echo "$route" | tr '/?=&' '____' | sed 's/^_*//')"
  [ -z "$name" ] && name="root"

  hdr=$(mktemp); body=$(mktemp)
  if [ -n "$user" ]; then
    code=$(curl -s -o "$body" -D "$hdr" -w '%{http_code}' \
      -H "x-bench-user: $user" "${BASE}${route}")
  else
    code=$(curl -s -o "$body" -D "$hdr" -w '%{http_code}' "${BASE}${route}")
  fi

  # Server-Timing metric NAMES only, sorted — durations vary run to run.
  timing=$(grep -i '^server-timing:' "$hdr" \
    | sed 's/^[Ss]erver-[Tt]iming: *//' \
    | tr ',' '\n' | sed 's/;.*//' | tr -d ' \r' | sort | paste -sd, -)

  # Normalise volatile values before hashing.
  norm=$(sed -E \
    -e 's/"id":[0-9]+/"id":0/g' \
    -e 's/"created_at":"[^"]*"/"created_at":"X"/g' \
    -e 's/[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z/X/g' \
    "$body")

  sha=$(printf '%s' "$norm" | sha256sum | cut -d' ' -f1)
  printf '{"status":%s,"timing_keys":"%s","body_sha256":"%s"}\n' \
    "$code" "$timing" "$sha" > "$OUT/${name}.json"
  echo "  ${route} -> status=${code} sha=${sha:0:12}"
  rm -f "$hdr" "$body"
done
echo "captured to $OUT"
```

- [ ] **Step 2: Make it executable and document it**

```bash
chmod +x harness/bench/golden/capture.sh
```

Create `harness/bench/golden/README.md` explaining: these are characterization snapshots taken before the lib/bin restructure (Task 2) and the domain extraction (Task 3). They pin HTTP status, the set of `Server-Timing` metric names, and a normalised body hash. They are not a spec of correct behaviour — they are a record of behaviour as it was, so a refactor that changes a response shape fails loudly instead of silently.

- [ ] **Step 3: Ignore generated goldens but keep the script**

Add to `harness/.gitignore`:

```
# Generated golden snapshots (recreated by golden/capture.sh) and bench outputs.
bench/golden/*.json
bench/results/*.json
bench/session_pool.json
bench/bench-run.db*
```

- [ ] **Step 4: Boot the instrumented app and capture**

Build and boot with a small seed so the capture is fast:

```bash
cd harness/backend
cargo build --release 2>&1 | tail -5
UMBRAL_BENCH_USERS=50 UMBRAL_BENCH_POSTS_PER_USER=3 UMBRAL_BENCH_LIKES_PER_USER=2 \
  ./target/release/umbral-bench serve &
sleep 20
harness/bench/golden/capture.sh 8113 harness/bench/golden/before
```

Expected: 8 lines, one per route, each with `status=200`. If `/posts/1` 404s, the seed produced no post — re-run with a larger seed before continuing.

- [ ] **Step 5: Verify the capture is stable**

Run the capture twice more against the same process and confirm the hashes match:

```bash
harness/bench/golden/capture.sh 8113 /tmp/golden-check-1
harness/bench/golden/capture.sh 8113 /tmp/golden-check-2
diff -r /tmp/golden-check-1 /tmp/golden-check-2 && echo "STABLE"
```

Expected: `STABLE`. A hash that differs between two captures of the same running process means a volatile field escaped normalisation — fix the `sed` rules in Step 1 before continuing, or Task 2's gate is meaningless.

- [ ] **Step 6: Commit**

```bash
git add harness/bench/golden harness/.gitignore
git commit -m "test(bench): characterization net for the harness refactor"
```

---

### Task 2: Restructure to a library plus two bin targets

`src/main.rs` is currently the crate root. Two binaries cannot share modules in that layout. Convert to `src/lib.rs` + `src/bin/`, changing no behaviour.

**Files:**
- Create: `harness/backend/src/lib.rs`
- Create: `harness/backend/src/bin/bench-instrumented.rs` (content moved from `src/main.rs`)
- Create: `harness/backend/src/bin/bench-prod.rs` (stub)
- Delete: `harness/backend/src/main.rs`
- Modify: `harness/backend/Cargo.toml`

**Interfaces:**
- Consumes: Task 1's golden captures.
- Produces: crate `umbral_bench` exposing `pub mod api; pub mod domain; pub mod lab; pub mod loadgen; pub mod models; pub mod probes; pub mod prod_handlers; pub mod seed; pub mod telemetry; pub mod views;`. Binaries are `umbral-bench-instrumented` and `umbral-bench-prod`.

- [ ] **Step 1: Create the library root**

Create `harness/backend/src/lib.rs`:

```rust
//! umbral-bench — the benchmark harness, as a library.
//!
//! Two binaries consume this crate:
//!   - `bench-instrumented` — the DIAGNOSTIC tool. Telemetry, probe plugins,
//!     the framework lab, and `DevAuth`. Answers "where did the time go".
//!   - `bench-prod` — the MEASURED application. Real session auth, real
//!     security, no instrumentation constructed at all. Answers "how fast is
//!     it". No number from the instrumented binary is ever quoted as a
//!     performance result.
//!
//! Query logic is shared in [`domain`]; the binaries differ only in how they
//! resolve the acting user and whether they record telemetry.

pub mod api;
pub mod domain;
pub mod lab;
pub mod loadgen;
pub mod models;
pub mod probes;
pub mod prod_handlers;
pub mod seed;
pub mod telemetry;
pub mod views;
```

- [ ] **Step 2: Create `src/domain.rs` as a placeholder re-export**

It is filled in Task 3; this step only makes the crate compile.

```rust
//! Shared query + serialization logic.
//!
//! Everything here is pure application behaviour: no tracing, no auth
//! resolution, no request extractors. The binaries' handlers own those, which
//! is what lets `bench-prod` measure the same queries `bench-instrumented`
//! decomposes.

// Populated in Task 3.
```

- [ ] **Step 3: Create `src/prod_handlers.rs` as a placeholder**

```rust
//! Thin handlers for `bench-prod`. Populated in Task 4.
```

- [ ] **Step 4: Move `main.rs` to `src/bin/bench-instrumented.rs`**

```bash
mkdir -p harness/backend/src/bin
git mv harness/backend/src/main.rs harness/backend/src/bin/bench-instrumented.rs
```

(`git mv` requires the destination directory to exist.)

Then edit `src/bin/bench-instrumented.rs`: **delete** its `mod` declarations (the library now owns them) and replace each `crate::` path with `umbral_bench::`. Specifically delete lines 28–35 (`mod api; mod lab; ... mod views;`) and change the `use crate::{...}` block at lines 44–48 to `use umbral_bench::{...}`.

- [ ] **Step 5: Create the prod stub**

Create `harness/backend/src/bin/bench-prod.rs`:

```rust
//! bench-prod — the MEASURED application. Populated in Task 4.

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    eprintln!("bench-prod: not implemented yet (see Task 4)");
    Ok(())
}
```

- [ ] **Step 6: Declare both binaries in Cargo.toml**

Add to `harness/backend/Cargo.toml` after `[package]`:

```toml
[lib]
name = "umbral_bench"
path = "src/lib.rs"

[[bin]]
name = "umbral-bench-instrumented"
path = "src/bin/bench-instrumented.rs"

[[bin]]
name = "umbral-bench-prod"
path = "src/bin/bench-prod.rs"
```

Note the binary names now carry the `-instrumented` suffix, so the `cargo run -- serve` commands in the README must become `cargo run --bin umbral-bench-instrumented -- serve`. Update `harness/backend/README.md`'s Quick start block accordingly in this same commit.

- [ ] **Step 7: Build and verify nothing changed**

```bash
cd harness/backend
cargo build --release 2>&1 | tail -20
```

Expected: builds clean. Fix any unresolved `crate::` paths the same way as Step 4.

- [ ] **Step 8: Verify against the golden captures**

```bash
UMBRAL_BENCH_USERS=50 UMBRAL_BENCH_POSTS_PER_USER=3 UMBRAL_BENCH_LIKES_PER_USER=2 \
  ./target/release/umbral-bench-instrumented serve &
sleep 20
../bench/golden/capture.sh 8113 ../bench/golden/after-task2
diff -r ../bench/golden/before ../bench/golden/after-task2 && echo "NO BEHAVIOUR CHANGE"
```

Run from `harness/backend`, and **with the same seed env Task 1 captured against**, against the same `bench.db`. A different dataset changes the response bodies, and the gate then fails for a reason that has nothing to do with your refactor. (Both facts are recorded in `harness/bench/golden/README.md`.)

Expected: `NO BEHAVIOUR CHANGE`. Any diff is an unintended change — fix it, do not update the golden.

- [ ] **Step 9: Commit**

```bash
git add harness/backend/Cargo.toml harness/backend/src harness/backend/README.md
git commit -m "refactor(bench): split harness into a library and two bin targets"
```

---

### Task 3: Extract shared query logic into `src/domain.rs`

The handlers currently do two jobs: resolve the acting user and run queries. `bench-prod` must run the *same* queries under real auth, so the query half moves to `domain`.

**Files:**
- Modify: `harness/backend/src/domain.rs`
- Modify: `harness/backend/src/api.rs:39-351`
- Test: `harness/backend/src/domain.rs` (inline `#[cfg(test)]` for the pure helpers)

**Interfaces:**
- Consumes: `crate::models::{Comment, Follow, Like, Post, comment, follow, like, post}`.
- Produces, all `pub` in `umbral_bench::domain`:
  - `pub async fn recent_posts(limit: u64) -> Result<Vec<Post>, ApiError>`
  - `pub async fn engage_counts(ids: &[i64]) -> Result<(HashMap<i64, usize>, HashMap<i64, usize>), ApiError>` — returns `(like_counts, comment_counts)`
  - `pub fn feed_payload(posts: &[Post], likes: &HashMap<i64, usize>, comments: &HashMap<i64, usize>) -> Value`
  - `pub async fn naive_feed_payload(limit: u64) -> Result<Value, ApiError>`
  - `pub async fn profile_payload(user_id: i64) -> Result<Value, ApiError>`
  - `pub async fn create_post(user_id: i64, body: &str) -> Result<Value, ApiError>`
  - `pub fn clamp_body(raw: &str) -> String` — the 280-char truncation
  - `pub async fn like_post(user_id: i64, post_id: i64) -> Result<Value, ApiError>`
  - `pub async fn follow_user(user_id: i64, target_id: i64) -> Result<Value, ApiError>`
  - `pub fn author_name(user: &AuthUser) -> String` and `pub fn hydrated_author_name(p: &Post) -> String`

- [ ] **Step 1: Write failing tests for the pure helpers**

Append to `harness/backend/src/domain.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_body_truncates_to_280_chars() {
        let long = "a".repeat(300);
        assert_eq!(clamp_body(&long).chars().count(), 280);
    }

    #[test]
    fn clamp_body_truncates_on_char_boundaries_not_bytes() {
        // 300 multi-byte chars: a byte-slice would panic or split a codepoint.
        let long = "é".repeat(300);
        let out = clamp_body(&long);
        assert_eq!(out.chars().count(), 280);
        assert_eq!(out, "é".repeat(280)); // valid UTF-8 preserved
    }

    #[test]
    fn clamp_body_leaves_short_bodies_untouched() {
        assert_eq!(clamp_body("hello"), "hello");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd harness/backend && cargo test --lib domain::tests 2>&1 | tail -15
```

Expected: compile error, `cannot find function clamp_body`.

- [ ] **Step 3: Implement the helpers and move the query bodies**

In `harness/backend/src/domain.rs`, add the imports and the pure helpers first:

```rust
use std::collections::HashMap;

use serde_json::{Value, json};
use umbral::prelude::*;
use umbral::web::ApiError;
use umbral_auth::AuthUser;

use crate::models::{Comment, Like, Post, comment, like, post};

/// Truncate a post body to the app's 280-char limit, on a character boundary.
pub fn clamp_body(raw: &str) -> String {
    raw.chars().take(280).collect()
}

pub fn author_name(user: &AuthUser) -> String {
    user.username.clone()
}

/// Author name for a post fetched WITH `select_related("author")`.
pub fn hydrated_author_name(p: &Post) -> String {
    p.author
        .__resolved()
        .map(author_name)
        .unwrap_or_else(|| format!("user_{}", p.author.id()))
}

fn counts_by_fk<T: IntoIterator<Item = i64>>(ids: T) -> HashMap<i64, usize> {
    let mut m: HashMap<i64, usize> = HashMap::new();
    for id in ids {
        *m.entry(id).or_default() += 1;
    }
    m
}
```

Then move the query bodies verbatim from `api.rs`, replacing `trace.add_db_wall(...)` with nothing and keeping the SQL identical. `recent_posts` is `api.rs:76-81`; `engage_counts` is `api.rs:86-100`; `feed_payload` is `api.rs:102-118`; `create_post` is `api.rs:281-288`; `like_post` is `api.rs:303-316`; `follow_user` is `api.rs:334-347`. `profile_payload` and `naive_feed_payload` move from `api.rs:126-265` unchanged.

- [ ] **Step 4: Run the helper tests to verify they pass**

```bash
cargo test --lib domain::tests 2>&1 | tail -15
```

Expected: `test result: ok. 3 passed`.

- [ ] **Step 5: Rewrite `api.rs` handlers as thin wrappers**

Replace `api.rs`'s handler bodies so all tracing stays in `api.rs` and all work goes to `domain`. `feed` becomes:

```rust
pub async fn feed(
    Query(q): Query<FeedQuery>,
    Extension(trace): Extension<Trace>,
) -> Result<Json<Value>, ApiError> {
    trace.handler_start();
    let limit = q.limit.unwrap_or(25).clamp(1, 100) as u64;

    let t = Instant::now();
    let posts = domain::recent_posts(limit).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();

    let t = Instant::now();
    let (like_counts, comment_counts) = domain::engage_counts(&ids).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let t = Instant::now();
    let payload = domain::feed_payload(&posts, &like_counts, &comment_counts);
    trace.add_serialize(t.elapsed().as_micros() as u64);
    trace.handler_end();

    Ok(Json(payload))
}
```

Apply the same shape to `feed_naive`, `profile`, `create_post`, `like_post`, `follow_user`. The write handlers keep `Extension(user): Extension<Option<BenchUser>>` and the `ApiError::unauthorized("x-bench-user header required")` check — that is the instrumented binary's user resolution and must not change. Delete the now-unused `name_of`, `hydrated_author_name`, and `counts_by_fk` from `api.rs` (they moved to `domain`); keep the module-level imports the thin wrappers still need.

- [ ] **Step 6: Build and run the full harness test suite**

```bash
cargo build --release 2>&1 | tail -20
cargo test --lib 2>&1 | tail -10
```

Expected: clean build, all tests pass. Warnings about unused imports mean a leftover from the extraction — remove it rather than `#[allow]`-ing it.

- [ ] **Step 7: Verify against the golden captures**

```bash
UMBRAL_BENCH_USERS=50 UMBRAL_BENCH_POSTS_PER_USER=3 UMBRAL_BENCH_LIKES_PER_USER=2 \
  ./target/release/umbral-bench-instrumented serve &
sleep 20
../bench/golden/capture.sh 8113 ../bench/golden/after-task3
diff -r ../bench/golden/before ../bench/golden/after-task3 && echo "NO BEHAVIOUR CHANGE"
```

Run from `harness/backend`, with the same seed env as Task 1 and Task 2, against the same `bench.db` — see `harness/bench/golden/README.md`.

Expected: `NO BEHAVIOUR CHANGE`. This is the gate that proves the extraction preserved both the response bodies and the `Server-Timing` metric names the diagnostic tool depends on.

- [ ] **Step 8: Commit**

```bash
git add harness/backend/src/domain.rs harness/backend/src/api.rs
git commit -m "refactor(bench): extract shared query logic into domain"
```

---

### Task 4: The `bench-prod` binary

**Files:**
- Modify: `harness/backend/src/bin/bench-prod.rs`
- Modify: `harness/backend/src/prod_handlers.rs`
- Test: manual, via the session cookie obtained in Task 5

**Interfaces:**
- Consumes: every `pub fn` in `umbral_bench::domain` (Task 3).
- Produces: a server on `127.0.0.1:8114` (or `UMBRAL_BIND`) exposing `/api/ping`, `/api/feed`, `/api/feed-naive`, `/api/users/{id}/profile`, `POST /api/posts`, `POST /api/posts/{id}/like`, `POST /api/users/{id}/follow`, plus `/` `/u/{id}` `/posts/{id}` and the auth routes.

- [ ] **Step 1: Write the prod binary**

Create `harness/backend/src/bin/bench-prod.rs`:

```rust
//! bench-prod — the MEASURED application.
//!
//! Mounts exactly what a production deployment of this app would: real
//! session auth, real CSRF protection, real security headers, the real
//! `DbStore` session backend, and the same query logic the instrumented
//! binary decomposes.
//!
//! Deliberately ABSENT: `TraceRoot`, `DevAuth`, `RequestLog`, `ResponseStamp`,
//! every `ProbePlugin`, `BenchLabMiddleware`, `RateLimitMiddleware`, and the
//! `UMBRAL_DB_LOG_STATEMENTS` override. Because none of them are constructed
//! here, a number from this binary cannot have measurement scaffolding in its
//! path.
//!
//! Boot:
//! ```bash
//! cargo run --release --bin umbral-bench-prod -- migrate
//! cargo run --release --bin umbral-bench-prod -- serve
//! ```

use umbral::cors::CorsConfig;
use umbral::prelude::*;
use umbral_auth::AuthPlugin;
use umbral_health::HealthPlugin;
use umbral_security::SecurityPlugin;
use umbral_sessions::SessionsPlugin;

use umbral_bench::{prod_handlers, seed};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut settings = Settings::from_env()?;
    // The prod bench owns 8114 so it can never disturb a developer's dev
    // server on the instrumented binary's 8113.
    if std::env::var("UMBRAL_BIND").is_err() {
        settings.bind_addr = "127.0.0.1:8114".parse()?;
    }

    let pool = umbral::db::connect(&settings.database_url).await?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        // Real plugins only. `SessionsPlugin::default()` is `DbStore` — a real
        // session read per authenticated request, which is the honest cost.
        .plugin(SessionsPlugin::default())
        .plugin(SecurityPlugin::new().csrf_exempt(["/api"]))
        .plugin(AuthPlugin::new().with_default_routes())
        .plugin(HealthPlugin::default())
        .cors_for(
            "/api",
            CorsConfig::strict()
                .allow_origins(vec!["http://localhost:5173"])
                .allow_credentials(false),
        )
        .templates_dir("templates")
        .routes(
            Routes::new()
                .get("/", prod_handlers::feed_page)
                .get("/u/{id}", prod_handlers::profile_page)
                .get("/posts/{id}", prod_handlers::post_page)
                .get("/api/ping", prod_handlers::ping)
                .get("/api/feed", prod_handlers::feed)
                .get("/api/feed-naive", prod_handlers::feed_naive)
                .get("/api/users/{id}/profile", prod_handlers::profile)
                .post("/api/posts", prod_handlers::create_post)
                .post("/api/posts/{id}/like", prod_handlers::like_post)
                .post("/api/users/{id}/follow", prod_handlers::follow_user),
        )
        .auto_migrate_on_serve()
        .seed_on_serve(seed::all)
        .build_deferred()?;

    umbral_cli::dispatch(app).await
}
```

**One thing you must verify rather than assume:** `Settings` must expose a mutable `bind_addr` field for the override above to compile. Run `grep -n 'pub bind_addr' crates/umbral-core/src/settings.rs`. If it is not directly mutable, set the bind through whatever env var `Settings::from_env` reads instead.

The `views.rs` coupling this step originally asked you to check is **already resolved — do not treat it as a blocker and do not re-open it.** `views.rs`'s three page handlers do take `Extension<Trace>`, and Step 2b is the answer: `bench-prod` gets its own page handlers over `domain`'s page functions. A `Trace` reference in `views.rs` is expected and correct; it is why `bench-prod` cannot mount `views::*` directly.

- [ ] **Step 2: Write the prod handlers**

Create `harness/backend/src/prod_handlers.rs`. The only structural difference from the instrumented handlers is how the acting user is resolved:

```rust
//! Thin handlers for `bench-prod`.
//!
//! Identical work to `crate::api`'s handlers (both delegate to
//! `crate::domain`), with two differences: no `trace.*` calls, and the acting
//! user comes from the REAL session rather than an `x-bench-user` header.

use serde::Deserialize;
use serde_json::Value;

use umbral::prelude::*;
use umbral::web::{ApiError, Json, Path, Query};
use umbral_auth::OptionalIdentity;

use umbral_bench::domain;

#[derive(Debug, Deserialize)]
pub struct FeedQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePost {
    pub body: String,
}

pub async fn ping() -> Json<Value> {
    Json(serde_json::json!({ "ok": true }))
}

pub async fn feed(Query(q): Query<FeedQuery>) -> Result<Json<Value>, ApiError> {
    let limit = q.limit.unwrap_or(25).clamp(1, 100) as u64;
    let posts = domain::recent_posts(limit).await?;
    let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();
    let (likes, comments) = domain::engage_counts(&ids).await?;
    Ok(Json(domain::feed_payload(&posts, &likes, &comments)))
}

pub async fn feed_naive(Query(q): Query<FeedQuery>) -> Result<Json<Value>, ApiError> {
    let limit = q.limit.unwrap_or(25).clamp(1, 100) as u64;
    Ok(Json(domain::naive_feed_payload(limit).await?))
}

pub async fn profile(Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    Ok(Json(domain::profile_payload(id).await?))
}

pub async fn create_post(
    user: OptionalIdentity,
    Json(input): Json<CreatePost>,
) -> Result<Json<Value>, ApiError> {
    let u = user.ok_or_else(|| ApiError::unauthorized("authentication required"))?;
    let body = domain::clamp_body(&input.body);
    Ok(Json(domain::create_post(u.id(), &body).await?))
}

pub async fn like_post(
    user: OptionalIdentity,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let u = user.ok_or_else(|| ApiError::unauthorized("authentication required"))?;
    Ok(Json(domain::like_post(u.id(), id).await?))
}

pub async fn follow_user(
    user: OptionalIdentity,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let u = user.ok_or_else(|| ApiError::unauthorized("authentication required"))?;
    if u.id() == id {
        return Err(ApiError::bad_request("cannot follow yourself"));
    }
    Ok(Json(domain::follow_user(u.id(), id).await?))
}
```

**The `domain` surface changed during Task 3's fix round — use these names, not the ones earlier text implies.** The read paths were re-cut at a query/assemble seam so each half gets its own measurement window in the instrumented binary. What exists now:

| Old shape | Current shape |
|---|---|
| `recent_posts(limit)` + `engage_counts(&ids)` + `feed_payload(...)` | `recent_posts(limit)` + `engage_rows(&ids)` + `feed_payload(&posts, &rows, kind)` |
| `naive_feed_payload(limit)` | `naive_feed_query(limit)` + `naive_feed_payload(...)` |
| `profile_payload(user_id)` | `profile_query(user_id)` + `profile_payload(...)` |
| inline `q.limit.unwrap_or(25).clamp(1, 100) as u64` | `feed_limit(&q.limit)` |

Read `harness/backend/src/domain.rs` for the exact current signatures rather than trusting this table — it is a map, not the territory, and it was written from the fix report.

`feed_limit` matters for parity: it is the one place the default-and-clamp lives, so both binaries agree on `/api/feed?limit=0` and `?limit=999`. Call it; do not re-inline the expression.

**The self-follow guard is not in `domain` — reproduce it here.** `domain::follow_user(user_id, target_id)` performs no `user_id == target_id` check; the guard deliberately lives in the handler (before the timer starts) to preserve stamp ordering. If `bench-prod` omits it, it returns `200 {"created":true}` where `bench-instrumented` returns `400`. Copy the guard exactly as the instrumented handler has it.

- [ ] **Step 2b: Add the prod page handlers**

The instrumented binary's page handlers in `src/views.rs` take `Extension<Trace>` and therefore cannot be mounted here — a missing axum extension is a runtime 500 on every HTML route, not a compile error. Task 3 therefore split each page's data-gathering into `domain`; `views.rs` keeps only rendering. `bench-prod` gets its own thin page handlers.

For each of `feed_page`, `profile_page` and `post_page`, write a `prod_handlers` function that calls the corresponding `domain::*_page_query` / `domain::*_page_payload` pair and renders the same template with the same context as `views.rs` does. **Open `src/views.rs` and mirror its context construction exactly** — the template expects the same keys, and a missing key renders as an empty section rather than an error, so a mismatch would be silent. The golden gate covers `/`, `/u/1` and `/posts/1`, so a body difference fails loudly once Task 4's verification runs — but the gate compares the *instrumented* binary against its own past, not prod against instrumented. **Before committing, diff prod's rendered output against the instrumented binary's for all three pages** and confirm they match.

**The extractor is `OptionalIdentity`** — verified, not guessed. `plugins/umbral-auth/src/auth_routes.rs:911-939` uses it on `/api/auth/me` to resolve an identity session-first and then from a bearer token, which is exactly what this app needs: a browser sends `umbral_session`, and an API client could send `Authorization: Bearer`. It filters on `is_active = true`.

Confirm its accessor before wiring, since the exact method name is not asserted here:

```bash
grep -rn 'impl OptionalIdentity\|fn id(\|fn user_id(\|fn identity(' \
  plugins/umbral-auth/src/*.rs | head -20
```

Use whatever it actually exposes. If the identity does not give the user id as an `i64`, change `domain::create_post`'s parameter type to match rather than casting at the call site. **Do not hand-roll a session lookup in the handler** — a benchmark that measures a session read the framework does not actually provide is worse than no benchmark at all.

Two further verified facts this task depends on: the login cookie is valid for **14 days** (`DEFAULT_TTL_SECONDS`, `plugins/umbral-sessions/src/lib.rs:153`), which comfortably outlives a 90-minute search; and `SecurityPlugin`'s CSRF middleware requires a `umbral_csrf_token` cookie plus an `X-CSRF-Token` header on unsafe methods **unless the path is exempt**. `bench-prod` exempts `["/api"]`, so the `/api/*` writes need no CSRF header — but if the exemption is ever narrowed, k6's writes will start returning `403` and it will look like a load failure rather than a configuration mistake.

- [ ] **Step 3: Confirm the write handlers' `id()` accessor**

`domain::create_post` takes a `user_id: i64`, so the extractor's identity must expose the user id as an `i64`. Check what the real accessor is called:

```bash
grep -rn 'fn id(' plugins/umbral-auth/src/*.rs | head
```

Adjust `u.id()` if the accessor has a different name or returns a different type, and adjust `domain::create_post`'s parameter type to match rather than casting at the call site.

- [ ] **Step 4: Build both binaries**

```bash
cd harness/backend
cargo build --release --bins 2>&1 | tail -20
```

Expected: both `umbral-bench-instrumented` and `umbral-bench-prod` build.

- [ ] **Step 5: Verify prod boots and answers unauthenticated reads**

```bash
UMBRAL_BENCH_USERS=50 UMBRAL_BENCH_POSTS_PER_USER=3 ./target/release/umbral-bench-prod migrate
UMBRAL_BENCH_USERS=50 UMBRAL_BENCH_POSTS_PER_USER=3 ./target/release/umbral-bench-prod serve &
sleep 20
curl -s localhost:8114/api/ping
curl -s 'localhost:8114/api/feed?limit=3' | head -c 300
echo
echo "--- an unauthenticated write must be 401, not 500 ---"
curl -s -o /dev/null -w '%{http_code}\n' -X POST localhost:8114/api/posts \
  -H 'content-type: application/json' -d '{"body":"nope"}'
echo "--- no Server-Timing header may be present in prod ---"
curl -si localhost:8114/api/ping | grep -ci 'server-timing' || echo "0 (correct)"
```

Expected: ping returns JSON; feed returns posts; the write returns `401`; `Server-Timing` is absent (grep finds 0). If `Server-Timing` is present, instrumentation leaked into the prod binary — that is the exact failure this design exists to prevent.

- [ ] **Step 6: Verify the container-visible absence of bench routes**

```bash
for p in /api/bench/telemetry /api/bench/runs /api/bench/lab; do
  printf '%-28s %s\n' "$p" "$(curl -s -o /dev/null -w '%{http_code}' localhost:8114$p)"
done
```

Expected: `404` for all three. Any `200` means a bench control surface was mounted into the measured app.

- [ ] **Step 7: Clear Task 2's disclosed leftovers**

Task 2 renamed the binary but deliberately deferred three loose ends. They belong here, because this task owns the new binary and the widening gap between the two.

**7a. Two runtime-rendered strings still name the old command.** `harness/backend/src/loadgen.rs:435` (the run-start error message) and `harness/backend/templates/feed.html:18` (the empty-feed page) both say `cargo run -- serve`, which now fails with cargo's "could not determine which binary to run". Update them to name `umbral-bench-instrumented` explicitly.

Both branches are unreachable against a seeded database, so the golden gate does not pin them and this edit cannot cause a false `after` failure — but do not regenerate `golden/before`.

**7b. Delete the stale pre-rename binary.**

```bash
rm -f harness/backend/target/release/umbral-bench
```

Goodbye to ~44 MB of pre-refactor code that cargo will never clean and that is now one tab-completion away from being run by accident. This is a build artifact, not a database — the "never delete" constraint does not apply to it.

**7c. Do NOT delete `harness/backend/target/release/umbral-bench-instrumented`** — it is the binary the golden gate runs against.

- [ ] **Step 8: Commit**

```bash
git add harness/backend/src/bin/bench-prod.rs harness/backend/src/prod_handlers.rs \
        harness/backend/src/loadgen.rs harness/backend/templates/feed.html
git commit -m "feat(bench): add bench-prod, the measured application"
```

---

### Task 5: Real credentials — password seeding and the session pool

**Files:**
- Modify: `harness/backend/src/seed.rs`
- Create: `harness/bench/seed-sessions.sh`

**Interfaces:**
- Consumes: `POST {prefix}/login` with `{"username","password"}` → `{user, token}` + `Set-Cookie: umbral_session=...`.
- Produces: `harness/bench/session_pool.json` — a JSON array of cookie values, loadable by k6 `SharedArray`.

- [ ] **Step 1: Use the verified credential API**

No guessing needed — this was verified against the working tree. The seed uses `umbral_auth::create_user` (`plugins/umbral-auth/src/lib.rs:1533`):

```rust
pub async fn create_user(username: &str, email: &str, plaintext: &str)
    -> Result<AuthUser, AuthError>
```

It hashes with argon2id (`Params::new(19_456, 2, 1, None)`), sets `is_active = true` — which is the **only** flag that gates login (`lib.rs:1655-1659`) — and normalises username and email to trim+lowercase. It is **deliberately non-validating** against the password-strength policy (`lib.rs:1574-1585`), unlike the `/api/auth/register` route, so the bench password needs no special shape. This is the same call the repo's own tests use (`plugins/umbral-auth/tests/integration.rs:140`).

The field is `password_hash`; the rotate-an-existing-user path is `umbral_auth::set_password<U: UserModel>(user: &mut U, plaintext: &str)` (`lib.rs:1690`).

- [ ] **Step 2: Verify the seeded timestamp format sorts correctly**

The feed orders by `created_at DESC` and SQLite stores it as TEXT, so a format that does not sort lexicographically returns a plausible but wrong feed with no error. Add a test to `harness/backend/src/seed.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_timestamps_sort_lexicographically() {
        // Earlier instants must compare less-than later ones as strings.
        let a = "2026-01-01T00:00:00.000Z";
        let b = "2026-01-01T00:00:00.001Z";
        let c = "2026-01-01T00:00:01.000Z";
        let d = "2026-12-31T23:59:59.999Z";
        let mut v = vec![d, b, a, c];
        v.sort();
        assert_eq!(v, vec![a, b, c, d]);
    }

    #[test]
    fn seed_emits_the_sortable_format() {
        // Pin the format the seed actually writes, so a change to it fails here
        // rather than silently reordering every feed.
        let now = Utc::now();
        let s = fmt_ts(now); // the seed's own formatter
        assert_eq!(s.len(), 24, "expected YYYY-MM-DDTHH:MM:SS.mmmZ, got {s}");
        assert!(s.ends_with('Z'));
        assert_eq!(&s[10..11], "T");
    }
}
```

If the seed has no `fmt_ts` helper, add one and use it at every `created_at` assignment in the file so the format has exactly one definition:

```rust
/// The one timestamp format the bench writes. Fixed-width and UTC, so SQLite's
/// TEXT comparison matches chronological order (the feed's ORDER BY depends on
/// this). Change this and the ordering tests above catch it.
fn fmt_ts(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}
```

- [ ] **Step 3: Run the tests to verify they fail, then pass**

```bash
cargo test --lib seed::tests 2>&1 | tail -20
```

Expected first: compile error on `fmt_ts`. After adding it and wiring it into every `created_at` write: `test result: ok`.

- [ ] **Step 4: Add a `seed-sessions` subcommand**

The pool must be built by code that lives in the binary, so it can use the same password API. Add a CLI command to `bench-prod` that creates `N` users with a known password and prints their ids. Wire it with `AppBuilder::command` (see `docs/specs/` for the framework's command surface) **or**, if that is more setup than it is worth, a plain `if args[1] == "seed-sessions"` branch before `umbral_cli::dispatch`. Prefer whichever the repo's other binaries already do — check `harness/backend/src/bin/bench-instrumented.rs` for how it exposes `migrate`/`serve`.

The command must: create users `bench_1..bench_N` (default `N=200`, override with `BENCH_SESSION_POOL_SIZE`) with a fixed password from `BENCH_PASSWORD` (default `bench-password-123`), and be idempotent — re-running finds existing users rather than duplicating them.

- [ ] **Step 5: Write the pool builder script**

Create `harness/bench/seed-sessions.sh`:

```bash
#!/usr/bin/env bash
# Build bench/session_pool.json by logging the pool users in for real.
#
# Each entry is the value of the `umbral_session` cookie the login returned.
# k6 reads this via SharedArray, exactly as the twelve-dollar challenge reads
# its seed/tokens.json — real sessions, built once, verified per request.
#
# Usage: seed-sessions.sh [port] [pool_size]
set -euo pipefail

PORT="${1:-8114}"
N="${2:-200}"
BASE="http://127.0.0.1:${PORT}"
OUT="$(cd "$(dirname "$0")" && pwd)/session_pool.json"
PASSWORD="${BENCH_PASSWORD:-bench-password-123}"

echo "logging in $N pooled users against $BASE ..."
tmp=$(mktemp)
printf '[' > "$tmp"
ok=0
for i in $(seq 1 "$N"); do
  resp=$(curl -s -i -X POST "$BASE/api/auth/login" \
    -H 'content-type: application/json' \
    -d "{\"username\":\"bench_$i\",\"password\":\"$PASSWORD\"}")
  cookie=$(printf '%s' "$resp" | grep -i '^set-cookie: umbral_session=' \
    | sed -E 's/^[Ss]et-[Cc]ookie: umbral_session=([^;]+).*/\1/' | tr -d '\r')
  if [ -z "$cookie" ]; then
    echo "  user bench_$i: NO COOKIE (status: $(printf '%s' "$resp" | head -1))" >&2
    continue
  fi
  [ "$ok" -gt 0 ] && printf ',' >> "$tmp"
  printf '"%s"' "$cookie" >> "$tmp"
  ok=$((ok + 1))
done
printf ']' >> "$tmp"

if [ "$ok" -eq 0 ]; then
  echo "ERROR: no sessions obtained — is the pool seeded and the login path right?" >&2
  rm -f "$tmp"; exit 1
fi
mv "$tmp" "$OUT"
echo "wrote $ok sessions to $OUT"
```

The login path is **`POST /api/auth/login`** — verified. `with_default_routes()` resolves its prefix to `{api_base()}/auth` at build time (`plugins/umbral-auth/src/lib.rs:813-821`), and `api_base()` defaults to `/api`. Both `/api/auth/login` and `/api/auth/login/` are registered (`auth_routes.rs:426-427`). It accepts JSON (anything that is not `application/x-www-form-urlencoded` is parsed as JSON, `auth_routes.rs:70-95`) and returns `{user, token}` with the session cookie.

One throttle to know about: failed logins are limited to **5 per IP+username per 5 minutes** (`plugins/umbral-auth/src/throttle.rs:158-159`), and the counter is forgiven on success (`auth_routes.rs:856`). Each pool user is attempted once, so a clean run never trips it — but if Step 6 reports `429`s, the pool was seeded against a binary without those users and the retries are what tripped the throttle, not load.

- [ ] **Step 6: Build the pool end to end**

```bash
chmod +x harness/bench/seed-sessions.sh
cd harness/backend
BENCH_SESSION_POOL_SIZE=200 ./target/release/umbral-bench-prod seed-sessions
./target/release/umbral-bench-prod serve &
sleep 20
./bench/seed-sessions.sh 8114 200
```

Expected: `wrote 200 sessions to .../session_pool.json`.

- [ ] **Step 7: Prove a pooled cookie authenticates a real write**

```bash
COOKIE=$(python3 -c 'import json,sys; print(json.load(open("harness/bench/session_pool.json"))[0])')
echo "--- with cookie: expect 2xx ---"
curl -s -o /dev/null -w '%{http_code}\n' -X POST localhost:8114/api/posts \
  -H "cookie: umbral_session=$COOKIE" \
  -H 'content-type: application/json' -d '{"body":"pooled auth works"}'
echo "--- with a forged cookie: expect 401 ---"
curl -s -o /dev/null -w '%{http_code}\n' -X POST localhost:8114/api/posts \
  -H "cookie: umbral_session=forged-nonsense" \
  -H 'content-type: application/json' -d '{"body":"should fail"}'
```

Expected: first `201` (or `200`), second `401`. A forged cookie succeeding means sessions are not being verified — stop and fix before any number is recorded.

- [ ] **Step 8: Commit**

```bash
git add harness/backend/src/seed.rs harness/backend/src/bin/bench-prod.rs harness/bench/seed-sessions.sh
git commit -m "feat(bench): real credentials — password seeding and the session pool"
```

---

### Task 6: The silo — Dockerfile and launcher

**Files:**
- Create: `harness/backend/Dockerfile`
- Create: `harness/backend/.dockerignore`
- Create: `harness/bench/run.sh`

**Interfaces:**
- Consumes: the `umbral-bench-prod` release binary (Task 4).
- Produces: `bench/run.sh <command>` where command is `serve` or `shell`; a container named `umbral-bench` on host networking bound to 8114.

- [ ] **Step 1: Confirm the binary's runtime dependencies**

```bash
cd harness/backend
cargo build --release --bin umbral-bench-prod
ldd target/release/umbral-bench-prod
```

Expected: `libc`, `libgcc_s`, `libm`, and the dynamic loader. SQLite is bundled and static (sqlx's `sqlite` feature compiles `libsqlite3-sys` from source), so there must be **no** `libsqlite3.so` entry. If there is, the base image needs that package — note it and adjust Step 2.

- [ ] **Step 2: Write the Dockerfile**

The host is Ubuntu 22.04 with glibc 2.35, so the runtime must be Ubuntu 22.04 or newer (glibc is backward-compatible, not forward). The binary is built on the host, not in the image — a multi-stage build would recompile all of umbral inside the container for no benefit.

```dockerfile
# Runtime image for the measured application.
#
# The binary is built on the HOST (cargo build --release) and copied in. A
# conventional multi-stage Dockerfile would rebuild the entire umbral workspace
# inside the container: slow, and tens of GB of build artefacts.
#
# Base is pinned to ubuntu:22.04 to match the build host's glibc 2.35. glibc is
# backward-compatible but not forward, so a newer runtime is fine and an older
# one is not.
FROM ubuntu:22.04

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates procps \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY target/release/umbral-bench-prod /app/umbral-bench-prod
COPY templates /app/templates
COPY migrations /app/migrations

# Data lives on a bind mount so the SQLite file is on the host's NVMe and not
# on overlayfs (which copy-up's on every write).
VOLUME ["/data"]

# `migrations/` MUST be copied, and its absence is not a harmless omission.
# The migration engine reads a missing migrations directory as "every applied
# migration is missing" and fails boot with
# `DriftDetected { missing: [("bench","0001_auto"), ...] }` — a startup failure
# that looks like a schema problem and is actually a missing COPY. Task 4 hit
# exactly this drift from a different cause (no plugin registered to own the
# bench migrations) and it cost a diagnosis cycle there.

ENV UMBRAL_DATABASE_URL="sqlite:///data/bench-run.db" \
    UMBRAL_BIND="0.0.0.0:8114" \
    RUST_LOG="warn"

EXPOSE 8114
ENTRYPOINT ["/app/umbral-bench-prod"]
CMD ["serve"]
```

- [ ] **Step 3: Write the .dockerignore**

Create `harness/backend/.dockerignore`:

```
target/debug
target/release/build
target/release/deps
target/release/incremental
traces/
bench.db*
*.db
*.db-shm
*.db-wal
```

Note: `target/release/umbral-bench-prod` must NOT be excluded — it is what gets copied in.

- [ ] **Step 4: Write the launcher**

Create `harness/bench/run.sh`:

```bash
#!/usr/bin/env bash
# Launch the measured application inside the fixed-resource silo.
#
# The envelope is the whole point: every number ever quoted must come from a
# container started by this script, so the CPU and memory available are
# identical between the baseline and every later comparison.
#
# `--cpuset-cpus=0,2,4,6` pins four DISTINCT PHYSICAL P-cores. On this i7-13700H
# the P-cores are hyperthreaded pairs (physical core 0 = cpus {0,1}, core 1 =
# {2,3}, core 2 = {4,5}, core 3 = {6,7}), so `--cpuset-cpus=0-3` would grant
# two physical cores, not four. Siblings 1,3,5,7 are left idle deliberately —
# k6 must never share a physical core with the server.
#
# `--cpus=4` is a redundant CFS cap; the cpuset is authoritative.
# `--memory-swap=4g` equals `--memory=4g`, which means NO SWAP: matching the
# challenge's stated rule, and making an over-budget run a hard OOM rather than
# a slow, unreproducible degradation.
#
# `--network host` means k6 reaches loopback directly. A published port would
# route through docker-proxy — the reverse-proxy hop the challenge explicitly
# rewards deleting, and measuring Docker rather than umbral.
set -euo pipefail

NAME=umbral-bench
HERE="$(cd "$(dirname "$0")" && pwd)"
BACKEND="$(cd "$HERE/../backend" && pwd)"
DATA="$HERE/data"
IMAGE="$NAME:latest"

mkdir -p "$DATA"

case "${1:-serve}" in
  build)
    echo "building release binary on the host ..."
    (cd "$BACKEND" && cargo build --release --bin umbral-bench-prod)
    echo "building image ..."
    docker build -t "$IMAGE" "$BACKEND"
    ;;
  serve)
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    docker run -d --name "$NAME" \
      --cpuset-cpus=0,2,4,6 \
      --cpus=4 \
      --memory=4g --memory-swap=4g \
      --network host \
      -v "$DATA:/data" \
      -e "${UMBRAL_DATABASE_URL:-UMBRAL_DATABASE_URL=sqlite:///data/bench-run.db}" \
      "$IMAGE" serve
    echo "started $NAME (pid $(docker inspect -f '{{.State.Pid}}' "$NAME"))"
    echo "waiting for readiness ..."
    for _ in $(seq 1 60); do
      if curl -sf "http://127.0.0.1:8114/api/ping" >/dev/null 2>&1; then
        echo "ready on 8114"
        exit 0
      fi
      sleep 1
    done
    echo "ERROR: not ready after 60s — last 40 log lines:" >&2
    docker logs --tail 40 "$NAME" >&2
    exit 1
    ;;
  stop)
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    echo "stopped"
    ;;
  shell)
    docker exec -it "$NAME" /bin/bash
    ;;
  verify)
    # Assert the container actually got the envelope we asked for. Run this
    # before trusting any number: a typo in the cpuset silently benchmarks a
    # different machine.
    echo "=== cpuset (expect 0,2,4,6) ==="
    docker inspect -f '{{.HostConfig.CpusetCpus}}' "$NAME"
    echo "=== cpu quota (expect 400000/100000 = 4) ==="
    docker inspect -f '{{.HostConfig.NanoCpus}}/{{.HostConfig.CpuPeriod}}' "$NAME"
    echo "=== memory (expect 4294967296) ==="
    docker inspect -f '{{.HostConfig.Memory}}' "$NAME"
    echo "=== memory+swap (expect equal to memory = no swap) ==="
    docker inspect -f '{{.HostConfig.MemorySwap}} {{.HostConfig.Memory}}' "$NAME"
    echo "=== from inside the container ==="
    docker exec "$NAME" nproc
    docker exec "$NAME" sh -c 'grep MemTotal /proc/meminfo'
    ;;
  *)
    echo "usage: run.sh [build|serve|stop|shell|verify]" >&2
    exit 2
    ;;
esac
```

- [ ] **Step 5: Build and boot the silo**

```bash
chmod +x harness/bench/run.sh
harness/bench/run.sh build
harness/bench/run.sh serve
```

Expected: image builds, container starts, `ready on 8114`.

- [ ] **Step 6: Verify the envelope is real**

```bash
harness/bench/run.sh verify
```

Expected: cpuset `0,2,4,6`; cpu quota `400000/100000`; memory `4294967296`; memory+swap equal to memory; `nproc` inside the container prints `4`. **Any mismatch invalidates every number from this container** — fix it before proceeding.

- [ ] **Step 7: Verify an OOM is distinguishable from a latency failure**

```bash
harness/bench/run.sh shell   # then, inside:
#   cat /sys/fs/cgroup/memory.max          -> expect 4294967296
#   exit
docker inspect -f '{{.State.OOMKilled}}' umbral-bench   # expect false while healthy
```

Expected: `memory.max` equals 4 GiB and `OOMKilled` is `false`. The runner records `OOMKilled` after every hold (Task 7) so a killed container is never mistaken for a slow one.

- [ ] **Step 8: Commit**

```bash
git add harness/backend/Dockerfile harness/backend/.dockerignore harness/bench/run.sh
git commit -m "feat(bench): fixed-resource Docker silo for the measured app"
```

---

### Task 7: The k6 suite — warm-up and hold

**Files:**
- Create: `harness/bench/k6/warmup.js`
- Create: `harness/bench/k6/hold.js`
- Create: `harness/bench/workload.md`

**Interfaces:**
- Consumes: `harness/bench/session_pool.json` (Task 5).
- Produces: k6 exit code 0/non-zero as pass/fail; a `--summary-export` JSON consumed by Task 8.

- [ ] **Step 1: Write the workload document with the mix left to the operator**

Create `harness/bench/workload.md`. It records that the mix is a deliberate choice and what it was chosen to represent. It must contain a table of the mix currently in `hold.js` and a sentence on what a change would mean. **The mix table is the operator's to fill in** — leave the three probability cells as `_____` in this file and fill them from whatever `hold.js` ships with once the operator has chosen.

- [ ] **Step 2: Write `warmup.js`**

```javascript
// Challenge phase 1 — the unscored warm-up.
//
// Runs the full workload at 1,000 VUs for 2 minutes to fill the connection
// pool, the SQLite page cache, and the session store before anything is
// measured. A hold started on a cold server measures warm-up, not throughput.
//
// Run: taskset -c 8,9,10,11 k6 run -e BASE=http://127.0.0.1:8114 bench/k6/warmup.js

import { sleep } from "k6";
import { SharedArray } from "k6/data";
import { doIteration, buildOptions } from "./lib.js";

const sessions = new SharedArray("sessions", () =>
  JSON.parse(open("../session_pool.json"))
);

export const options = buildOptions({ vus: 1000, duration: "2m", scored: false });

export default function () {
  doIteration(sessions);
  sleep(1);
}
```

- [ ] **Step 3: Write the shared iteration library**

Create `harness/bench/k6/lib.js` so warm-up, hold and saturation cannot drift apart:

```javascript
// The workload, in one place. warmup.js, hold.js and saturation.js all call
// doIteration so a change to the mix cannot make two scripts disagree about
// what "the workload" is.
//
// Endpoints and mix follow the twelve-dollar challenge's shape (a read-heavy
// feed with occasional engagement writes), extended with an HTML page so the
// template-render path is exercised — a dimension the challenge has none of.
import http from "k6/http";
import { check, sleep } from "k6";
import { Trend } from "k6/metrics";

export const BASE = __ENV.BASE || "http://127.0.0.1:8114";
export const USERS = Number(__ENV.USERS || 50000);
export const POSTS = Number(__ENV.POSTS || USERS * 10);

// ---------------------------------------------------------------------------
// THE MIX — the operator's call. These three numbers define what "fast" means
// for this benchmark. The defaults follow the twelve-dollar challenge:
// a read-dominant feed.
//   LIKE_P = probability an iteration also likes a post
//   POST_P = probability an iteration also creates a post
//   PAGE_P = probability an iteration reads an HTML page instead of the JSON feed
// ---------------------------------------------------------------------------
export const LIKE_P = Number(__ENV.LIKE_P || 0.15);
export const POST_P = Number(__ENV.POST_P || 0.02);
export const PAGE_P = Number(__ENV.PAGE_P || 0.10);

export const serverTotal = new Trend("server_ms", true);

const uid = () => 1 + Math.floor(Math.random() * USERS);
const pid = () => 1 + Math.floor(Math.random() * POSTS);

function think(minMs, maxMs) {
  // Challenge think time is 3-8s between requests. Set THINK_MIN/THINK_MAX to
  // 0 for the saturation test, which is the only run that has no think time.
  const lo = Number(__ENV.THINK_MIN ?? 3000);
  const hi = Number(__ENV.THINK_MAX ?? 8000);
  if (hi <= 0) return;
  sleep((lo + Math.random() * (hi - lo)) / 1000);
}

function jar(session) {
  // Every VU is pinned to one pooled session, so its requests are a single
  // coherent user rather than a new identity per call.
  return session ? { cookies: { umbral_session: session } } : {};
}

function go(method, path, session, body) {
  const params = {
    ...jar(session),
    tags: { name: path.replace(/\/\d+/g, "/{id}") },
  };
  let res;
  if (method === "GET") {
    res = http.get(`${BASE}${path}`, params);
  } else {
    res = http.post(`${BASE}${path}`, body === undefined ? null : JSON.stringify(body), {
      ...params,
      headers: { "content-type": "application/json" },
    });
  }
  const key = Object.keys(res.headers).find((k) => k.toLowerCase() === "server-timing");
  if (key) {
    const m = /(?:^|,\s*)total;dur=([0-9.]+)/.exec(res.headers[key]);
    if (m) serverTotal.add(Number(m[1]));
  }
  return res;
}

/// One iteration of the workload. `sessions` is the SharedArray from the pool.
export function doIteration(sessions) {
  const session = sessions[(__VU - 1) % sessions.length];

  // Every iteration reads the feed (the dominant operation).
  const feed = go("GET", "/api/feed", session);
  check(feed, { "feed 200": (r) => r.status === 200 });

  // Occasionally a heavier read: the HTML profile page.
  if (Math.random() < PAGE_P) {
    const page = go("GET", `/u/${uid()}`, session);
    check(page, { "page 200": (r) => r.status === 200 });
    think(3000, 8000);
    return;
  }

  think(3000, 8000);

  // A single post read, as the challenge does.
  const post = go("GET", `/posts/${pid()}`, session);
  check(post, { "post 200 or 404": (r) => r.status === 200 || r.status === 404 });

  if (Math.random() < LIKE_P) {
    const like = go("POST", `/api/posts/${pid()}/like`, session, {});
    check(like, { "like 2xx": (r) => r.status >= 200 && r.status < 300 });
  }
  if (Math.random() < POST_P) {
    const created = go("POST", "/api/posts", session, {
      body: `bench ${Date.now()} vu${__VU} iter${__ITER}`,
    });
    check(created, { "create 2xx": (r) => r.status >= 200 && r.status < 300 });
  }
}

export function buildOptions({ vus, duration, scored, ramp }) {
  const opts = {
    discardResponseBodies: false, // the feed body is parsed, so this stays false
    summaryTrendStats: ["avg", "med", "p(95)", "p(99)", "max"],
    scenarios: {
      users: {
        executor: "ramping-vus",
        startVUs: 0,
        gracefulRampDown: "10s",
        stages: ramp || [
          { duration: "60s", target: vus },
          { duration: duration || "5m", target: vus },
          { duration: "30s", target: 0 },
        ],
      },
    },
  };
  if (scored) {
    opts.thresholds = {
      http_req_duration: ["p(95)<500", "p(99)<1000"],
      http_req_failed: ["rate<0.01"],
    };
  }
  return opts;
}
```

- [ ] **Step 4: Write `hold.js`**

```javascript
// Challenge phase 3 — the scored 5-minute hold.
//
// Ramp to VUS over 60s, hold for 5 minutes, ramp down. Encodes the challenge's
// pass criteria as k6 thresholds, so a non-zero exit code IS the verdict.
//
// Run: taskset -c 8,9,10,11 k6 run -e VUS=2500 \
//        --summary-export=bench/results/raw.json bench/k6/hold.js

import { SharedArray } from "k6/data";
import { doIteration, buildOptions } from "./lib.js";

const sessions = new SharedArray("sessions", () =>
  JSON.parse(open("../session_pool.json"))
);

const VUS = Number(__ENV.VUS || 2500);

export const options = buildOptions({ vus: VUS, duration: "5m", scored: true });

export default function () {
  doIteration(sessions);
}
```

- [ ] **Step 5: Verify a short hold runs and reports**

```bash
harness/bench/run.sh serve
taskset -c 8,9,10,11 ~/.local/bin/k6 run \
  -e VUS=50 -e THINK_MIN=100 -e THINK_MAX=300 \
  --summary-export=/tmp/hold-smoke.json \
  harness/bench/k6/hold.js
```

Expected: the run completes, `http_req_duration` and `http_req_failed` appear in the summary, and the exit code is 0. A `401`-heavy failure mix means the session pool is not being applied — check `lib.js`'s cookie wiring against a pooled cookie manually.

- [ ] **Step 6: Verify the k6 fd limit is adequate**

```bash
ulimit -n
```

Expected: if this is below ~20000, a high-VU hold will fail client-side with `EMFILE` and it will look like a server failure. Record the value. If it is low, raise it for the benchmark shell (and record that you did):

```bash
ulimit -n 65535   # may require a privileged shell
```

The runner records `ulimit -n` in every result so a client-side limit is never mistaken for a server ceiling.

- [ ] **Step 7: Commit**

```bash
git add harness/bench/k6 harness/bench/workload.md
git commit -m "feat(bench): k6 warm-up and 5-minute hold, with the mix in one place"
```

---

### Task 8: Results store and comparison

**Files:**
- Create: `harness/bench/results/record.sh`
- Create: `harness/bench/results/compare.sh`
- Create: `harness/bench/results/README.md`

**Interfaces:**
- Consumes: k6's `--summary-export` JSON, `docker inspect`, `ulimit -n`.
- Produces: `results/<git-sha>-<utc>.json`; `compare.sh <a.json> <b.json>` exiting non-zero on regression.

- [ ] **Step 1: Write the recorder**

`harness/bench/results/record.sh` takes the k6 summary path, the VU count, the dataset scale, and the exit code, and writes one JSON with every fact needed to judge whether the number means anything: git sha, binary name, container limits and cpuset (read from `docker inspect`), the host's load average at start, `ulimit -n`, peak thermal-zone temperature during the run, whether throttling was observed, `OOMKilled`, whether a WAL checkpoint stall was seen, the session max-age, the pass/fail verdict, and the mix parameters. It must refuse to write a result if `run.sh verify`'s cpuset does not match the expected value — a result from an unverified envelope is not a result.

- [ ] **Step 2: Write the comparator**

`compare.sh` diffs two result files and exits non-zero when `rps` moves by more than 10% in either direction or `p99` by more than 20%, and refuses outright when `dataset_scale`, `cpuset`, or `mix` differ. It prints a one-line verdict per metric.

- [ ] **Step 3: Verify the comparator**

Build a synthetic pair first so this step is self-contained (no real run needed yet):

```bash
mkdir -p /tmp/cmp && cd /tmp/cmp
cat > a.json <<'JSON'
{"git_sha":"aaa","rps":1000,"p99_ms":200,"dataset_scale":20000,
 "cpuset":"0,2,4,6","mix":{"LIKE_P":0.15,"POST_P":0.02,"PAGE_P":0.10}}
JSON
cp a.json same.json
python3 - <<'PY'
import json
d = json.load(open("a.json"))
d["rps"] = 600            # -40%, beyond the 10% regression threshold
json.dump(d, open("regressed.json", "w"))
d2 = json.load(open("a.json")); d2["dataset_scale"] = 50000
json.dump(d2, open("rescaled.json", "w"))
PY

harness/bench/results/compare.sh a.json same.json ; echo "identical -> exit=$? (expect 0)"
harness/bench/results/compare.sh a.json regressed.json; echo "regressed -> exit=$? (expect non-zero, names rps)"
harness/bench/results/compare.sh a.json rescaled.json; echo "rescaled  -> exit=$? (expect non-zero, refuses)"
```

(Adjust the field names in the heredocs to whatever `record.sh` actually writes — the point is that all three behaviours are exercised.)

- [ ] **Step 4: Commit**

```bash
git add harness/bench/results
git commit -m "feat(bench): results store and regression comparator"
```

---

### Task 9: The baseline

This is the deliverable: three scored holds at one VU count, reported with variance, plus the caveats that make the number trustworthy.

**Files:**
- Modify: `harness/bench/README.md` (create if absent)

**Interfaces:**
- Consumes: everything above.
- Produces: three result JSONs and a written baseline.

- [ ] **Step 1: Bring the dataset to scale**

Per spec §2a: 20,000 users for development, 50,000 for any number that gets quoted. Pick one and record it — `compare.sh` will refuse to diff across scales.

```bash
cd harness/backend
UMBRAL_BENCH_USERS=20000 UMBRAL_BENCH_POSTS_PER_USER=10 UMBRAL_BENCH_LIKES_PER_USER=4 \
  ./target/release/umbral-bench-prod migrate
# Seeding is resumable and never wipes: re-running tops up.
UMBRAL_BENCH_USERS=20000 UMBRAL_BENCH_POSTS_PER_USER=10 UMBRAL_BENCH_LIKES_PER_USER=4 \
  ./target/release/umbral-bench-prod serve &
# wait for "seed complete", then Ctrl-C / kill the host-side process
```

Then move the seeded file into the silo's data directory so the container uses it:

```bash
cp harness/backend/bench.db harness/bench/data/bench-run.db
```

**Never delete an existing bench database to "get a clean run"** — the resume logic tops up, and a wipe destroys the rows that make the seed meaningful. If the shape needs to change, change the scale parameters and let it top up.

- [ ] **Step 2: Warm up**

```bash
harness/bench/run.sh build && harness/bench/run.sh serve
harness/bench/run.sh verify          # envelope MUST match before proceeding
taskset -c 8,9,10,11 ~/.local/bin/k6 run -e BASE=http://127.0.0.1:8114 \
  harness/bench/k6/warmup.js
```

- [ ] **Step 3: Run the first hold**

Pick a starting VU count. The challenge starts its search at 2,500; do the same so the first data point is comparable.

```bash
taskset -c 8,9,10,11 ~/.local/bin/k6 run \
  -e BASE=http://127.0.0.1:8114 -e VUS=2500 \
  --summary-export=/tmp/hold-1.json \
  harness/bench/k6/hold.js; echo "exit=$?"
harness/bench/results/record.sh /tmp/hold-1.json 2500 20000 $?
```

- [ ] **Step 4: Cool down, then repeat twice**

2–3 minutes idle between runs, per spec locked decision #6. This machine heat-soaks; without the cooldown run 3 is slower than run 1 for reasons that have nothing to do with umbral.

```bash
sleep 180
taskset -c 8,9,10,11 ~/.local/bin/k6 run -e BASE=http://127.0.0.1:8114 -e VUS=2500 \
  --summary-export=/tmp/hold-2.json harness/bench/k6/hold.js; echo "exit=$?"
harness/bench/results/record.sh /tmp/hold-2.json 2500 20000 $?
sleep 180
taskset -c 8,9,10,11 ~/.local/bin/k6 run -e BASE=http://127.0.0.1:8114 -e VUS=2500 \
  --summary-export=/tmp/hold-3.json harness/bench/k6/hold.js; echo "exit=$?"
harness/bench/results/record.sh /tmp/hold-3.json 2500 20000 $?
```

- [ ] **Step 5: Report the spread, not the mean**

Compute and write down RPS and p95/p99 for all three, plus the spread. **If the spread exceeds ±10%, the rig is not yet a ruler** — investigate thermal throttling (compare peak temps and core MHz across the three) before quoting anything. Record the finding either way.

- [ ] **Step 6: Write the baseline into `harness/bench/README.md`**

State: the three results, the spread, whether it passed the thresholds, the dataset scale, the envelope, the mix, and — explicitly — that this is the first number from this rig and that no earlier harness number is comparable to it. Note the spec's `DbStore` decision so a reader knows a per-request session read is included in the cost.

- [ ] **Step 7: Commit**

```bash
git add harness/bench/README.md harness/bench/workload.md
git commit -m "docs(bench): first trustworthy baseline"
```

---

## Phase 2 — The boundaries

### Task 10: `search.sh` — the limit search

**Files:**
- Create: `harness/bench/search.sh`

**Interfaces:**
- Consumes: `hold.js`, `record.sh`, `compare.sh`.
- Produces: `results/search-<timestamp>.json` recording the discovered maximum and the search path.

- [ ] **Step 1: Implement the search**

Per spec's resolved decision #3: start at 2,500, double until a hold fails, narrow to within 250, and **cap the doubling range at 40,000 VUs**. Enforce the 2–3 minute cooldown between every hold. Record every hold's result, including the failures.

When the cap is reached without a failure, report the result as a **floor**, not a discovered maximum — the true breaking point is unknown and the summary must say so.

- [ ] **Step 2: Verify the search logic without paying for it**

Add a `--dry-run` that prints the planned hold sequence (VU counts, expected wall clock) without running k6. Confirm the sequence for a hypothetical "fails at 20,000, passes at 10,000" reaches a within-250 answer, and that a "passes at 40,000" run terminates at the cap and reports a floor.

- [ ] **Step 3: Run it**

This is a 60–90 minute job by construction. Run it once, in the background, and record the wall clock.

- [ ] **Step 4: Commit**

```bash
git add harness/bench/search.sh
git commit -m "feat(bench): limit search with a 40k VU cap"
```

---

### Task 11: `saturation.js` — the RPS ceiling

The VU score and "can it do 25,000 RPS" are different questions. The challenge-shaped workload is sleep-dominated (roughly `RPS ≈ VUs × 0.106`), so it can never answer the second one.

**Files:**
- Create: `harness/bench/k6/saturation.js`

**Interfaces:**
- Consumes: `lib.js`'s `doIteration` and the session pool.
- Produces: max sustained RPS at which thresholds still hold.

- [ ] **Step 1: Write the saturation script**

Use `constant-arrival-rate` with `preAllocatedVUs` and a high `maxVUs`, and `THINK_MIN=THINK_MAX=0` so the generator offers a fixed request rate rather than a fixed user count. Ramp the arrival rate until thresholds break, exactly as the challenge ramps VUs.

- [ ] **Step 2: Verify against a deliberately low rate**

```bash
taskset -c 8,9,10,11 ~/.local/bin/k6 run -e BASE=http://127.0.0.1:8114 -e RATE=50 \
  --summary-export=/tmp/sat-smoke.json harness/bench/k6/saturation.js
```

Expected: k6 reports an achieved rate near 50/s with p95 well under 500 ms — this proves the script measures rate, not just load.

- [ ] **Step 3: Find the ceiling and state the gap**

Run the rate search. Report max sustainable RPS, and the measured gap to 25,000 RPS.

- [ ] **Step 4: Commit**

```bash
git add harness/bench/k6/saturation.js
git commit -m "feat(bench): saturation test for the RPS ceiling"
```

---

## Self-Review

**Spec coverage.** Every spec section maps to a task: §1 the silo → Task 6; §2 the prod binary + §2a dataset scale → Tasks 3, 4, 9; §3 the session pool → Task 5; §4 the k6 suite → Tasks 7, 10, 11; §5 threats to validity → folded into Tasks 6 (OOM), 7 (fd limit), 8 (WAL + temps), 9 (variance and cooldown); §6 results → Task 8; Phasing → Phase 1 is Tasks 1–9, Phase 2 is Tasks 10–11. Locked decisions: #1 → Tasks 2, 3; #2 → Tasks 4, 5; #3 → Task 4 (`SessionsPlugin::default()`); #4, #5 → Task 6; #6 → Tasks 9, 10; #7 → Tasks 7, 9; #8 → Global Constraints.

**Placeholder scan.** No "TBD"/"TODO". The three `_____` cells in `workload.md` are deliberate: the mix is the operator's decision and Step 1 of Task 7 says so explicitly and tells the operator how to fill them from `hold.js`.

**Verified vs. still-unverified APIs.** The whole credential chain was verified against the working tree after the plan was first drafted, and the plan now carries the real names: `umbral_auth::create_user` / `set_password` (Task 5 Step 1), the `POST /api/auth/login` prefix and its JSON/`Throttle` behaviour (Task 5 Step 6), the `umbral_session` cookie and its 14-day `Max-Age` (Review Focus 4), and the `OptionalIdentity` extractor as the thing `bench-prod` should use (Task 4 Step 2). Two details remain explicitly flagged for in-situ confirmation rather than asserted from memory: `Settings.bind_addr` mutability, and `OptionalIdentity`'s exact accessor name and return type. Both are one `grep` each and neither is load-bearing for the design. If `OptionalIdentity` turns out not to expose a user id at all, that is a **genuine framework gap** to report rather than work around — the plan says so in place.

**Type consistency.** `domain::`'s signatures are declared once in Task 3's Interfaces block and used identically in Tasks 3 and 4. `clamp_body`, `create_post`, `like_post`, `follow_user` all take `user_id: i64` in both. `buildOptions({vus, duration, scored, ramp})` is defined in Task 7 Step 3 and called with those exact keys in Steps 2 and 4.
