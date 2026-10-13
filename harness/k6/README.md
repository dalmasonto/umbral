# harness/k6 — external load drivers

The in-process generator (`harness/backend`, the Live view) and these k6
scripts drive the same workloads. Use in-process for convenience and SSE
visualisation; use k6 for high virtual-user counts and for keeping the
generator's CPU off the server's.

k6 is **not required** for the harness — these are the optional external
drivers. The backend's own telemetry (layers, middleware, statements)
captures everything either way; k6's `http_req_duration` at 5–10k VUs is
the honest end-to-end number the in-process generator can't produce on the
same machine.

## Install

```bash
# any of:
sudo apt install k6            # Debian/Ubuntu (Grafana repo)
brew install k6                # macOS
docker run --rm -i --network host grafana/k6 run - < journeys.js
```

## Scripts

| Script | Shape |
|---|---|
| `journeys.js` | Stateful personas — reader / writer / lurker walk multi-step sequences with think time (browse → linger → profile → occasionally post). The "users do different things at different times" workload. |
| `single-endpoints.js` | One route hammered per scenario (feed / feed-naive / profile / writes / static) — the "how fast is this endpoint alone" question. |

Both send `x-bench-user: <random id>` (the harness's bench identity), tag
requests by **path template** (so `/api/users/123/profile` reports as
`/api/users/{id}/profile`), and collect the server's `Server-Timing`
header into `server_total_ms` / `server_db_ms` trends — so k6's summary
shows the server-side decomposition next to the end-to-end latency:

```
http_req_duration .....: avg=12.3ms  p(95)=41.2ms  p(99)=118ms   ← end-to-end
server_total_ms .......: avg=9.8ms   p(95)=32.1ms  p(99)=91ms    ← server only
```

## Run

```bash
# against the harness backend on :8113, 30k-user dataset
k6 run journeys.js

# parameterised
BASE=http://127.0.0.1:8113 USERS=30000 \
READERS=30 WRITERS=6 LURKERS=10 DURATION=2m \
k6 run journeys.js

# one endpoint, hard
ONLY=profile VUS=300 USERS=30000 POSTS=300000 k6 run single-endpoints.js
```

The built-in thresholds are the harness budgets — a k6 exit code of 0
means the run passed: `p(95)<500 ms`, `p(99)<1000 ms`, `failed<1%`.

## Notes

- k6 runs are **not** written into the app's run registry (that belongs to
  the in-process generator). The server-side view of a k6 run is still
  complete: `GET /api/bench/telemetry` and `traces/requests.jsonl` see every
  request. Recording external runs into the registry is on the v2 list.
- For Postgres-vs-SQLite comparisons, point `BASE` at two backend instances
  (they are different processes with different DBs) and keep the same k6
  arguments — `harness/backend/README.md` documents the per-backend boot.
