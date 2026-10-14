# The results store

Every number this rig quotes is written by `record.sh` and argued from `compare.sh`. The point of the pair is that a performance number is not a number: it is a number *plus* every fact needed to decide whether it means anything — which commit and which binary, what container envelope, what dataset scale, what workload mix, whether the machine was throttling, whether the container was OOM-killed, whether the run's checks actually held. A number without those facts is an anecdote, and six weeks later nobody can tell the difference.

`record.sh` writes one JSON per run into this directory (`results/<git-sha>-<utc>.json`); `compare.sh` diffs two of them and refuses to when they describe different things. The JSONs and the sampler's trace are gitignored (`harness/.gitignore`: `bench/results/*.json`, and this directory's own `.gitignore` for `.thermal-sample.tsv` / `.record.*`) — the store is local and machine-specific, and a result is only meaningful beside the envelope facts it carries. The scripts and this file are committed.

## Recording a run

```bash
# the sampler: the run's own thermal + load window (started before k6, stopped after it)
harness/bench/results/record.sh --thermal-sample &
SAMPLER=$!

# the run (scored hold; the summary export is what record.sh reads)
taskset -c 8,9,10,11 ~/.local/bin/k6 run \
  -e BASE=http://127.0.0.1:8114 -e VUS=2500 -e USERS=20000 -e POSTS=200000 \
  --summary-export=/tmp/hold-1.json harness/bench/k6/hold.js
K6=$?                        # k6's OWN status, captured before anything else runs
echo "exit=$K6"

kill "$SAMPLER"; wait "$SAMPLER"

# the record — the captured code, never a bare `$?`
harness/bench/results/record.sh /tmp/hold-1.json 2500 20000 "$K6"
```

**Capture k6's exit code immediately, as above — never a bare `$?` on the `record.sh` line.** By then `$?` belongs to `wait`, and the sampler exits **0** (it traps `TERM`/`INT` and returns explicitly), so a bare `$?` hands the recorder `0` for every run: a hold that exited `99` on a failed threshold, or `107` on a preflight abort, would be filed as `exit.class: "passed"` — the exact field `compare.sh` keys on — with only the recorder's cross-check warnings hinting otherwise. It fails *silently*, and a regression recorded as a pass is the one failure this store exists to prevent. (An earlier version of this example used the bare `$?`; the same shape was still loose in `bench/workload.md`, and both are corrected now. `bench/README.md`'s recording example never carried it.)

**Run the sampler around every hold you intend to quote.** It samples the thermal zones and the load average once a second on the host — never in the container, whose cores are the measurement; its cost is a handful of sysfs reads a second, and it is not CPU-pinned (start it under `taskset -c` with a spare core if you want it off the k6 cores entirely) — and the recorder reads the run's peak out of its file (`results/.thermal-sample.tsv` by default; `BENCH_THERMAL_SAMPLE` overrides it, and the sampler truncates it at start, so one file is one run's window). Without it there is no in-run temperature at all, and the recorder says so in the result: see [Reading the facts that cannot be measured in a single file](#reading-the-facts-that-cannot-be-measured-in-a-single-file). A sampler file that does not cover the run it is being recorded against is rejected, not used.

Arguments are the summary path, the VU count, the dataset scale, and the **k6 exit code**. The exit code is not a boolean; `record.sh` classifies it:

| Exit | Meaning | Recorded as |
|---|---|---|
| `0` | thresholds held | `exit.class: "passed"`, `verdict: "pass"` |
| `99` | a threshold failed (latency or error rate) | `exit.class: "threshold_failed"`, `verdict: "fail"` — a real load result |
| `107` | aborted in `setup()`/preflight | `exit.class: "preflight_abort"`, `verdict: "not_a_load_result"` — a **rig-configuration** failure (wrong id ranges, a pool that does not match the served database), not a server failure |
| anything else | unclassified | `exit.class: "unclassified"`, `verdict: "unknown"` — recorded as-is, never folded into either bucket |

Filing a `107` as a load result would blame umbral for a typo'd `-e USERS=`; the two are read weeks apart by someone who was not there.

**The recorder refuses to write anything** when the envelope cannot be verified: no container named `umbral-bench`, or a `HostConfig.CpusetCpus` that is not `0,2,4,6` (read with `docker inspect` — what took effect, not what was asked for). It also refuses on unusable arguments or a missing/corrupt summary export. Its exit codes: `0` written, `2` bad invocation, `3` envelope refusal.

Facts that cannot be verified but should not be silent — a memory envelope other than 4 GiB/no-swap, a dirty working tree, a container that is not running, a check that failed, a VU count or dataset scale that disagrees with what the run's own `setup_data` records, `OOMKilled`, a missing `setup_data`, a suspicious latency spike — are written into `.warnings[]` and echoed at the end of the recorder's output. `compare.sh` surfaces them again when two results are diffed.

## What a result contains

Flat where comparison happens, grouped where reading happens. Schema `1`:

| Field | Meaning |
|---|---|
| `git_sha`, `git_sha_short`, `git_dirty` | the commit the rig believes it measured; `git_dirty: true` means the tree did not match it |
| `recorded_at`, `summary.written_at`, `summary.recorded_after_run_s` | when the result was filed, when k6 wrote the summary (≈ when the run ended), and the gap between them |
| `vus`, `dataset_scale` | the run's own parameters, cross-checked against the summary (`.vus_max`) and the workload's verified id range (`.workload_range`) |
| `mix` | `LIKE_P` / `POST_P` / `PAGE_P` **and** `think_ms`, read from the run's own `setup_data` — never re-typed |
| `rps`, `p50_ms`, `p95_ms`, `p99_ms`, `max_ms`, `error_rate` | the measurement. `rps` is k6's `http_reqs.rate`; percentiles are in **milliseconds** (the summary export's own unit — do not re-scale it); `error_rate` is a ratio 0..1 |
| `requests`, `iterations`, `vus_max` | supporting counts |
| `checks` | per-check `passes`/`fails` from the summary (the thresholds cover latency and error rate, **not** the checks — a run where `page 200` never passes can still exit 0) plus `held` |
| `thresholds` | the summary's own threshold map, verbatim (in k6's export a `true` means **violated**) |
| `exit`, `verdict` | the classified exit code and the verdict |
| `k6_setup_data` | the summary's `setup_data`, verbatim — the run's own record of its parameters |
| `envelope` | container, image and image id, the binary's path/args/`sha256`, cpuset, CPU cap, memory and swap limits, the in-container `nproc` / `memory.max` / `memory.swap.max` / `nofile`, `container_running`, `oom_killed`, `verified` |
| `thermal` | peak thermal-zone temperature per zone, with its provenance: `source` (`sampler` = the run's own window, `post-run-window` = the fallback), `stale`, the window's `from`/`to`/`window_s`, `interval_s`, `samples` (ticks, not per-zone readings) and `sampled_after_run_s` (null for `sampler`) |
| `throttling` | the CPUs' thermal-throttle counters, cumulative since boot, restricted to the container's cpuset CPUs. An unreadable counter is `null`, and one unknown component makes the aggregate `null` — never `0`, which would read as "no throttling" |
| `wal` | the served database's `db_bytes`/`wal_bytes`/`shm_bytes`, the container log's busy/error line counts (`null` if `docker logs` could not be read — not `0`), and `checkpoint_stall_suspected` with its rule in `basis` |
| `session` | the session backend, cookie, TTL and max age the measured binary runs with |
| `host` | hostname, kernel, CPU, `loadavg_before_run` (the sampler's first reading, null when no sampler ran), the load average at record time, and the recorder shell's `ulimit -n` |
| `warnings` | everything above that a reader should not miss |

## Comparing two results

```bash
harness/bench/results/compare.sh results/<a>.json results/<b>.json
```

It prints one line per metric — `rps`, `p95_ms`, `p99_ms`, `error_rate` — then a `context` block (checks, thermal, throttling, differing VU counts, recorded warnings), then a verdict.

| Exit | Meaning |
|---|---|
| `0` | comparable, every thresholded metric within its limit |
| `1` | comparable, but a metric moved beyond its limit |
| `2` | a file cannot be read or parsed (or is not a `record.sh` result) |
| `3` | **refused**: these results are not comparable |

Limits: `rps` beyond **±10%**, `p99_ms` beyond **±20%**. Both directions count — a run that got 30% faster is as much a change as one that got 30% slower, and the verdict line says which (`REGRESSION` / `IMPROVEMENT`). `p95_ms` and `error_rate` are printed with `(no limit)`: they are shown, not judged.

It **refuses** (exit 3, no comparison at all) when:

- either file is not a load result (`preflight_abort`, `unclassified`, or any class other than `passed`/`threshold_failed`);
- `dataset_scale` differs — the same code against a bigger dataset is a different measurement;
- the container `cpuset` differs — different CPU sets are different machines;
- `mix` differs, **including `think_ms`** — the mix is what "fast" means for this rig;
- `rps` is zero (a relative change from zero is undefined), or any comparison fact is missing/null.

It **fails closed**: a file it cannot parse, or one missing `schema`, is an error (exit 2) — never a silent pass.

## "Spread" means range ÷ mean

Every `spread` figure this rig quotes is **(max − min) ÷ mean**, as a percentage, taken over the runs of one series. The formula was implicit until it turned out two baselines had been computed differently: the SQLite baseline divided by the **mean** (its p50 row gives 5.5%, where dividing by the minimum gives 5.7%), while the first draft of the Postgres baseline divided by the **minimum**, which inflated its p99 spread from 48.2% to 64.2%. The two baselines' spread columns were therefore not comparable with each other, and the "noise floor" quoted for every future p99 claim came from the inflated figure.

**range ÷ mean is canonical.** Min, max, mean and range are printed beside every spread so either convention can be recomputed from what is written. The SQLite baseline already used it and needed no change; the Postgres figures were recomputed. The convention matters most exactly where the numbers are small — on the tail, where dividing by the min instead of the mean moves the answer by a third — but it changes no conclusion in either baseline. What it changes is what a future run has to beat.

## Reading the facts that cannot be measured in a single file

Four facts are recorded in forms that only mean something when read correctly:

- **Thermal.** `thermal.source` says where the peak came from. `sampler` is the run's own window — the only reading that may be used to explain a run-to-run difference as heat-soak. `post-run-window` is the fallback for a hold that was recorded without the sampler: it is the machine's temperature at record time, and the further from the run it was taken, the less it has to do with it. Past `THERMAL_FRESH_S` (60 s) the recorder marks it `stale: true` and the note says in as many words that it is not evidence about the run; a stale sample is not a weaker version of the run's peak, it is a measurement of something else. Degenerate case worth knowing: at a lag of tens of minutes the window can even go *up* — the recorder has 10 s of whatever the machine is doing, and the machine is doing something else.
- **Load average.** `host.loadavg_before_run` is the sampler's first reading, taken a moment before k6 is launched — the closest available reading of the host at the run's start. Without a sampler, only `loadavg_at_record` exists, and the note distinguishes the two cases: within ~2 minutes it is *run-tainted* (the 1/5-minute averages still carry the run's tail, but they are not a pre-run baseline), beyond that it describes a machine that has long since moved on, and the recorder warns.
- **Throttling.** The per-CPU throttle counters (`/sys/devices/system/cpu/cpu*/thermal_throttle/`) are cumulative since boot, so one file cannot say whether throttling happened during its own run. `compare.sh` prints the delta between two results: a non-zero delta means the CPU throttled *between the two records*, which includes the second run. The counters are restricted to the container's cpuset CPUs, which also carry other processes' work. An unreadable counter is `null`; `compare.sh` renders a null counter as `n/a` rather than a delta of zero.
- **`ulimit -n` and the database files.** `nofile` is the limit of the shell that invoked `record.sh`, which in the documented flow is the same shell that ran k6. The `wal` sizes are read from the bind mount at record time; an absent `-wal` (SQLite removes it when the last connection closes cleanly) is recorded as `null`, not as an error and not as `0`.

The `wal` block is the same idea for storage: `bench-prod` deliberately carries no per-request instrumentation, so a SQLite checkpoint stall cannot be observed directly. `checkpoint_stall_suspected` is inferred by the explicit rule in `.wal.basis` (`max_ms > 200` **and** `max_ms > 10 × p99_ms`) from the raw numbers stored beside it; the log counters are cumulative since container start and are meant to be diffed across results.

## The tail's spread: what is established, and what is not

Measured while taking the Postgres baseline (2026-10-14, `reports/2026-10-14-baseline-postgres.md`), and recorded here because it changes how the next cycle has to be run rather than only how one report reads.

**Established — the cooldown does not set this tail, and heat does not explain it.** Six holds at 2,500 VUs, same envelope, same mix, same binary, same commit: **RPS spread 0.41%** across all six, and **p99 spread 53.7%**. Series A used the plan's 2–3 minute cooldowns; series B extended the cooldown to at least 180 s and then held until the package sensor read ≤ 50 °C, capped at 900 s, specifically so every window would open cool. **The gate never reached its target** — the machine bottomed out at 55–56 °C and every series-B window opened at 63–65 °C, the same as series A's hot runs. There was no cool state to return to while the desktop was busy (a browser renderer near 100% of a core; the measured cores sat 15–22% busy at idle).

That is the half to act on. Ranked by window-opening temperature the runs do not order themselves — two windows opening at the same 65 °C differ by 66% in p99 — so **temperature does not explain the spread, and a longer cooldown is not the fix**: the plan's 2–3 minutes is sufficient, because more does not cool this machine and series B spent 900 s per hold to no effect.

**Not established — that ambient load is the driver.** Across the six runs `host.loadavg_before_run` is the best *in-sample* predictor of p99 — the three starting below load 2 gave 4.108 / 4.210 / 4.065 ms, the three at load 2 or above gave 6.744 / 5.290 / 5.539 ms, a perfect 3–3 partition that holds *across* the temperature split (a 65 °C window in the low group, a 63 °C window in the high one). **Treat that as a hypothesis to requote at n=6, not as a result.** Six points, a threshold chosen after seeing the data, and a partition worth roughly p ≈ 0.05 under a random null support no causal claim; the linear relation is only r = +0.58, which the numbers above already show. And there is a counter-datapoint in this rig's own records: **the SQLite baseline ran at load 2.06 / 2.25 / 2.68 — all at or above the threshold — with a 1.43% p99 spread and no wide tail.** Load average counts runnable processes and says nothing about what they are doing; two mostly-idle processes and a pegged browser renderer are the same number to it.

**What is safe to act on for the next cycle:**

- **A longer cooldown is not the fix and should not be added to the recipe.** See above: series B proved it by spending 900 s per hold for no change in window-opening temperature. The SQLite baseline, by contrast, passed on *byte-identical* throttle counters — zero movement across its whole window — which is what a quiet host looks like.
- **Record `host.loadavg_before_run` beside every comparison.** It is already in every result; nothing needs building. Until the sample is large enough to say more, quote it as one of the facts a reader needs, not as an explanation.
- **Do not read a small p99 movement as a result — and note that `compare.sh` will.** The measured p99 spread is **27.1%** on the tighter series and 53.7% across all six, while `compare.sh`'s p99 limit is **20%**, which sits *inside* that noise. Diffing B1 → B2 — two runs of identical code — returns `p99_ms 5.29 -> 4.065  -23.16%  IMPROVEMENT (limit 20%)` and **exit 1**. The comparator's verdict lines are a change detector, not evidence of a change; on this host they will call noise a win the moment Phase 3 starts.
- **RPS remains a usable regression guard at ±0.4%**, and is structurally incapable of registering an improvement at this VU count, because it is the think time expressed as a rate.
- **If a tighter tail is needed, quiet the host first.** One series measured on a genuinely idle machine would set a much lower noise floor than any amount of cooldown discipline on a busy one.
