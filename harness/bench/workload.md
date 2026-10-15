# The bench workload — the mix, and why

The k6 scripts under `harness/bench/k6/` do not hammer an endpoint and call it a workload. They run a shaped traffic mix for a fixed hold, and it is that mix — how often a feed is read, how often a post is opened, how often something is written — that decides what every p95, p99 and RPS number out of this rig *means*. It is a deliberate choice, documented here and changed only by explicit decision (spec locked decision #7), never as an accident of whatever a script happened to default to.

The mix lives in exactly one place in code: the `THE MIX` block of `k6/lib.js`. `warmup.js`, `hold.js` and the saturation script all call the same `doIteration`, so two scripts cannot disagree about what "the workload" is. This file is the record of the *decision*; `lib.js` is the implementation of it.

## The mix

The three probabilities are the operator's decision, and it has been made: **ratified 2026-10-14 at the values below**, which are the twelve-dollar challenge's shape (a read-dominant feed with engagement on a minority of iterations — at a rate deliberately far above a real user's; see *What one iteration is*). They are what `lib.js` ships, and every number this rig reports is a number about them. A run's own copy of the mix travels inside its `--summary-export` (`setup_data.mix`), so a result can always be read back against the workload that produced it.

| Knob | What it decides | Value |
|---|---|---|
| `LIKE_P` | probability that an iteration also likes a post | `0.15` |
| `POST_P` | probability that an iteration also creates a post | `0.02` |
| `PAGE_P` | probability that an iteration's second read is the HTML profile page instead of a single post (the JSON feed is read by every iteration either way) | `0.10` |

All three are independent: `PAGE_P` chooses which second read happens, and `LIKE_P`/`POST_P` then fire at exactly their stated rate, whatever `PAGE_P` is. Think time is not one of the three, but it is part of the workload and just as load-bearing: one gap of 3–8 seconds per iteration (`THINK_MIN`/`THINK_MAX`, in milliseconds), which is what makes this a browsing shape rather than a saturation hammer. Disabling it (`THINK_MAX=0`) is documented for exactly one caller — the saturation test — and a run records the think time it used alongside the mix, so a hold that somehow ran without it cannot hide.

**What a change would mean.** Raising `LIKE_P` or `POST_P` moves the load onto the write path, where every request is an authenticated transaction against the same SQLite file the reads are reading (WAL commit, one writer at a time): the same request count, a materially different cost. Raising `PAGE_P` trades a `post` read for a template render and its data assembly. Lowering think time raises the offered request rate for the same VU count, which is exactly how the previous harness's `sleep(0.01)` turned every measurement into a saturation test. Because the mix defines what "fast" means, **a result is only comparable to another result with the same mix and the same think time** — Task 8's `compare.sh` refuses to diff two runs whose mix differs, and this table is where the ratified values live.

## What one iteration is

Every iteration reads the feed, thinks, performs one deeper read, and then may react with a write. The deeper read and whether the reaction happens are what the mix above decides, and each request draws its own id — the like's post is a freshly drawn random one, not necessarily the post just read (the challenge's script draws them independently too).

| # | Request | What it exercises | Rate per iteration |
|---|---|---|---|
| 1 | `GET /api/feed` | the batched read: 25 posts with authors, like and comment counts, ordered by `created_at DESC` | 1.00 |
| 2 | `GET /u/{id}` | the HTML profile page — template render, aggregate counts | `PAGE_P` |
| 2 | `GET /posts/{id}` | one post's HTML page — template render, author join | `1 − PAGE_P` |
| 3 | `POST /api/posts/{id}/like` | engagement write: an existence check then an insert, on a fresh id | `LIKE_P` |
| 4 | `POST /api/posts` | a write: insert a post row | `POST_P` |

Every request carries a pooled session cookie, reads included: the session middleware performs its real store lookup on each one, and that lookup is part of the honest cost of a `DbStore` deployment (spec locked decision #3). VUs are pinned to one pooled session each — **the pool holds one session per dataset user (20,000), shuffled at mint, so the identities a run activates are a random sample of the user base rather than always the lowest ids** — so a VU's traffic is one coherent user rather than a new identity per call. The preflight enforces the part that matters for measurement: with a pool smaller than the VU count, `(VU-1) % pool.length` would hand each identity to several concurrent VUs, shrinking the session working set inside the measured database time and flattering exactly the number a session-store comparison exists to produce. That check runs without a request and is **not** skippable by `PREFLIGHT=0`, and a failing pool aborts the run (k6 exit 107) before a single iteration. The pool's size travels in every result (`session.pool_size`), and `compare.sh` refuses to diff two results across a change in it.

With the ratified values that is **2.17 requests per iteration — 2.0 reads and 0.17 writes, a 92.2% / 7.8% read/write split by request count** — spread over one 3–8 s think (mean 5.5 s), so ≈0.39 requests per second per VU before latency is counted: ≈986 RPS at 2,500 VUs in the steady state, less in practice because server latency adds to the think time.

**What that models, honestly.** The shape is a social feed being browsed: reads dominate, and the writes that do happen are on the contended path (one SQLite writer at a time). The per-user *rates* are not observed user behaviour — at these probabilities a browsing VU likes something roughly every 37 seconds and posts roughly every 4.6 minutes, far more engagement than a real user generates. The write rate is a deliberate stress; read it as a rate this workload imposes, not one it observed.

Each id in rows 2–4 is drawn independently and uniformly from the served dataset's ranges (users `1..USERS`, posts `1..POSTS`), not from the feed's newest 25 posts. That spreads the read working set across the table and index rather than measuring one hot page, and it makes the id ranges a correctness property of the run: an id past the last seeded row is a 404, k6 counts a 404 as a failed request, and a hold whose reads are missing would fail its error threshold while measuring the miss path. `setup()`'s preflight refuses to start a run whose ranges do not exist in the served dataset.

## The run's shape

| Stage | Duration | What happens |
|---|---|---|
| ramp | 60 s | 0 → `VUS` |
| hold | `duration` (5 m for a hold, 2 m for the warm-up) | at `VUS` |
| ramp down | 30 s | `VUS` → 0, `gracefulRampDown: 10s` |

So a hold is a **6.5-minute wall clock**, and the 5-minute hold is a real 5 minutes — the ramp precedes it rather than eating into it. The thresholds and the summary's p95/p99/RPS are taken over the whole run, ramp and drain included, which is a mild average of the run rather than of the steady state alone (the challenge this rig calibrates against behaves the same way). An unscored run (`warmup.js`) has no thresholds at all, so it exits 0 whatever its summary says; pass/fail is the exit code of a scored `hold.js` (0 pass, 99 on a failed threshold).

## Running it

```bash
# The silo must already be serving the database you measured against.
harness/bench/run.sh serve
harness/bench/run.sh verify          # envelope first: a number from an unverified cpuset is not a number

# The session pool — once per dataset, against the SERVED database, one session
# per dataset user (20,000), shuffled so each run's active set is a random
# sample of the user base. The seeder and the script must be the same size (the
# script refuses when they visibly disagree); both halves are idempotent, and
# `auth_user` keeps its row count throughout — the seeder gives the dataset's
# own rows the pool password, it creates no users.
docker exec umbral-bench /app/umbral-bench-prod seed-sessions
harness/bench/seed-sessions.sh 8114 20000

# Warm-up (unscored, 1000 VUs, ~3.5 min).
taskset -c 8,9,10,11 ~/.local/bin/k6 run -e BASE=http://127.0.0.1:8114 \
  harness/bench/k6/warmup.js

# A hold (scored, VUS from the limit search, ~6.5 min). The id ranges must match
# the rows the silo serves — add -e USERS=20000 -e POSTS=200000 for the
# development-scale dataset, or the preflight will refuse the run.
taskset -c 8,9,10,11 ~/.local/bin/k6 run -e BASE=http://127.0.0.1:8114 -e VUS=2500 \
  -e USERS=20000 -e POSTS=200000 \
  --summary-export=/tmp/hold-1.json harness/bench/k6/hold.js
K6=$?                                # k6's OWN status, captured immediately: the
echo "exit=$K6"                      # recorder classifies 0/99/107, and anything
                                     # between here and record.sh would clobber $?
```

Then record it — `results/README.md` has the full flow, including the thermal
sampler that has to bracket the run — passing the captured code, never a bare
`$?`:

```bash
harness/bench/results/record.sh /tmp/hold-1.json 2500 20000 "$K6"
```

Three rules decide whether a run is worth anything, and the preflight checks all of them before the first request:

- **`USERS`/`POSTS` must match the served dataset, not the mix.** They are the id ranges, and they default to the challenge's quoted scale (50,000 users / 500,000 posts). A development-scale database (spec §2a's 20,000 users / 200,000 posts) needs `-e USERS=20000 -e POSTS=200000`. A run whose ranges disagree with the rows is not a slower run, it is a different, cheaper run.
- **The session pool must hold at least one session per VU.** The pool holds one session per dataset user (20,000 today), shuffled; a pool smaller than the run's VU count shares identities between concurrent VUs, which is what the preflight refuses (107) before any load. `PREFLIGHT=0` does **not** skip this one — it is knowable from two numbers, no request needed.
- **The session pool must have been built against the served database.** Sessions are rows in the database the server opened, so cookies minted against another file authenticate nothing — the failure reads as the server rejecting writes under load, which it is not. With writes in the mix the preflight calls `GET /api/auth/me` with the first pooled session and refuses to run on a 401.

The preflight spends a handful of requests (`/api/ping`, `/api/auth/me`, the tops of the two id ranges) and its failure messages name the fix. `-e PREFLIGHT=0` skips the server probes — the ids are then on their own honour. It costs a few requests inside a run that will issue hundreds of thousands of them, and it never touches the thresholds.

k6 is pinned to cores 8–11 (`taskset`) so it never shares a physical core with the container, which owns 0, 2, 4 and 6. Each VU holds a socket, so a run needs a file-descriptor limit well above the VU count — `ulimit -n` is recorded with every result so a client-side `EMFILE` is never mistaken for a server ceiling.

## The first measurement under this mix

These values were measured for the first time on 2026-10-14, at 2,500 VUs against a 20,000-user / 200,000-post dataset in the silo: **≈865 RPS, p95 3.3 ms, p99 4.8 ms, zero failed requests, a 0.10% run-to-run spread over three holds**. The three results, what they do and do not include, and the traps in reproducing them are in `README.md`; the raw records are in `results/`. **They were measured with the pre-fix 200-entry session pool** — 12–13 concurrent VUs per identity — so they are not comparable to a post-fix result and `compare.sh` refuses them (they carry no `session.pool_size`). A result is only comparable to another result with this mix, this think time and the same pool size, which is why `compare.sh` refuses to diff across a change in any of them — and why the ratified values above are quoted into every result file as `mix`.

## Known properties, and what the number does not include

- **No `Server-Timing`.** `bench-prod` constructs no telemetry, so it sends no `Server-Timing` header; nothing in these scripts reads one, and no trend here is fed by one. The instrumented binary on :8113 does emit it, and the older `harness/k6/*.js` scripts collect it there — that is a diagnostic number, never a performance result.
- **The mix writes, so the run dirties the database.** At the ratified values a 2,500-VU hold issues ≈340,000 requests and writes ≈3,100 posts and ≈23,500 likes over the whole 6.5-minute run — about 2,700 and 20,500 of those inside the 5-minute hold stage itself — with latency trimming all of them slightly. Nothing ever deletes or re-seeds the bench database between runs; the dataset grows, and a baseline is a number about the dataset at the moment it was taken (recorded with the result, per Task 8).
- **Writes require a live pool.** Until the seeder has given the first N dataset users the pool password and `bench/session_pool.json` has been rebuilt against the served database at the ramp's peak size, every authenticated request is a 401; a reads-only smoke run sets `-e LIKE_P=0 -e POST_P=0` and the preflight says so out loud.
- **The preflight's probes are part of the run.** A handful of extra requests land in `http_reqs`, at the top of the ramp rather than inside the hold.
- **The next ceiling is the host's ephemeral port range, not file descriptors.** The container's descriptor limit is 524,288 — the most this host can grant (dockerd's own `LimitNOFILE` caps it there, so more headroom would take a daemon restart) — set deliberately at that ceiling by `run.sh serve` and asserted from inside the container by `run.sh verify`. Ports run out first: `net.ipv4.ip_local_port_range` is `32768 60999`, i.e. 28,232 of them, and k6 holds roughly one connection per active VU, so a hold beyond that count fails to open connections — a client-side shortage that looks exactly like the server refusing load and, like any refused connection, shows up as request timeouts rather than as latency, so it never reaches the percentiles. No `ulimit` fixes it and `run.sh` cannot see it; widening the range is a host sysctl, and therefore the operator's call rather than the rig's.
