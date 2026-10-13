# harness — benchmark & profiling harness for umbral

A standalone bench rig that lives OUTSIDE the framework workspace (excluded in the root `Cargo.toml`, same as `examples/`): it consumes umbral exactly the way a real app does, measures it, and shows where the time actually goes.

```
harness/
  design/    # the UI design export (design source of truth; ports to frontend/)
  backend/   # the instrumented umbral app: APIs + pages + per-layer telemetry + load generator + SSE
  frontend/  # React + Vite app that presents runs live (SSE) and historically
  k6/        # optional external drivers: stateful persona journeys + single-endpoint hammers
```

Workloads come in two shapes, on both drivers: **journeys** (`readers` /
`community` — personas browse, linger, open profiles, occasionally write,
with think time) and **single-endpoint hammers** (`feed`, `write-heavy`,
`profile`, `static`). Run records persist per run in `traces/runs.jsonl` and
are rehydrated on boot, so history survives restarts. The Live view also has
a **framework lab** (inject latency into a real middleware / switch on a
429-returning rate limiter) for watching what a middleware costs; k6 usage
is documented in `k6/README.md` (k6 is a user-local install — `harness/k6/run.sh`
resolves it).

## Why it exists

`planning/review_4/performance.md` closed with an honest caveat: the sweep was static — no measurements. This harness is the measurement half. It answers, with numbers:

1. **How fast is umbral, really?** — on SQLite and Postgres, under read-heavy and write-heavy load, at rising virtual-user counts.
2. **Where does the time go?** — every request decomposed into middleware / routing / query build / database / serialize / render — and inside middleware, **every middleware individually** (the framework's own `wrap_router` layers bracketed by marker plugins, the app's typed middlewares self-timed, SQL captured from sqlx's own tracing events).
3. **Which layer is the bottleneck?** — the attribution is served as `Server-Timing` headers, JSONL traces (`traces/requests.jsonl`), rolling aggregates (`GET /api/bench/telemetry`), and an N+1 detector that flags repeated statement fingerprints per request.

The measurement seams (and their limits) are documented in `backend/src/telemetry.rs` and `backend/src/probes.rs`. Nothing in the framework is patched to make this work — it uses the documented `Plugin::wrap_router` / `Plugin::dependencies` / `Middleware` contracts, which is exactly the validation the blocked in-app dev-tool task (#675) is waiting on.

## Quick start

```bash
cd harness/backend
cargo run -- migrate          # schema
cargo run -- serve            # boots on 127.0.0.1:8113, seeds on first run

# in another terminal — watch the layers:
curl -s localhost:8113/api/feed | head -c 400
curl -s localhost:8113/api/bench/telemetry | jq '.layers'
curl -si localhost:8113/api/feed | grep -i server-timing

# start a 30-second run at 200 VUs, then stream it (SSE):
curl -s -X POST localhost:8113/api/bench/runs \
  -H 'content-type: application/json' \
  -d '{"scenario":"feed","vus":200,"duration_s":30}'
curl -N localhost:8113/api/bench/stream?run=1

cd ../frontend && npm install && npm run dev   # the UI at :5173
```

Load-test the OTHER backend by pointing the same harness at Postgres:

```bash
UMBRAL_DATABASE_URL=postgres://localhost/umbral_bench cargo run -- serve
```

(The run records store the backend label, so sqlite vs postgres comparisons stay honest.)
