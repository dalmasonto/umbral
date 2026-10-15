# Bench rig v2 — the layer ledger under load (SQLite, diagnostic)

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Commit:** `219e1e29` · **Run tag:** `990001` · **Raw captures:** §Raw captures

---

## What this is — and what it is not

**This is diagnosis, not measurement.** Phase 3's cycle rule opens every cycle with where the time is going — the instrumented binary's layer ledger for the same workload the measured binary runs — so the bucket a cycle targets is named from evidence instead of guessed. No ledger under load had ever been captured.

The instrumented binary carries telemetry in the request path by construction (`TraceRoot` probes, Server-Timing, JSONL writer). **No latency from this run is quotable as a performance result**, nothing here goes through `record.sh`, and the numbers below are **proportions**. For what the app actually delivers at this operating point, read the SQLite baseline (`reports/2026-10-14-baseline-sqlite.md`); that is the measured binary's job.

**Exactly one run was made, as dispatched.** The first capture attempt aborted in the pre-run gate *before k6 started* — the gate tested `GET /api/auth/me` with a pooled cookie, which the instrumented binary's `CookieStore` mount cannot authenticate (see the seam note below). No run was spent. The gate and the k6 `preflight()` were corrected to assert the instrumented binary's real identity seam, and the run was then made once. k6 exit code: **0**; checks **342 924 passes / 0 fails**; every route records 0 errors.

## Envelope (verified live, mid-run, by `docker inspect`)

| | |
|---|---|
| Container | `umbral-bench-instrumented`, id `96ab48218d9978eaf64f6888806963f25c3c38c46af638ef104d4f005b05f892`, started `2026-10-14T10:45:04Z` |
| Image | `umbral-bench-instrumented-diagnosis:latest` = `sha256:77bd8eeab6247695cf57545bb643cafa5c09ee6064997eb13512ccb2b37d2d49`, built `2026-10-14T13:44:57+03:00` `FROM umbral-bench:latest` + `COPY target/release/umbral-bench-instrumented /app/umbral-bench-prod` — the rig's own image, only the binary swapped, so templates/migrations/ENTRYPOINT/CMD are the rig's |
| Binary | `/app/umbral-bench-prod` inside the running container, sha256 `e5dad51ec5e234813e2a7f0141c13c91c48e3657b01f9ff1002953b59a380a07` (hashed in-container) |
| Cpuset | `--cpuset-cpus=0,2,4,6`; `cpuset.cpus.effective` read inside the container: `0,2,4,6`; `nproc` 4 |
| Limits | `--cpus=4` (NanoCpus 4000000000), `--memory=4g --memory-swap=4g`, `--ulimit nofile=524288:524288`, `--network host`, `/data` bind mount |
| k6 | external, `taskset -c 8,9,10,11`, started `2026-10-14T10:49:41Z`, 2 500 VUs |

`run.sh` was not edited and its envelope was not changed; the container was started with the same flags `run.sh serve` uses.

## Workload and the seam adaptation

Identical to the measured recipe: `USERS=20000`, `POSTS=200000`, `VUS=2500`, mix `LIKE_P=0.15`, `POST_P=0.02`, `PAGE_P=0.10`, think 3000–8000 ms, 60 s ramp + 5 m hold + 30 s ramp-down. Workload parity is observable: **158 088 iterations here vs 158 190 in the measured baseline (0.06%)**, and 342 928 k6 requests vs 343 375.

`harness/bench/k6/lib.js` + `hold.js` were copied to `/tmp/partb-k6/` with a minimal, marked diff (`/tmp/partb-k6/lib-js.diff`, 83 diff lines): every workload request carries `x-bench-user: 20001 + ((__VU-1) mod 200)` — the DevAuth seam, identity for write handlers — and `x-bench-run: 990001`, the telemetry run tag. `hold.js` is byte-identical to the repo copy. The measured binary reads neither header.

**Seam note (why identity works this way).** Under the instrumented binary's `CookieStore` mount, cookie login cannot authenticate at all in this tree: `login_user_id`'s in-request fast path sets a raw rotated UUID token as the cookie, the session layer's exit-time store write is skipped when a handler-set session cookie is present, and `CookieStore::load` can never decrypt a raw token (it returns `Ok(None)`). Under `bench-prod` (`DbStore`) the same pool authenticated 100% of writes (Part A). The instrumented workload therefore drives writes through DevAuth and reads through public routes; the pooled cookie still rides on every request so the request shape matches the measured run.

## The ledger

Per-run aggregate for tag `990001`: **N = 342 925 requests** — exactly the tagged workload population. (The global aggregate reads 342 930; the delta of 5 is this session's own untagged probes/readouts. Reconstructing the same window from the 343 145-line JSONL yields 342 927 — the extra two are k6's untagged `rangeMiss` preflight probes, one `/u/{id}` and one `/posts/{id}`.)

The eight layers are an **exact partition** of request time: middleware + routing + (handler = query_build + database + serialize + render + handler_logic) + tail. Verified in the capture: layer counts all equal N, and the count-weighted mean route total equals the layer sum to 0.00 µs.

| layer | avg µs | share | what it is |
|---|---|---|---|
| **database** | **905.8** | **83.26%** | the handler's own wall around ORM awaits (see "cannot see" #2) |
| middleware | 68.0 | 6.25% | all tower middleware, before+after (decomposed below) |
| tail | 43.0 | 3.95% | edge layers' after-work, short-circuit residue |
| serialize | 39.8 | 3.66% | JSON serialization inside handlers |
| render | 20.2 | 1.85% | template rendering |
| routing | 7.0 | 0.64% | route matching + extractors |
| handler_logic | 4.2 | 0.38% | handler code outside DB/serialize/render |
| query_build | 0.0 | 0.00% | 0 by construction on SQLite (see "cannot see" #2) |

**The headline: ~83% of a request is database wall time.** Routing is noise (0.6%); middleware is ~6%; the serialization/render work the framework does around the DB is ~5.5% combined. If a Phase 3 cycle is aimed at latency, this ledger says the bucket to target is `database` — not middleware, not rendering.

### Per-route breakdown

Full-population means over the run window (N = 342 927 as above; route counts here are ±1 of the per-run counts because of the two untagged preflight probes). Layer sums per route match the route totals exactly.

| route | n | total µs | middleware | security | sessions | probe wrap | routing | database | serialize | render | handler_logic | tail |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `GET /api/feed` | 158 088 | 1427.0 | 66.2 | 39.7 | 15.8 | 10.1 | 6.5 | 1202.2 | 86.4 | 0.0 | 2.4 | 63.4 |
| `GET /posts/{id}` | 141 932 | 769.6 | 70.9 | 42.8 | 16.5 | 10.8 | 7.2 | 619.5 | 0.0 | 41.6 | 5.1 | 25.2 |
| `POST /api/posts/{id}/like` | 23 699 | 848.8 | 59.3 | 32.3 | 15.9 | 10.4 | 7.8 | 755.7 | 0.0 | 0.0 | 0.8 | 25.1 |
| `GET /u/{id}` | 16 158 | 1019.7 | 74.1 | 45.6 | 16.7 | 11.0 | 7.5 | 828.5 | 0.0 | 62.3 | 18.3 | 28.9 |
| `POST /api/posts` | 3 050 | 533.5 | 57.3 | 31.2 | 15.2 | 10.2 | 16.7 | 432.8 | 0.0 | 0.0 | 2.0 | 24.7 |

Read of the matrix:

- **The feed is the workhorse**: 46% of all requests, the most expensive shared route (1 427 µs avg), and **84% of its time is database**. It is the JSON read path, so it also pays the ledger's only real serialize cost (86 µs — the largest non-DB, non-framework single line in the table).
- **The HTML page read** (`/posts/{id}`) is cheaper (770 µs) and pays render instead of serialize (42 µs render).
- **The write mix is surprisingly DB-heavy per request** — a like is 849 µs with 89% of it database (756 µs), more per-request DB time than the cheaper reads. The isolation/no-read-before-write path is already doing its job at the framework level; this is the SQLite write transaction itself.
- **`POST /api/posts` (create) is the cheapest route per request** (534 µs) — a single insert, no read amplification.
- `GET /u/{id}` is the most expensive *per unit of framework work* (1 020 µs with 18 µs handler_logic and 62 µs render): a fan-out read.
- Routing is ≤ 3% everywhere, and on the common routes it is 0.5–0.9%. **No cycle should target routing or middleware dispatch.**

### The middleware row

The middleware rows sum exactly to the `middleware` layer (68.0 µs): typed middleware (dev auth / request log / response stamp / bench lab / api rate limit) plus the three probe-derived buckets.

| component | avg µs | share of request | max µs |
|---|---|---|---|
| security (headers + CSRF) | 40.6 | 3.74% | 4098 |
| sessions | 16.1 | 1.48% | 3549 |
| bench probe (wrap) — *instrumentation* | 10.5 | 0.96% | 3306 |
| response stamp — *instrumentation* | 0.39 | 0.04% | 358 |
| dev auth (header) — *instrumentation* | 0.19 | 0.02% | 708 |
| request log — *instrumentation* | 0.09 | 0.01% | 462 |
| api rate limit | 0.01 | ~0% | 632 |
| bench lab (injectable) | 0.01 | ~0% | 512 |

The instrumentation's own cost inside this binary is therefore ≈ 11 µs, ~1% of the request (probe wrap + stamp + auth + log). The five non-instrumentation components are what prod would also pay *in this configuration*. Per route, security costs 31–46 µs and sessions 15–17 µs (route differences track body/header sizes and the response path, not logic).

## What this ledger cannot see

1. **The session store is the wrong one, deliberately (ruling 15).** The instrumented binary mounts `CookieStore` while `prod` mounts `DbStore`. **Prod is the truth; the ledger is a model with that one deliberate omission.** Concretely: the 16 µs `sessions` bucket here is a CookieStore load *attempt* — a base64url decode of the pooled UUID that fails AEAD and yields a fresh anonymous session mint — **not** prod's per-request session-read round trip against the database, which would add DB-backed time to the `sessions` bucket in prod. Read every `sessions` figure in this report as a floor, not prod's cost. (Related, same root: this binary's cookie login cannot authenticate at all — see the seam note.)
2. **On SQLite there is no per-request SQL attribution, by design.** Every JSONL record carries `db_source: "orm_wall"`. The `database` bucket is the handler's own wall around ORM awaits — it includes connection checkout, decode, and any wait — and `query_build` is 0 by construction. Per-statement SQL times exist only in the global SQL view (gaps7 #176/#177). **"83% database" means "83% waiting on the ORM's own wall", not "83% inside SQLite executing".**
3. **The instrumentation is in the path.** These proportions were measured with the measurement attached. The §middleware-row estimate (~1%) is itself produced by the same probes it measures; treat it as a floor, and never quote a latency from this run.
4. **These are means over the full population; the p50/p95/p99 figures in the captures are ring-based (last 4 096 requests per route) and describe the run's *end*.** The feed shows it: avg 1 427 µs vs ring p50 1 725 µs — the run got slower toward its end, consistent with the served database growing during the run (the workload added ~3 050 posts / ~23 700 likes; the measured baseline grows the same way). Do not read route p50/p95/p99 as whole-run percentiles.
5. **The identity seam is diagnostic-only.** Writes here authenticate via `x-bench-user` at ~0.2 µs; prod's writes authenticate via the session middleware (bucket 1). Another reason to read the like/create rows as *lower bounds* on prod's per-request cost.
6. **One run.** No variance is estimable from this ledger; it is a proportion model, not a distribution. (The rig's own host-noise findings apply to any *re-run* of this ledger too.)

## Raw captures

All on the recording machine; none of these are `record.sh` records (this was diagnosis).

| file | size | sha256 |
|---|---|---|
| `/tmp/partb-run.json` (per-run telemetry, tag 990001) | 2 154 B | `41d1a3e555273b89ae3f53a9683fcc057aaf095af9ca082a2231e869301dd784` |
| `/tmp/partb-global.json` (global telemetry) | 2 562 B | `02ed3c0903319ebfa593da2ad5bac0104e25347cd5bfb6dc304b3d7785b1db26` |
| `/tmp/partb-mw.json` (middleware inventory) | 1 276 B | `55f38090d4c2923e8cb1278fa26186da2b5bd8a3865f90ed2f4741cd398068a9` |
| `/tmp/partb-summary.json` (k6 summary export) | 4 774 B | `2428464478945b94d35a368ea93f86921db5ae05385b345e6f4ceb3141e7692c` |
| `/tmp/partb-requests.jsonl` (per-request JSONL, 343 145 lines) | 273 523 057 B | `35ba1650b3c5043b26436bbd12f2ac4aa1fd368ebce0c6e412cd65d2b7910fa5` |
| `/tmp/partb-inspect.json` (container inspect) | 9 705 B | `3975b61f76c31df6c97764eb8065697976915ec2b1b1612482d171863022423e` |
| `/tmp/partb-container.log` (15 lines) | 2 327 B | `9020218878a466baa6a7e2255f9bbc3a7d01cbb49319e7fb00cc83131bba692c` |
| `/tmp/partb-k6/lib-js.diff` (the workload diff) | 83 diff lines | `24415433b66bf4e5e9cb4b1438aea795520799c9b1fabc17cba5753110d90648` |

The small JSON captures were also copied into `harness/bench/results/` (the rig's gitignored local store, alongside the hold records) as `partb-ledger-*.json`. The 261 MiB JSONL stays in `/tmp` with its sha256 above; it can be re-extracted from the (stopped, not removed) container with `docker cp umbral-bench-instrumented:/app/traces/requests.jsonl .`.

## Reproduction

1. `harness/bench/run.sh build` — the rig's image (prod binary at `/app/umbral-bench-prod`).
2. `docker build -f /tmp/Dockerfile.instrumented -t umbral-bench-instrumented-diagnosis:latest harness/backend` where the Dockerfile is `FROM umbral-bench:latest` + `COPY target/release/umbral-bench-instrumented /app/umbral-bench-prod`.
3. `docker run -d --name umbral-bench-instrumented --cpuset-cpus=0,2,4,6 --cpus=4 --memory=4g --memory-swap=4g --ulimit nofile=524288:524288 --network host -v <worktree>/harness/bench/data:/data -e 'UMBRAL_DATABASE_URL=sqlite:///data/bench-run.db?mode=rwc' umbral-bench-instrumented-diagnosis:latest serve` — same flags as `run.sh serve`.
4. Wait for readiness on `:8114`; `harness/bench/seed-sessions.sh 8114 200` (the pool; it succeeds under this binary as an HTTP flow even though the cookie cannot re-authenticate — only the id mapping 20001+ matters here).
5. Copy `harness/bench/k6/{lib.js,hold.js}` to a scratch dir and apply the marked diff (`lib-js.diff`): `x-bench-user` + `x-bench-run` headers on every workload request; adapt `preflight()`'s write-identity probe to DevAuth.
6. Gate: read 200s, a DevAuth write 2xx, per-run capture probe true, target run tag 404 before the run; then `POST /api/bench/reset`.
7. `taskset -c 8,9,10,11 k6 run -e BASE=http://127.0.0.1:8114 -e VUS=2500 -e USERS=20000 -e POSTS=200000 -e BENCH_RUN=990001 --summary-export=<file> hold.js`.
8. Capture: `GET /api/bench/runs/990001/telemetry`, `GET /api/bench/telemetry`, `docker cp umbral-bench-instrumented:/app/traces/requests.jsonl`.
