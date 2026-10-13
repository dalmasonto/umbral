# harness/backend — the instrumented umbral app

A realistic social-feed app (users / follows / posts / likes / comments) wired with real plugins, plus a telemetry layer that decomposes every request down to **each middleware and each SQL statement**. This document is the map of what is measured, through which seam, and what the numbers do and do not include.

## Surfaces

| Route | What | Why it exists |
|---|---|---|
| `GET /` | feed page (HTML) | template-render workload (the `render` bucket) |
| `GET /u/{id}` | profile page (HTML) | heavy page: counts + recent posts |
| `GET /posts/{id}` | post detail (HTML) | single-row + counts |
| `GET /api/feed` | batched feed JSON | `select_related` + IN-queries — the ORM's intended shape |
| `GET /api/feed-naive` | **unbatched** feed JSON | deliberate N+1: 1 author fetch + 2 COUNTs per post — the detector's test subject |
| `GET /api/users/{id}/profile` | **heavy aggregate** | follower/following/post counts, likes+comments received, recent window with per-post counts, top post (~10 queries) |
| `POST /api/posts` | create post | write path |
| `POST /api/posts/{id}/like` | like (idempotent) | write path with unique-constraint shape |
| `POST /api/users/{id}/follow` | follow (idempotent) | write path |
| `GET /api/ping` | tiny JSON | no-DB baseline ("static" scenario) |
| `GET /api/bench/telemetry` | rolling aggregates | feeds the Layers view |
| `GET /api/bench/middleware` | **every middleware**, registry + measured | the "expand middleware" data |
| `POST /api/bench/runs` | start a load run | `{"scenario","vus","duration_s"}` — failures are captured per kind: status, path **template**, count and a response-body sample (`error_breakdown` in the run summary; new kinds also broadcast live over SSE) |
| `GET /api/bench/runs[/{id}]` | run records (sampled per second) | feeds Overview / Runs views |
| `GET /api/bench/runs/{id}/telemetry` | **the layer profile of ONE run** | requests the run generated carry `x-bench-run: <id>`; the ledger/middleware/routes/statements for just those requests (`captured:false` for runs recorded before this existed). Runs are serialized (one at a time) so a run's SQL window — diffed from global counters — stays clean; statement `max_us` is an upper bound when other traffic is present |
| `GET /api/bench/stream[?run=N]` | **SSE** sample stream | the Live view |
| `POST /api/bench/reset` | zero the aggregates | between experiments |
| `GET/POST /api/bench/lab` | framework-lab controls | `fake_middleware_us` (sleeps inside a real middleware at order −70) and `rate_limit_per_sec` (per-user token bucket that short-circuits with 429); both default off — see `src/lab.rs` |

Identity for scenarios: the `x-bench-user: <id>` header (bench-only middleware `DevAuth` — no DB access, so it doesn't distort layer measurements; real auth plugins are also installed).

## How the decomposition is measured

Request path (outer → inner, per `App::build`): core edge layers → typed `MiddlewareStack` → plugin `wrap_router` layers (topo order) → router → handler.

| Bucket | Seam |
|---|---|
| **middleware** | app middlewares self-time their own before/after (`TraceRoot`, `DevAuth`, `request log`, `response stamp`); **plugin `wrap_router` layers** are bracketed by marker plugins (`ProbePlugin`) placed via `Plugin::dependencies()`, so `security` and `sessions` get their own rows; the registry (`GET /api/bench/middleware`) lists every middleware the framework knows about, including ones not individually timed |
| **routing** | from the innermost marker to the handler's first statement (route match + extractors + core router) |
| **query build** | handler wall around ORM awaits **minus** the sqlx-reported execution time - SQL build + pool checkout + decode overhead (reported as 0 in SQLite's `orm_wall` mode, where the split is unknowable rather than zero) |
| **database** | two honest modes: where the driver emits statement events **within the request's context** (Postgres) it is the sum of sqlx-reported execution times for *this* request (the same seam the test-side query counter uses); on **SQLite** sqlx executes off the request task, so per-request attribution is impossible - the bucket is then the handler's own wall around ORM awaits, and the `Server-Timing` `desc` says so (`"11 queries"` vs `"orm wall, statements global"`), as does `db_source` in the JSONL line |
| **serialize / render** | timed inside the handler around payload assembly / `templates::render` |
| **handler logic** | handler wall minus the above (loop/format work) |
| **tail** | everything on the way out not otherwise attributed (edge-layer after-work, short-circuits) |

Every response also carries:

```
Server-Timing: total;dur=41.2, bench-trace-root;dur=0.1, ..., db;dur=13.9;desc="7 queries", handler;dur=21.3
X-Bench-Trace: 1284
```

and a JSON line lands in `traces/requests.jsonl` (writer thread, off the request path — the request never blocks on I/O).

### Known limits (read before trusting a number)

- **Outside the measured window:** the core edge layers that wrap *everything* (trace span, hardening headers, body limit/timeout, host guard, CORS, compression, panic/render-500, route-context, slash-redirect). They run before the first app middleware and after the last — their cost lands in `total` but not in a bucket. In Dev they're near-free; in Prod the host guard and compression are the ones to watch. Making them individually visible is exactly what the blocked productization task (#675) is about.
- **Per-statement SQL data:** fingerprints, per-statement avg/max and the slow list are **global** (process-wide) on every backend; exact per-request SQL lists are Postgres-only, and SQLite gets a 250 ms burst heuristic for the N+1 detector instead. The global views need sqlx statement events enabled - the harness sets `UMBRAL_DB_LOG_STATEMENTS=debug` for itself at startup (the framework knob added alongside this harness); an operator override is respected.
- **In-process load generation:** the VUs run as tokio tasks in the same process (loopback HTTP). Generator CPU is small but nonzero; the design's "separate process" rule applies once k6/oha drivers land (see `docs/decisions/2026-08-08-fuzz-upgrade-load-tests.md`).
- **Short-circuits** (CSRF 403, 404s) produce partial stamp sets; their cost shows up in `tail`, and their per-plugin rows may read 0. Only successful traffic feeds the interesting numbers.
- **`DevAuth` is not production auth.** It reads a header; nothing else. Its purpose is to make "authed" scenarios cheap and honest.

## Seeding

**Resumable per entity** — growing the dataset is a restart away: users/posts top up to the configured volume, users with no follows/likes yet get theirs, and comments cover the posts created by that run (existing rows and activity are kept; the 30k demo shape is `UMBRAL_BENCH_USERS=30000` → 30k users / 300k posts). Deterministic (xorshift64, fixed seed). Volumes via env:

```bash
UMBRAL_BENCH_USERS=50000 UMBRAL_BENCH_POSTS_PER_USER=10 UMBRAL_BENCH_LIKES_PER_USER=25 \
  cargo run --bin umbral-bench-instrumented -- serve   # 50k users / 500k posts — the design's target shape
```

Defaults: 2,000 users · 10 posts/user · 25 likes/user · 8 follows/user · ~60% of posts get 1–2 comments.

## Running for real numbers

```bash
cargo build --release --bin umbral-bench-instrumented
cargo run --release --bin umbral-bench-instrumented -- migrate
cargo run --release --bin umbral-bench-instrumented -- serve
```

`[profile.dev] debug = "line-tables-only"` keeps the target dir (and the disk) sane; the dev profile still runs correctly, just slower — never quote dev-profile latency as a result.

## Where the frontend reads from

`harness/frontend` (React + Vite, port 5173; CORS is configured for it) consumes the `/api/bench/*` endpoints above and the SSE stream. The design source of truth for those views is `harness/design/`.
