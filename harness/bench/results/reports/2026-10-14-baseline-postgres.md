# Bench rig v2 — Postgres baseline

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Commit:** `ad956af2` · **Result files:** `harness/bench/results/ad956af2-20261014T{075701,080640,081620,084434,090614,092754}Z.json`

---

## The number

**≈863 RPS at 2,500 VUs, p95 3.0–3.5 ms, p99 4.07–6.74 ms, zero failed requests, every run `exit.class: passed`** — against **Postgres 18.6 in a sibling container**, on the ORM path the framework recommends.

**The baseline proper is the second series** (runs B1–B3): **≈863 RPS, p95 3.0–3.3 ms, p99 4.07–5.29 ms, RPS spread 0.29%, p99 spread 30.1%.**

### Series A — the plan's 2–3 minute cooldowns

| Run | Recorded (UTC) | RPS | p50 | p95 | p99 | max | Errors | Exit | Thermal peak | Window opened | load1 at start |
|---|---|---|---|---|---|---|---|---|---|---|---|
| A1 | 07:57:01 | 862.059 | 1.607 ms | 3.178 ms | 4.108 ms | 10.355 ms | 0 | passed | 77 °C | 47 °C | 0.48 |
| A2 | 08:06:40 | 865.282 | 1.348 ms | 3.536 ms | **6.744 ms** | 32.802 ms | 0 | passed | 95 °C | 65 °C | 2.18 |
| A3 | 08:16:20 | 861.741 | 1.276 ms | 3.221 ms | 5.539 ms | 27.026 ms | 0 | passed | 96 °C | 63 °C | 4.12 |

RPS spread **0.41%** · p95 **11.3%** · p99 **64.2%** · max 216.8%.

### Series B — cooldown gated on the package sensor

| Run | Recorded (UTC) | RPS | p50 | p95 | p99 | max | Errors | Exit | Thermal peak | Window opened | load1 at start |
|---|---|---|---|---|---|---|---|---|---|---|---|
| B1 | 08:44:34 | 862.722 | 1.324 ms | 3.287 ms | 5.290 ms | 25.660 ms | 0 | passed | 96 °C | 63 °C | 2.81 |
| B2 | 09:06:14 | 865.223 | 1.346 ms | 3.035 ms | **4.065 ms** | 13.094 ms | 0 | passed | 96 °C | 65 °C | 1.88 |
| B3 | 09:27:54 | 863.251 | 1.310 ms | 3.050 ms | 4.210 ms | 15.212 ms | 0 | passed | 96 °C | 65 °C | 0.67 |

RPS spread **0.29%** · p95 **8.3%** · p99 **30.1%** · max 96.0%.

**All six runs together:** RPS spread **0.41%** (861.741–865.282), p95 16.5%, p99 65.9%. All six: 0 failed requests, 0 failed checks, all thresholds cleared, `git_dirty: false`.

---

## ⚠️ Not comparable to the SQLite baseline

**No artifact from this run may be diffed against `2026-10-14-baseline-sqlite.md`.** SQLite's database is a function call inside the measured process; Postgres's is a TCP round trip to a sibling container with its own 1 GB and its own share of the same four cores. They are different architectures with different envelopes, and a delta between them is the architecture, not the code. Only sqlite-vs-sqlite and postgres-vs-postgres comparisons are meaningful.

`compare.sh` enforces this structurally: it derives the backend from each result's recorded `wal.db_url` scheme and **refuses with exit 3** when two results name different backends.

**One thing a reader will notice and must not act on.** Several Postgres runs produced a *lower* p99 than the SQLite baseline's 4.747–4.815 ms (B2: 4.065 ms). That is not "Postgres is faster than SQLite". SQLite serialises writers; Postgres does not, so the Postgres tail is not paying a checkpoint / writer-contention cost — while paying a network hop SQLite does not have. The two tails are shaped by different mechanisms. Neither number is better; each is a fixed point only for itself.

---

## What the number is, and what it is not

**863 RPS is what 2,500 virtual users asked for, not what umbral can do.** The mix offers `2.17 requests/iteration ÷ 5.5 s think` = **0.3945 req/s per VU** — ≈986 RPS flat at 2,500 VUs, averaged to ≈866 over the run's ramp and drain. Throughput is pinned by the think time; the server's own latency is ~2 ms against a 5.5-second loop, and the observed request concurrency is ~1.3 requests in flight, **at most a third of the silo's four CPUs**.

Two consequences:

1. **RPS here is a one-sided regression detector.** A server that stopped keeping up would show up as failed requests and timeouts at almost unchanged RPS; a server that got faster would show up as almost unchanged RPS too. **p95/p99 is the metric a Phase 3 win has to move.**
2. **The design predicted this exercise's shape — "p95 rising while RPS holds means the hop costs latency without costing throughput" — and that is exactly what Postgres produced.** RPS moved by 0.41% across six runs in which p99 varied by 66%.

---

## ⚠️ The p99 spread is 30–64%, and the host is why

This is the most important caveat in this report, and it is a **rig** finding, not a framework one.

**Series A could not serve as a comparator.** A p99 spread of 64% is larger than any effect a Phase 3 change could plausibly produce, so the second series was run with the cooldown gated on the package sensor, intended to make every window open cool.

**The gate did not work.** On all three occasions the cooldown ran to its 900-second cap and the machine still read 55–56 °C; it never reached the 50 °C target. Every series-B window opened at **63–65 °C — the same as series A's hot runs.** The host carries a continuous ambient load (a Chrome renderer at ~100% of a core, a video-capture service, `gnome-shell`; load average 1.9–4.9 throughout, with the measured cores 0/2/4/6 sitting at 15–22% busy *at idle*). There is no cool state to return to while that load is running.

**That falsifies temperature as the explanation.** Ranked by window-opening temperature, two runs opening at the same 65 °C differ by 66% in p99 (4.065 vs 6.744). Temperature does not order the runs. **The host's load average at the window's start does**:

```
load1 < 2  ->  p99 = 4.108, 4.210, 4.065
load1 >= 2 ->  p99 = 6.744, 5.290, 5.539
```

The separation is perfect, and it holds *across* the temperature split — one low-load run opened at 65 °C, one high-load run at 63 °C. p99 tracks `max` at r = +0.97, i.e. it is one or two 25–33 ms outliers per run, not a broad shift: a scheduling hiccup on a shared core, which moves the tail and does not move think-pinned RPS.

**What this means for the next cycle.** On this host, as it stands, a Phase 3 change will not be demonstrable on p99 unless its effect exceeds ~30%. RPS is stable enough (0.3–0.4%) to serve as the regression guard, but it is structurally incapable of registering an improvement at this VU count. Quieting the host — or accepting the noise floor and quoting it — is a prerequisite for Phase 3, not a detail of it.

**The thermal-throttle counters, for the record.** Cumulative since boot, read after each run, so each delta spans the preceding cooldown plus the following run. The pre-measurement baseline was core 44,290 / package 190,336 — **identical** to A1's post-run reading, so the 3.5-minute warm-up and A1's whole run moved them by zero.

| Interval | Δcore | Δpackage |
|---|---|---|
| A1 → A2 | +12 | +48 |
| A2 → A3 | +95 | +380 |
| A3 → B1 | +30 | +128 |
| B1 → B2 | +26 | +104 |
| B2 → B3 | +50 | +204 |

The counters are recorded, not used as an explanation: B3 moved more than B2 and had the *better* p99. For contrast, the SQLite baseline's window moved them by **zero** — this host was measurably more loaded during the Postgres work.

---

## Configuration

| | |
|---|---|
| Backend | **Postgres 18.6**, sibling container `umbral-bench-pg`, port **55432** |
| Database envelope | `--cpuset-cpus=0,2,4,6 --memory=1g --memory-swap=1g --network host`, data on a bind mount at `harness/bench/data/pg`, `memory.swap.max` 0 |
| Postgres settings | **stock defaults, deliberately** — `shared_buffers` 128 MB, `max_connections` 100, `work_mem` 4 MB, `fsync` on, `synchronous_commit` on |
| App envelope | `--cpuset-cpus=0,2,4,6`, 4 GB, no swap, `nofile 524288:524288`, `--network host`, `run.sh verify` exit 0 |
| Measured binary | `umbral-bench-prod` sha256 `00a65b45…91ad88`; image `sha256:c41d6d9b8283…28595c` |
| VUs | 2,500 |
| Dataset | **20,000 users / 200,000 posts** — ids contiguous from 1, `USERS=20000`, `POSTS=200000` |
| Workload | the ratified mix and think time, unchanged from the SQLite baseline |
| Auth | **real** session auth — `DbStore` against Postgres, a session-store read inside every authenticated request |
| k6 | v2.3.0, pinned to cores 8–11 |

**Schema provenance.** The served database (`umbral_bench_pg`) was created empty and built entirely by the app's own migrations — `auth/0001_auto`, `bench/0001_auto`, `bench/0002_auto`, `sessions/0001_create_session`. No schema was hand-written for Postgres.

**One binary note.** The app binary and image were rebuilt for this task, so the measured binary's sha256 differs from the SQLite baseline's. The rebuild was required: the image's `migrations/` COPY still carried the damaged `0001_auto.json`, and the already-committed `crates/` migration fixes had to be compiled in. No source was modified — the rebuild compiles the committed tree.

## Honest caveats

- **Scale is 20,000, not the 50,000** the spec reserves for figures quoted outside the repo — same as the SQLite baseline, so the two remain comparable to their own kind.
- **The dataset grows during a run.** `post` 200,000 → 219,387 and `like` 80,000 → 226,978 across the warm-up plus six holds. A baseline is a number about the dataset at the moment it was taken.
- **`like` grew 2.8×** because each hold's like-rate draws fresh random `(user, post)` pairs and the UNIQUE constraint rejects only collisions.
- **`SecurityPlugin` runs without HSTS or CSP** — two warnings per boot, unchanged from the SQLite baseline.
- **Postgres was not tuned.** `shared_buffers` and friends are stock because tuning them would measure this rig's configuration expertise rather than the framework. If Postgres's defaults turn out to be the binding constraint later, that is a finding, and tuning is a separate deliberate change.

## Reproduction

```bash
# Postgres in a sibling container on the app's cpuset, host networking, stock settings.
docker run -d --name umbral-bench-pg --cpuset-cpus=0,2,4,6 --memory=1g --memory-swap=1g \
  --network host -v "$PWD/harness/bench/data/pg:/var/lib/postgresql" \
  -e POSTGRES_PASSWORD=… -e POSTGRES_DB=umbral_bench -e POSTGRES_USER=umbral \
  postgres:18 -c port=55432
docker inspect umbral-bench-pg -f '{{.HostConfig.CpusetCpus}} {{.HostConfig.Memory}}'   # verify, do not trust
docker exec umbral-bench-pg cat /sys/fs/cgroup/memory.swap.max                          # must be 0

# Create the served database, let the APP build the schema, then seed it at scale.
psql -h 127.0.0.1 -p 55432 -U umbral -d postgres -c "CREATE DATABASE umbral_bench_pg OWNER umbral;"
UMBRAL_DATABASE_URL="postgres://…@127.0.0.1:55432/umbral_bench_pg" \
  harness/backend/target/release/umbral-bench-prod migrate          # -> Applied 4 migration(s)
UMBRAL_DATABASE_URL="postgres://…@127.0.0.1:55432/umbral_bench_pg" UMBRAL_BIND=127.0.0.1:8199 \
UMBRAL_BENCH_USERS=20000 UMBRAL_BENCH_POSTS_PER_USER=10 UMBRAL_BENCH_LIKES_PER_USER=4 \
  harness/backend/target/release/umbral-bench-prod serve            # wait for "[bench-seed] done:"

# The silo, on Postgres. The URL must be a whole NAME=value assignment.
UMBRAL_DATABASE_URL='UMBRAL_DATABASE_URL=postgres://…@127.0.0.1:55432/umbral_bench_pg' \
  harness/bench/run.sh serve
harness/bench/run.sh verify                                        # envelope before anything else
docker logs umbral-bench | grep '\[bench-seed\] done:'             # readiness is NOT seed-completion

# The credential chain, both halves, against the SERVED database.
docker exec umbral-bench /app/umbral-bench-prod seed-sessions
harness/bench/seed-sessions.sh 8114 200

# Warm up, then the scored holds with the sampler bracketing each one.
taskset -c 12 harness/bench/results/record.sh --thermal-sample &
SAMPLER=$!
taskset -c 8,9,10,11 ~/.local/bin/k6 run -e BASE=http://127.0.0.1:8114 -e VUS=2500 \
  -e USERS=20000 -e POSTS=200000 --summary-export=/tmp/hold.json harness/bench/k6/hold.js
K6=$?                                    # k6's OWN status, captured immediately
kill "$SAMPLER"; wait "$SAMPLER" 2>/dev/null
harness/bench/results/record.sh /tmp/hold.json 2500 20000 "$K6"
```

## What's next

Phase 3 optimization cycles on the Postgres backend, reporting **latency percentiles as the primary signal** and RPS as the regression guard — **after** the p99 noise floor in the section above is addressed or explicitly accepted. A change that improves p99 by less than 30% on this host has not been demonstrated to do anything.
