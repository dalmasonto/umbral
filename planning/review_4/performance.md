# performance — review_4 companion (2026-10-10)

Consolidated performance analysis for umbral: what the review_4 sweep actually found, where the hot paths are, and what to measure and improve — including the original asks (struct layout, Arc-vs-borrow, allocations). **Read the caveat first:** this is a *static* analysis. No profiles or benchmarks were run (disk at 97% prevented builds; the sweep was read-only), so every number cited below is the *code's own claim* or a mechanical count, not a measurement. Items are marked **found** (verified finding, already in `gaps7.md`) or **proposal** (needs measurement before action). The one existing comparative surface is `examples/cot-bench` (ApacheBench-shape endpoints mirroring shop's `/bench/*`, run against Cot) — it was not run during this sweep.

## 1. What the sweep found (verified findings with a performance dimension)

| gaps7 | Where | What costs | Sketch |
|---|---|---|---|
| **#42** | core, every typed `create`/`save`-INSERT | Row serialized **twice** (+ a full `Map` clone) to build audit arguments whose first act is an `audited`-flag check — unaudited models pay it too (`CORE-13`) | Gate on `ModelMeta::audited` at the call site or one single-serialize helper |
| **#12** | auth `change_password`/`reset_password` | Sync argon2 (~100 ms CPU + 19 MiB per the crate's own doc) **pins a tokio worker**, outside `HASH_GATE` (`AUTH-1`) | Swap to `hash_password_async` (2 lines) |
| **#80** | tasks worker/beat | A closed shutdown channel makes `changed()` `Ready(Err)` forever → **hot DB-poll loop** (`TASKS-3`) | Treat `Err` as shutdown |
| **#24** | storage `TypeLimitedStorage::store_stream` | Buffers the **whole body** before any check — `max_size` bounds writes, not memory (`STORAGE-2`) | Size-cap outermost, or bounded sniff window + capped stream |
| **#60** | email API path | Fresh default `reqwest::Client` per send, **no timeout** (`EMAIL-2`); SMTP transport rebuilt per send (`EMAIL-3`, **#156**) | Shared timed client; cached transport |
| **#57** | core timezone | Unbounded warn-once `HashSet` on caller-supplied names (`CORE-28`) | Cap / ring, or reword the docstring |
| **#40** | core typed UPDATE ×3 | Three UPDATE builders disagreeing (`CORE-11`) — a consistency bug today, a **guaranteed perf-drift** tomorrow (one copy will get an optimization the others don't) | One builder |
| — | core `schema_qualified_table` (`CORE-60`, INFO) | Route-context `Arc` clone per table reference in SQL build | Cache per query |
| — | admin palette (`ADMIN-10`, INFO) | One sequential query per viewable model **per keystroke** | Batch / debounce |
| — | casing `to_snake_case` (`CASING-4`, INFO) | `Vec<char>` + `len+4` String alloc | Peekable iterator if ever hot |
| — | realtime inbound cap (`RT-6`, INFO) | Binary frames bypass the counter (parse cost only) | Count both kinds |

Adjacent correctness items that *are* performance-relevant once fixed: **#15** (flat loader LIMIT — correctness first), **#43** (router bypass — dispatch cost + correctness), **#33** (write-skew → recompute, not serve-stale).

## 2. Hot-path inventory (measure here, in this order)

**Per request:** hyper/axum → tower layer stack (host guard, trace, timeout, security headers, CSRF, session layer, `user_context_layer` when `with_user_in_templates()` — *one DB read per request*, the shop's own comment), slash-redirect, cache headers, logs capture → router → extractors → handler → ORM (`QuerySet` build → sea-query SQL render → sqlx execute: pool checkout, prepared-statement path, decode) → serde/templates → response. The layer stack is the most under-measured area — its per-request clone count is currently unknown.

**Per write:** serialize → validation/cleaners → build INSERT/UPDATE → sqlx execute → readback decode → M2M flush → signals (post-commit) → audit (serialize again — see **#42**) → response strip (`apply_overrides` recursion).

**Background:** tasks worker/beat polls (see **#80**; heartbeat/lease semantics in **#81**), realtime fan-out (frames already rendered once and shared per dispatch — *strength*, `gaps4 #38`), cache bust-log compaction (bounded — *strength*).

**Boot:** `App::build()` phases + `on_ready` per plugin + system checks (15-id catalogue) + migration drift check + RLS policy apply (**#74**'s DDL is currently autocommit DROP-then-CREATE — also a startup-cost item).

## 3. Proposals (each with its measurement gate)

1. **Benches before fixes (do this first).** Add a `criterion` micro-bench suite to `umbral-core` covering: (a) `QuerySet` build+render with no DB (filter chains of 1/5/20 predicates), (b) full layer-stack request baseline (hello route with security+session+logs attached) vs bare router — this isolates middleware overhead, (c) typed `create` with `audited` off/on (pins **#42**), (d) 50-column row serde + dynamic `fetch_as_json`, (e) template render of a realistic page, (f) `App::build()` cold boot with the standard plugin set, (g) `bulk_create(1000)` SQLite+PG. Wire a `cargo bench --quick` smoke into the CI job from `gaps7 #29`. *Gate:* none — this unblocks every other item.
2. **Fix the found costs** — **#42, #12, #80, #24** are cheap, already-sketched, and safe.
3. **Layer stack & identity.** Count the layers a prod request passes and what each clones; measure `with_user_in_templates()`'s DB read (offer a session-embedded identity snapshot or a per-request memo — the session row is already loaded by the session layer, so a second read exists to be eliminated). *Gate:* bench (b) + a template-heavy route profile.
4. **SQL access patterns.** Verify index coverage including M2M **junction tables** (FK autodetection shipped — confirm it covers junction FKs and composite columns); document pool sizing + what `connect()` sets per backend (`busy_timeout` etc.) in one place; note sqlx's per-connection prepared-statement cache behavior so pool-size guidance accounts for it. N+1 is *largely solved* (`select_related`/`prefetch` = 1+N, count pushdown, single-statement bulk) — the remaining SQL leads are correctness bugs (**#15**), not pattern gaps.
5. **Allocation & Arc pass (the original ask), scoped by the bench.** Targets, in order of suspected value: per-request `String`/`Vec` clones in the layer stack; `&Arc<T>` params that need only `&T`; `Arc<str>` for route tables; **#60**'s per-query clone; avoid the `format!`-per-error in hot paths. *Gate:* a counting global allocator in a test binary (allocations per request before/after) — do not guess.
6. **Struct layout & size (the original ask, answered honestly).** The sweep found **no** `repr(C)` padding problems, no oversized-enum hot paths, and rustc already reorders `repr(Rust)` fields — manual field ordering is not a project. Residual, low priority: add `size_of` assertion tests for hot types (`RequestContext`, `QuerySet`, `Column`, `ModelMeta`) so growth is visible, and optionally run `cargo +nightly -Zprint-type-sizes` on `umbral-core` once benches exist (the profile already trades debug info for disk via `line-tables-only`).
7. **Async hygiene audit (small).** Known exception: **#12**. Check the realtime connection tasks and tasks worker for `spawn`-per-item patterns; the argon2 gate + `spawn_blocking` pattern is the model citizen to copy.
8. **Startup & compile.** Measure `App::build()` (bench (f)); the slim-build fix (**#27**) also cheapens consumer compiles; macro consolidation (`MACROS-7`) trims expansion time. Binary/compile profile tuning is already done (`line-tables-only`) — don't revisit without numbers.
9. **Streaming/media.** **#24**'s fix is both the security and the throughput fix; image-decode caps already shipped (strength).

## 4. Strengths — do not regress these

`select_related`/`prefetch_related`/M2M hydration in 1+N queries (no N+1); `count()` pushdown; `bulk_create`/`bulk_update` single-statement; realtime frames rendered once, shared across recipients; cache `BustLog` bounded with amortized-O(1) compaction; argon2 concurrency gate with graceful 503 shedding; pool created once and aliased per route; FK/index autodetection shipped; `line-tables-only` profile keeping `target/` at ~20 GB instead of ~80.

## 5. Honest gaps (what this document cannot tell you)

- No micro-benchmarks exist in the workspace; `cot-bench` is comparative (AB-shaped endpoints vs Cot) and was not run here.
- No production-style profiling was possible from a static sweep — memory/latency numbers in the findings are code-claims (e.g., argon2's ~100 ms) or structural counts, not measurements of your workloads.
- After `gaps7 #29` (CI) lands, the bench smoke + a re-run of `cot-bench` are the two cheapest ways to convert everything above from analysis into numbers.
