# The baseline — the first number this rig produces

**≈865 RPS at 2,500 VUs, p95 ≈3.3 ms, p99 ≈4.8 ms, zero failed requests — with a run-to-run spread of 0.10% on RPS across three consecutive holds.**

This is the first measurement from the fixed-resource silo: `bench-prod` inside a 4-CPU cgroup, under real session authentication, with no telemetry middleware in the measured path, driven by an external k6 on the cores the container does not own. Everything it says *no* to is in the sections below, because a number is only worth its caveats.

**Read it as a demand figure, not a capacity figure.** The mix offers **986 requests/s** at 2,500 VUs — 2.17 requests per iteration ÷ 5.5 s of think — and this run delivered 865 (a run-level rate, with the ramp and the drain in its denominator, so the two are not directly comparable). The run's own counters close the gap: mean in-flight VUs 2,195 × (2.17 ÷ 5.5 s) = 866 requests/s offered against 865.4 delivered, i.e. **99.9% of what the load asked for**; and the mean iteration took 5.502 s against a mean think of 5.500 s, so all ~2.17 requests in an iteration together added **2 ms to a 5.5-second loop**. Umbral kept up with essentially everything thrown at it, and the observed request concurrency — 865 req/s × 1.55 ms mean latency ≈ **1.3 requests in flight**, at most a third of the silo's four CPUs — says the same thing from the other side. **865 is a statement about this workload's think time, not about a limit of the framework's.**

**No earlier number from this repository is comparable to it.** The old harness ran its load generator inside the server process, bypassed authentication with an `x-bench-user` header, and kept instrumentation middleware in the path; its best figure described a 20-thread laptop under a hammer with a `sleep(0.01)`, not umbral under a workload. The instrumented binary on :8113 (which does emit `Server-Timing`) is a diagnostic tool and no number from it is a performance result. This file is the only baseline; when it is superseded, its successor replaces it here rather than accumulating beside it.

## The three runs

All three: `hold.js`, `VUS=2500`, `USERS=20000`, `POSTS=200000`, k6 pinned to cores 8–11, 60 s ramp + 5 m hold + 30 s ramp-down (≈6.5 min wall clock), 180 s idle cooldown before every hold, and the thermal sampler pinned to core 12 bracketing each run (started after the cooldown, stopped when k6 exited).

| # | recorded (UTC) | result file | RPS | p50 | p95 | p99 | max | errors | requests | checks | exit | thermal peak |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 2026-10-14T01:25:28Z | `results/52e800a9-20261014T012528Z.json` | 865.381 | 1.324 ms | 3.284 ms | 4.749 ms | 37.649 ms | 0 | 342,891 | 342,887 / 342,887 | `passed` | 86 °C |
| 2 | 2026-10-14T01:35:09Z | `results/1819ddbc-20261014T013509Z.json` | 865.086 | 1.341 ms | 3.318 ms | 4.747 ms | 44.498 ms | 0 | 343,410 | 343,406 / 343,406 | `passed` | 85 °C |
| 3 | 2026-10-14T01:44:50Z | `results/1819ddbc-20261014T014450Z.json` | 864.536 | 1.399 ms | 3.389 ms | 4.815 ms | 42.951 ms | 0 | 343,073 | 343,069 / 343,069 | `passed` | 79 °C |

Every run held all three thresholds (`p(95)<500`, `p(99)<1000`, `rate<0.01`), k6 exited `0` on all three, and all three are `verdict: pass` with an empty `.warnings[]`.

### The spread

| Metric | min | max | mean | range | spread |
|---|---|---|---|---|---|
| RPS | 864.536 | 865.381 | 865.001 | 0.845 | **0.098%** |
| p50 | 1.324 ms | 1.399 ms | 1.355 ms | 0.075 ms | 5.5% |
| p95 | 3.284 ms | 3.318 ms | 3.330 ms | 0.105 ms | 3.2% |
| p99 | 4.747 ms | 4.815 ms | 4.770 ms | 0.068 ms | 1.4% |
| max | 37.649 ms | 44.498 ms | 41.699 ms | 6.849 ms | 16.4% |

The spread is **far inside ±10%** on every thresholded metric — two orders of magnitude inside it on RPS — so the rig is a ruler at this operating point: differences above ~0.1% in throughput and ~3% in p95 are measurable. `compare.sh` agrees: `results/compare.sh` over any pair exits `0` ("no thresholded metric moved beyond its limit"), with RPS deltas of −0.03% and −0.06% and p99 deltas of −0.04% and +1.43%.

**But the two metrics are not equally sensitive, and the asymmetry governs how every later result must be read.** At this operating point **throughput is pinned by the workload's think time**, not by the server: latency is two to three orders of magnitude below the think, so the run's iteration rate is the rate the think allows (99.9% of it, above). The entire headroom any server-side optimization can buy at 2,500 VUs is therefore the gap between the delivered **865** and the offered **986** — under 1% at a flat 2,500 VUs, and less still once the ramp and the drain are averaged in. **RPS here is a one-sided regression detector**: a server that stopped keeping up would show up as missed requests, failed checks and timeouts at almost unchanged RPS, while a server that got faster would show up as almost unchanged RPS too. **p95/p99 is the metric a Phase 3 win has to move**, and an optimization argued from RPS at this VU count would be structurally incapable of registering its own improvement; `saturation.js` (Phase 2) is the instrument that gives throughput back its sensitivity. Note which terms do *not* appear in the offered rate: dataset scale is not one of them — going to 50,000 users changes the cost of a request, not the rate at which the workload offers them. Only VUs, think time and the mix do.

`max` is the one number that moves (16.4%, 37.6 → 44.5 ms). It is a single request out of ~343,000 and it is not a thresholded metric: the pass criteria are p95, p99 and the error rate, and all three were flat. It is reported here rather than dropped, because a reader comparing a future run's `max` needs to know this is its natural noise floor.

### Why the spread is trusted rather than merely small

- **Heat-soak is ruled out rather than assumed away — and the evidence covers each run differently.** The thermal-throttle counters are cumulative since boot, so one record says nothing about its own run; only a *delta between two records* bounds a span. They are byte-identical in all three results (`core_throttle_count` 43,441 → 43,441 → 43,441; package 186,551 → 186,551 → 186,551, summed over the cpuset CPUs), which proves **no CPU throttled between the first and the last record** — a span covering **runs 2 and 3 and both cooldowns**. **Run 1's own span is not bounded by any delta** and rests on its other evidence instead: its window opened at the coolest of the three (55 °C on `x86_pkg_temp`, against 57 and 55 elsewhere) and it was the **fastest run while also being the hottest** (86 °C), which is the opposite of the direction throttling pushes. Every window opened well below the recorder's 75 °C hot-start warning, which never fired; the peaks were 86 / 85 / 79 °C, and the two runs that differ most in peak (1 and 3) differ by 0.1% in RPS, with the *cooler* one slower.
- **`thermal.source` is `sampler` in all three** (`stale: false`, windows of 398 / 399 / 399 s at a measured ~1 s interval, 390 / 391 / 390 ticks). These are the runs' own windows, not a reading of the machine at record time.
- **The container was healthy throughout.** `OOMKilled: false`, `RestartCount: 0`, and zero `error`/`busy`/`locked`/`panic`/`Too many open files` lines in `docker logs` after all three holds. No WAL checkpoint stall was suspected (`checkpoint_stall_suspected: false` on the stored `max_ms`/`p99_ms`).
- **The host was quiet and similarly loaded at the start of each run**: load average (1 min) 2.25, 2.68, 2.06 — the ambient desktop (browser, shell), which the cpuset keeps off k6's cores but not off the container's.

### One provenance wrinkle, stated rather than hidden

Run 1 was recorded at commit `52e800a9`; runs 2 and 3 at `1819ddbc`. That is not a change of what was measured: `1819ddbc` adds 80 lines to the plan document (`docs/superpowers/plans/2026-10-13-bench-rig-v2-silo.md`) and touches nothing under `harness/`, `crates/` or `plugins/`. The measured artefact is the container binary, and its sha256 is byte-identical in all three results: `bcc4479a03f7347dac1ecd64cc720548534c5015fd6a3c00a42b329b7d2418eb`. The workload scripts, the mix and the recorder were likewise unchanged across the three runs. `compare.sh` does not treat a differing `git_sha` as a reason to refuse — it prints both shas beside each result so a reader can judge — and it returned `0` for both pairs.

## What was measured

| Fact | Value |
|---|---|
| **Dataset** | **20,000 users / 200,000 posts** (`auth_user` 20,000, `post` 200,000, `like` 79,900, `follow` 159,917 at seed time) |
| **Envelope** | `--cpuset-cpus=0,2,4,6`, 4 GB memory, no swap, `nofile 524288:524288`, `--network host`, image `umbral-bench:latest` |
| **Mix** | `LIKE_P=0.15`, `POST_P=0.02`, `PAGE_P=0.10`, think time 3000–8000 ms — the ratified values in `workload.md` (≈2.17 requests per iteration, a 92.2 / 7.8 read/write split by request count) |
| **VUs** | 2,500, the challenge's starting point and the value `search.sh` doubles from |
| **Sessions** | `SessionsPlugin::default()` — the `DbStore` backend, one real session-store read per authenticated request, **reads included** |
| **Measured binary** | `umbral-bench-prod` (no `TraceRoot`, `DevAuth`, `RequestLog`, `ResponseStamp`, probes, lab middleware or rate limiter), sha256 `bcc4479a…2418eb` |
| **Load generator** | k6 v2.3.0, `taskset -c 8,9,10,11` |

The dataset scale is a parameter, recorded in every result as `dataset_scale`; `compare.sh` refuses to diff two runs at different scales because rows-per-table changes index depth and therefore the latency of the queries under measurement. This baseline is at **20,000** — spec §2a's development/smoke scale. Spec §2a reserves **50,000 users / 500,000 posts** for a number that is quoted outside this repository; a baseline at that scale is the same measurement repeated against a bigger dataset (`seed::all` tops up in place, nothing is wiped) and must be taken before this figure is quoted as the framework's benchmark result. Until then, quote it as *the 20k-scale baseline*.

## What the number includes, and what it does not

**Included, deliberately:**

- **A database session read on every request, including reads.** `SessionsPlugin::default()` is the `DbStore` backend (spec locked decision #3) — what a developer gets without choosing — so the session lookup and its SQLite read are part of the honest cost of this workload. If session lookups were to dominate the number, that would be a finding to act on rather than a cost to configure away.
- **Real authentication.** The workload authenticates through `umbral-auth`/`umbral-sessions` for real; `DevAuth` does not exist in this binary. Every VU is pinned to one pooled session, so its traffic is one coherent user.
- **The write path's contention.** At the ratified mix every iteration reads the feed, thinks, makes one deeper read, and may like (15%) or post (2%). The writes are real transactions against the same SQLite file the reads are reading, one writer at a time — ≈3,100 posts and ≈23,500 likes over a 6.5-minute run, which is why the dataset grows across runs and why a baseline is a number about the dataset at the moment it was taken.
- **The ramp and the drain.** The p95/p99/RPS in the summary are taken over the whole run — 60 s ramp, 5 m hold, 30 s ramp-down — not the steady state alone. The same is true of the challenge this rig calibrates against.

**Not included, and should not be inferred:**

- **No telemetry.** `bench-prod` constructs no instrumentation, so there is no `Server-Timing` header and no per-request layer breakdown behind this number. Attribution lives on :8113 with the instrumented binary, and any trend read from there is diagnostic.
- **Not a saturation ceiling.** This is a *scored hold at a fixed VU count* under a sleep-dominated workload, and with the ratified mix the offered rate is a fixed multiple of the VU count: **`RPS ≈ VUs × 0.3945`** (2.17 requests per iteration ÷ 5.5 s of think) — 986 RPS at 2,500 VUs. That is why the challenge-shaped mix can never answer the "how much can it actually serve" question: the number is set by the sleep, not by the server. `saturation.js` (Phase 2) is the separate exercise that drops the think time to zero and ramps the offered *rate* instead. (An earlier draft of this file carried `RPS ≈ VUs × 0.106` from the plan, written before the mix was ratified; the run's own counters measured 2.1696 requests per iteration over 5.5 s of think, so 0.106 was stale and is gone.)
- **Not a limit search.** 2,500 VUs is where the search begins; where it ends is Task 10's job.
- **Not comparable to a run at another scale, mix, think time, or envelope.** `compare.sh` refuses those diffs by construction.
- **Not a number about a different backend.** SQLite here; a Postgres run is a separate exercise with its own envelope, and no report may compare across backends.

## Reproducing it

```bash
# Dataset: a disposable scratch copy — NEVER harness/backend/bench.db, which is
# the golden gate's fixture (Task 1 captured golden/before against it).
cp harness/backend/bench.db /tmp/umbral-bench-seed/bench-seed.db
UMBRAL_DATABASE_URL="sqlite:////tmp/umbral-bench-seed/bench-seed.db?mode=rwc" \
UMBRAL_BIND=127.0.0.1:8199 \
UMBRAL_BENCH_USERS=20000 UMBRAL_BENCH_POSTS_PER_USER=10 UMBRAL_BENCH_LIKES_PER_USER=4 \
  harness/backend/target/release/umbral-bench-prod serve   # wait for "[bench-seed] done:"
# (UMBRAL_BIND is required on the host: the binary otherwise defaults to
#  127.0.0.1:8114, which the silo already holds.)

cp /tmp/umbral-bench-seed/bench-seed.db harness/bench/data/bench-run.db
harness/bench/run.sh build && harness/bench/run.sh serve
harness/bench/run.sh verify                 # the envelope MUST match before anything runs

# Both halves of the credential chain, against the SERVED database:
docker exec umbral-bench /app/umbral-bench-prod seed-sessions
harness/bench/seed-sessions.sh 8114 200

# Warm up, then the three scored holds (see the header of results/record.sh):
taskset -c 8,9,10,11 ~/.local/bin/k6 run -e BASE=http://127.0.0.1:8114 \
  -e USERS=20000 -e POSTS=200000 harness/bench/k6/warmup.js

taskset -c 12 harness/bench/results/record.sh --thermal-sample &
SAMPLER=$!
taskset -c 8,9,10,11 ~/.local/bin/k6 run -e BASE=http://127.0.0.1:8114 -e VUS=2500 \
  -e USERS=20000 -e POSTS=200000 --summary-export=/tmp/hold-1.json \
  harness/bench/k6/hold.js; K6=$?
kill "$SAMPLER"; wait "$SAMPLER" 2>/dev/null
harness/bench/results/record.sh /tmp/hold-1.json 2500 20000 "$K6"
```

Two traps this flow is shaped around, both of which produce a clean-looking run that measures the wrong thing:

- **Wait for the seed, not for the readiness probe.** `run.sh serve` returns when `/api/ping` answers, but boot also runs the seed top-up. On a 20,000-user dataset that can still be running when the probe passes, and a hold started then measures the seed's writes competing with the workload. Wait for the `[bench-seed] done:` line in `docker logs umbral-bench`.
- **`USERS`/`POSTS` are the id ranges, not the workload size.** k6 draws random ids from them, and a range past the last seeded row turns reads into 404s — which do not fail any threshold (a 404 is not a transport failure and its body is never read) while making the run silently cheaper. `preflight()` refuses the run in about a second if they disagree with the served dataset; do not disable it (`PREFLIGHT=0` is for reads-only smoke runs).

## Where the rest lives

- `workload.md` — the mix, why it is what it is, and what it models.
- `results/README.md` — what a result file contains and how to read it; `results/compare.sh` — the refusal-first comparator.
- `run.sh`, `seed-sessions.sh` — the silo and the credential chain.
- `docs/superpowers/specs/2026-10-13-bench-rig-fixed-resource-silo-design.md` — the design, its locked decisions, and the threats to validity this rig exists to answer.
