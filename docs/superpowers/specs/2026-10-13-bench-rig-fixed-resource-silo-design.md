# Bench rig v2 — fixed-resource silo, external k6, trustworthy baseline

Status: **design approved 2026-10-13**, awaiting spec review before implementation. Supersedes the measurement methodology of `harness/` as built on 2026-10-10. Related: `docs/decisions/2026-08-08-fuzz-upgrade-load-tests.md` (the load/soak gap this answers), `planning/review_4/performance.md` (the static sweep this is the measurement half of).

## Why this doc

The harness built on 2026-10-10 is a good *diagnostic* instrument and a bad *ruler*. It answers "where did the time go" precisely, and it has never answered "how fast is umbral" honestly. The recorded evidence is `harness/backend/traces/runs.jsonl`: 11 runs, every one between 6 and 8 seconds, the largest at 150 VUs, the best reading 2,550 RPS at p95 145 ms. No number in that file measures the framework, because seven independent flaws each moved it:

1. **The load generator runs inside the server process.** `harness/backend/src/loadgen.rs` spawns VU tokio tasks in the same binary as the handlers, so client and server contend for the same cores. Under saturation the generator is not a small tax, it is a competing tenant.
2. **Runs are 6–8 seconds.** There is no steady state to measure — pool warm-up, page cache fill, and TCP ramp dominate the sample.
3. **No resource envelope.** The app has only ever run on a 20-thread / 30 GB host. Numbers do not transfer to any other machine and cannot be compared across commits.
4. **No limit search.** VU counts were chosen by hand (150, 15, 10). The VU count at which the latency budget breaks has never been found, so no run is known to be near the boundary.
5. **`DevAuth` bypasses authentication.** `harness/backend/src/main.rs:84` mounts a bench-only middleware that reads an `x-bench-user` header and constructs an identity with no database access. Every recorded number excludes the cost of authenticating.
6. **Benchmark scaffolding sits inside the measured path.** Four telemetry middlewares (`main.rs:83–86`), four `ProbePlugin`s (`main.rs:95–119`), and two framework-lab middlewares (`main.rs:90–91`) run on every request. They are instrumentation, not application code, but they are inside every number.
7. **Statement logging is forced on.** `main.rs:56–58` sets `UMBRAL_DB_LOG_STATEMENTS=debug` unless the operator overrode it, making sqlx emit a tracing event per statement on every query.

The conclusion is not "the numbers were slightly off". It is that **no prior number is usable as a baseline**, and therefore nothing needs reconciling before we start.

## Goal

A ruler, not a product. After this work, a single command must produce a number that is:

- **Reproducible** — the same commit run twice on a quiet machine agrees within a stated tolerance.
- **Enveloped** — measured inside a declared CPU/memory budget that anyone else can recreate exactly.
- **Honest** — the application under measurement does everything a real deployment of the same app would do: authenticates, writes durably, renders templates, emits security headers. Nothing is stubbed, and nothing that is not application code is in the path.
- **Comparable** — the methodology matches `arjaythedev/twelve-dollar-server-challenge` closely enough that their published results serve as a calibration reference for what a hand-optimized implementation of a similar shape achieves.

### Non-goals

- Beating the challenge's leaderboard. The challenge scores *virtual users on a 1 vCPU / 2 GB box with a fixed 5-endpoint SQLite app*; this rig measures *a full-stack umbral application on a 4 vCPU / 4 GB box*. The workloads resemble each other; the scores do not compare. Their numbers are a sanity reference, not a target.
- Replacing the instrumented harness. It stays, and becomes more valuable: it is the tool we point at whatever this rig says is slow.
- Measuring Postgres. Phase 1 is SQLite-only. A Postgres variant needs a second container, which changes the resource envelope, so it is a separate exercise.

## Locked decisions

1. **Approach A — two bin targets in one crate.** `harness/backend` gains `bench-prod` beside the existing instrumented binary. The prod binary never *constructs* the telemetry middleware, probe plugins, lab middlewares, or `DevAuth`, so measuring with instrumentation accidentally switched on is structurally impossible rather than merely discouraged. The instrumented binary is documented as the diagnostic tool and no number from it is ever quoted as a performance result.
2. **Real session authentication in the loop.** The prod bench authenticates through `umbral-auth`/`umbral-sessions` for real. `DevAuth` does not exist in the prod binary.
3. **`DbStore` for sessions, not `CookieStore`.** `SessionsPlugin::default()` is what a developer gets, so a real database session read per authenticated request is part of the honest cost. The instrumented app's `CookieStore` (`main.rs:106`) was chosen to keep the sessions row from dominating a *diagnostic* view; adopting it here would design away real work. If session lookups dominate the prod number, that is a finding to act on.
4. **The silo is defined by `--cpuset-cpus`, not `--cpus`.** `--cpus` is a CFS quota that permits migration across all 20 hardware threads; only a cpuset pins the workload to specific physical cores. `--cpus=4` is kept as a redundant safety net.
5. **`--network host`.** A published Docker port routes through `docker-proxy` (or iptables DNAT). That is precisely the reverse-proxy hop the challenge rewards deleting, and including it would mean measuring Docker rather than umbral.
6. **Cooldown between runs: 2–3 minutes**, enforced by the runner. This is a laptop, and a sustained 5-minute full load heat-soaks the package.
7. **The workload mix is a deliberate, documented choice**, not an accident of whatever the old script did. It is specified in `harness/bench/workload.md` and changes only by explicit decision, because it defines what "fast" means.
8. **No `unsafe` in optimization work.** Every Phase 3 change must be safe Rust. No `unsafe` blocks, no `unsafe fn`, no new `unsafe impl`, no `#[allow(unsafe_code)]`, and no depending on unstable or very recent Linux kernel behaviour to get a win. This is a deliberate acceptance of a slower ceiling.

   The reason it is a locked decision rather than a preference: the challenge's fastest submissions reach their numbers through hand-written HTTP and JSON parsers and `unsafe` code leaning on modern Linux features — the video is explicit that the winning C++ entry is 1,300 lines in a single file with its own parsers, and that the reviewer "probably wouldn't ship most of these implementations." A framework whose selling point is compile-time guarantees and "secure by default" cannot buy throughput with memory-safety escape hatches. An optimization that requires `unsafe` is therefore **out of scope, not deferred** — the honest response is to record the ceiling it imposes rather than smuggle it in.

   Consequence to state plainly in results: when a benchmark identifies a gap that can only be closed with `unsafe`, the finding is reported as **"not addressable within the safety budget"** with the measured cost of the gap, so the trade-off is visible rather than silently resolved.

## Architecture

```
harness/
  backend/
    src/bin/bench-prod.rs        # NEW — the measured application
    src/bin/bench-instrumented.rs # existing main.rs, moved + renamed
    Dockerfile                   # NEW — host-built binary into a slim runtime
  bench/
    run.sh                       # NEW — silo launcher (cpuset, memory, host net)
    search.sh                    # NEW — warm-up → limit search → confirm
    seed-sessions.sh             # NEW — build session_pool.json
    workload.md                  # NEW — the mix, and why
    k6/warmup.js hold.js saturation.js
    results/                     # one JSON per run + compare.sh
```

### 1. The silo

The container runs `bench-prod` under:

| Knob | Value | Rationale |
|---|---|---|
| `--cpuset-cpus` | `0,2,4,6` | Four **distinct physical** P-cores. Verified topology: this i7-13700H's P-cores are hyperthreaded pairs — physical core 0 is cpus `{0,1}`, core 1 is `{2,3}`, core 2 is `{4,5}`, core 3 is `{6,7}`, core 4 is `{8,9}`, core 5 is `{10,11}`; E-cores are `12–19`, one thread each. A naive `--cpuset-cpus=0-3` would therefore have granted **two** physical cores, not four. Siblings `1,3,5,7` are deliberately left idle. |
| `--cpus` | `4` | Redundant cap; the cpuset is authoritative. |
| `--memory` | `4g` | |
| `--memory-swap` | `4g` | Equals `--memory`, meaning **no swap** — matching the challenge's stated rule. |
| `--network` | `host` | See locked decision 5. |
| SQLite path | bind-mount to NVMe | Not a Docker volume: avoids overlayfs copy-up on every write. Matches the challenge's model of `SQLITE_PATH` being an ordinary file. |

**Build strategy.** Compile on the host with `cargo build --release`, then `COPY` the binary into a slim Debian runtime stage alongside `templates/` and `static/`. A conventional multi-stage Dockerfile would rebuild all of umbral inside the container: slow, and tens of gigabytes of build artifacts that do not fit the disk budget. The container image holds the artifact, not the build.

**k6 placement.** Pinned with `taskset -c 8,9,10,11` — two full physical P-cores (both threads of cores 4 and 5). k6 must never be scheduled onto the container's siblings, or client and server would share execution units.

**Port.** `8114`, verified free, with the instrumented harness keeping `8113`. Distinct ports so the rig cannot disturb a developer's running dev server. (Note for the record: the host's native Postgres on 5432/5434 and Redis on 6379 are system services, not containers, and were unaffected by the 2026-10-13 Docker cleanup.)

### 2. The `bench-prod` binary

Mounts exactly what a production deployment of this application would:

- Models with migrations owned by umbral, seeded at challenge scale.
- The JSON API and the HTML pages, so the template-render path is exercised — a dimension the challenge has none of.
- `AuthPlugin`, `SessionsPlugin` (`DbStore`), `SecurityPlugin` (CSRF active, `/api` exempt), `HealthPlugin`.
- **Not** present: `TraceRoot`, `DevAuth`, `RequestLog`, `ResponseStamp`, `ProbePlugin` ×4, `BenchLabMiddleware`, `RateLimitMiddleware`, and the `UMBRAL_DB_LOG_STATEMENTS` override.

### 2a. Dataset scale

"Users" is overloaded in this domain — it names both **database rows** and **concurrent virtual users** — so both are fixed here explicitly.

**Database rows.** The seeded shape is the challenge's: 50,000 users, 500,000 posts, ~2,000,000 likes. Because seeding 50k users is slow enough to make iteration painful, the rig treats scale as a **parameter with a documented progression**: `UMBRAL_BENCH_USERS=20000` for development and smoke runs, `50000` for any run whose number is quoted. A result file records the dataset scale it was measured against, and `compare.sh` refuses to diff two runs at different scales, because rows-per-table changes the index depth and therefore the latency of the very queries under measurement.

**Concurrent virtual users.** Not chosen by hand, ever. The VU count on a `hold.js` run is whatever `search.sh` discovered; the current scripts' hand-picked counts (150, 15, 10) are the specific mistake being corrected. The search starts at 2,500 and doubles.

### 3. The session pool

k6 needs real credentials. Logging in every VU during `setup()` is not viable: at 64,000 VUs that is 64,000 argon2 verifications at roughly 50–100 ms each, which is over an hour before the first request.

Instead, `seed-sessions.sh` logs in a pool of ~200 users once and writes their session cookies to `bench/session_pool.json`. k6 loads it via `SharedArray`, exactly as the challenge loads `seed/tokens.json`. Sessions are real; the pool is small; and **every request still performs its real session-store lookup**, so per-request verification is genuine and is included in the measured cost.

### 4. The k6 suite

The current scripts use `sleep(0.01)` between requests, which is a saturation hammer mislabelled as a workload. The replacements:

| Script | Shape | Purpose |
|---|---|---|
| `warmup.js` | 1,000 VUs, 2 min, unscored | Challenge phase 1. Fills pool, page cache, and TCP state before anything is measured. |
| `hold.js` | ramp 60 s → **hold 5 min** → ramp down 30 s | Challenge phase 3. `-e VUS=N`. Thresholds `p95<500`, `p99<1000`, `failed<1%`. Pass/fail is the k6 exit code. |
| `search.sh` | start 2,500 VUs, double until a hold fails, narrow to within 250 | Challenge phase 2. Enforces the cooldown. Every hold is 5 minutes, so a full search is a long-running job by construction — this cost is accepted, not worked around, because a shorter hold is exactly the mistake being corrected. |
| `saturation.js` | `constant-arrival-rate`, no think time | The "25,000 RPS" question. Separate from the VU score, which the challenge's think-time-shaped workload cannot answer. |

All four use real session auth and tag requests by **path template** so `/api/users/123/profile` reports as `/api/users/{id}/profile`.

### 5. Threats to validity

These are properties of benchmarking a mobile CPU on a laptop, and each needs an explicit countermeasure rather than optimism:

- **Thermal throttling.** An i7-13700H under five minutes of full load will heat-soak; a later run is slower for reasons unrelated to the code. Countermeasures: the 2–3 minute enforced cooldown; thermal-zone temperature and per-core MHz recorded in every result; a run flagged as suspect if throttling is detected.
- **Turbo variance.** P-cores boost to 4.8–5.0 GHz, E-cores to 3.7 GHz. Under all-core load the achievable boost drops. Countermeasure: report the baseline as **three runs with variance**, not one number. A baseline that cannot be reproduced within ±10% is an anecdote.
- **Host noise.** Compilers, browsers, and editors on the same machine steal cycles. Countermeasures: the cpuset makes the container's cores exclusive; k6 keeps its own cores; the runner warns if host load is high before starting.
- **Loopback softirq.** Network processing for loopback traffic may land on either side's cores. Accepted as a small constant present in both baseline and post-optimization runs, therefore cancelling in comparisons.

### 6. Results

Every run writes `bench/results/<git-sha>-<utc-timestamp>.json` recording: git sha, binary, container limits, cpuset, host load at start, VU count, RPS, p50/p95/p99, error rate, peak temperature, whether throttling was observed, and pass/fail. `compare.sh` diffs two result files and flags regressions beyond a threshold. Run records are the artefact that makes "we improved umbral" a claim with evidence rather than an assertion.

## Phasing

- **Phase 1 — the first trustworthy baseline.** Silo, `bench-prod`, session pool, `warmup.js`, `hold.js`, results store. Ends with a number. This is the deliverable that unblocks everything else.
- **Phase 2 — the boundaries.** `search.sh` (max sustainable VUs) and `saturation.js` (max RPS). Ends with the gap between current and 25,000 RPS quantified.
- **Phase 3 — optimization.** Wherever the instrumented harness says the time is going. Not scoped here; its shape depends on Phase 1 and 2 findings.

Phases 1 and 2 are measurement and must not change framework behaviour. If a Phase 1 or 2 task requires touching `crates/` or `plugins/`, that is a signal the rig is wrong, not the framework.

## Open questions

1. **Which JWT feature, and when.** `umbral-auth` has no JWT support (`BearerAuthentication` is DB-backed token lookup). A session-authenticated baseline does not need it, so Phase 1 does not depend on it. It remains agreed work because stateless API auth is a genuine framework gap, and it becomes relevant if we later want a challenge-shaped auth comparison. **Resolution needed:** schedule it in Phase 3 or treat it as independent.
2. **Port and harness coexistence.** The rig needs a port that cannot clash with a developer's dev server. **Resolution needed:** confirm which ports are reserved before implementation.
3. **`search.sh` budget.** A full limit search is several 5-minute holds plus cooldowns — plausibly 60–90 minutes of wall clock. **Resolution needed:** confirm the acceptable search budget, or cap the doubling range.

## What this spec deliberately does not do

- It does not delete or rewrite `harness/frontend`, `harness/design`, the SSE stream, the framework lab, or the telemetry seams. Those are the diagnostic half and they are working.
- It does not change `harness/backend/src/loadgen.rs`. The in-process generator stays for convenience, but the spec forbids quoting any number it produces as a performance result, and the docs must say so.
- It does not touch `crates/` or `plugins/` at all. The output of this work is a measurement, and a measurement that changed the thing it measures would be worthless.
