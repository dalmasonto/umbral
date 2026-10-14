# Bench rig v2 — splitting the database bucket: where the 87% actually goes

**Date:** 2026-10-14 · **Branch:** `bench-rig-v2` · **Diagnosis, not measurement** — no number here is quotable as latency; the products are proportions and per-statement splits.

The layer ledger puts **83% (SQLite) to 87% (Postgres) of server time in the `database` bucket** and could never say which query. This report opens that bucket with two complementary passes, and lands on an arithmetic that reorders the optimization targets.

## Method

- **Server-side** (the real prod workload, Postgres): the PG container was recreated with `pg_stat_statements` preloaded (same flags; data on the bind mount; nothing dropped), the app restarted so no pooled connection was stale, `pg_stat_statements_reset()`, then one diagnostic 2,500-VU hold (~343k requests) and the per-statement totals read from the view. This covers statements the instrumented binary cannot model — including the **DbStore session read**.
- **Client-side** (instrumented binary on :8113, same envelope, DevAuth seam with a run tag): one 2,500-VU hold; the per-fingerprint client-observed times are a before/after **diff of the global snapshot** (`/api/bench/telemetry`), because per-request SQL attribution is empty on both backends (see *Gaps* below). Read alongside the per-request JSONL layer records from the same run.
- **The ping A/B** (local, same binary, PG, 8 VUs × 20 s, zero think, one variable): `UMBRAL_DB_TEST_BEFORE_ACQUIRE` true vs false.

## 1. Server-side split (prod workload, per whole run)

Server execution totals **≈36.3 s across ~343k requests ≈ 106 µs/request**:

| share | calls | mean | statement |
|---|---|---|---|
| **32.1%** | 158,037 | 74 µs | `SELECT * FROM auth_user WHERE id IN (…)` — `select_related("author")`'s prefetch, **~25 rows per feed request** |
| **13.6%** | 342,925 | 14 µs | `SELECT … FROM session WHERE id = $1` — the DbStore read, 1/request |
| 10.9% | 158,037 | 25 µs | the feed itself (`post … ORDER BY created_at DESC LIMIT`) |
| 6.3% | 158,037 | 14 µs | `like WHERE post IN (…)` — returns **0 rows** for load-fresh posts |
| 6.2% | 23,721 | 95 µs | `INSERT INTO like … RETURNING *` |
| 5.2% | 142,329 | 13 µs | `COUNT(*) FROM like WHERE post = $1` (post page) |
| 4.3% | 142,329 | 11 µs | `SELECT * FROM auth_user WHERE id IN (…)` (single author, post page) |
| 4.1% | 158,037 | 10 µs | `comment WHERE post IN (…)` |
| 4.1% | 142,329 | 11 µs | `COUNT(*) FROM comment WHERE post = $1` |
| 4.1% | 142,329 | 10 µs | `post WHERE id = $1 LIMIT $2` (post page) |

**Statements per request ≈ 4.7.** The framework's own composition dominates: the author prefetch (32%), the session read (13.6%), and the post page's two separate count queries (9.3% together).

## 2. Client-side split (instrumented run, per request)

Per-request averages over 342,846 requests (JSONL, PG, DevAuth seam — so no session read; prod's bucket is *larger* than this by the session read's client cost):

| layer | avg µs | share |
|---|---|---|
| **database** | **1181** | **87.3%** |
| middleware (all) | 67 | 5.0% |
| tail | 40 | 3.0% |
| serialize | 36 | 2.7% |
| render | 18 | 1.3% |
| routing | 7 | 0.5% |
| handler_logic | 4 | 0.3% |
| **total** | **1353** | 100% |

Per route (`total` / `database`, µs): feed **1628 / 1416** · like-write 1466 / 1379 · profile page 1299 / 1118 · post-create 1050 / 955 · post page 1041 / 898.

Client-observed per-statement time (fingerprint diff, whole run ≈ **544 µs/request** across ~4.7 statements): feed list 221 µs · author prefetch 193 µs · post-by-id 112 µs · like COUNT 103 µs · like IN 100 µs · auth IN(single) 100 µs · comment IN 88 µs · comment COUNT 97 µs · **INSERT like 605 µs** · INSERT post 590 µs.

## 3. The arithmetic that reorders the targets

For the same run, three independent totals per request:

```
server-side execution (pg_stat)            ≈ 106 µs
client-observed statement time (sqlx)      ≈ 544 µs
the ORM's `database` bucket                ≈ 1181 µs   (instrumented, no session read)
```

- **The database is not the cost — the round trips and the ORM around them are.** Server execution is ~9% of the client-observed statement time; ~438 µs/request is per-statement client handling (round trip, protocol, decode-in-sqlx) across ~4.7 statements (~90 µs each).
- **And ~637 µs/request is not inside any statement at all** — it sits between them: pool acquire/release, the liveness ping, ORM row→model mapping, and trace accounting. **47% of the bucket is outside the statements.**
- **The ping is real but small**: `test_before_acquire` (default **true**) makes every acquire of an idle connection a `Sync`+ready round trip on PG (invisible to `pg_stat_statements`). A/B with only that knob changed: database 706 → 623 µs (**−83 µs/request, −12% of the bucket**; total 975 → 910 µs). The setting's own doc says "a few microseconds per acquire"; the measured cost is ~17 µs per ping across ~4.7 statements — **the doc understates it by ~3–8×**.

## 4. Targets, ranked by measured size

1. **Statement count per request** (4.7 now): the author prefetch (1 statement + 25-row `SELECT *` decode), the post page's two separate `COUNT`s, the feed's two `IN` fetches that return nothing for fresh posts. Each statement costs ~90 µs of client handling *plus* its share of the 637 µs gap. `join_related` ("one round-trip total" per its own docs) collapses the prefetch.
2. **The 637 µs outside statements**: decode/mapping + acquire/ping. The ping is measured (−83 µs, knob-only fix); the mapping share is not yet isolated.
3. **The session read**: 1 statement/request (~124 µs all-in: 14 µs server + client handling), 13.6% of server DB time. In-process caching is a *correctness* trade; the store comparison (Redis/Cookie) is the operator's requested route.
4. **Write statements**: `INSERT like` 605 µs client-observed (95 µs server) — the gap is likely `synchronous_commit=on` + the FK `FOR KEY SHARE` lock (50,563 calls, invisible to the ERM wall split).

## Gaps (stated, not hidden)

- **Per-request SQL attribution does not work** — `sql_count` is 0 on every JSONL record and the per-run `sql` block is empty, on both backends (verified locally). The sqlx event is not matched to the request span. This report's route↔statement mapping is by call-count inference, not attribution. (The global snapshot diff works and is what section 2 uses.)
- The global snapshot truncates to the top 25 fingerprints **by average**, so a high-call/low-average statement could be missing from the diff.
- The instrumented bucket excludes the DbStore session read (ruling 15) — prod's is bigger.
- The ping A/B is local (8 VUs, zero think) — the rig-level confirmation is Cycle 1's hold pair.

## What this licenses

A **ranked, measured target list** for Phase 3, and one already-scoped knob-only cycle (the ping). It does not license any latency claim from either diagnostic run.
