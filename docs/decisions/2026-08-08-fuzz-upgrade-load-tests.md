# Fuzz, upgrade compatibility, and load/soak testing

Status: proposed (planning/gaps5.md #95 tf#308, #96 tf#309, #97 tf#310). Date: 2026-08-08.

## Why this doc

Three testing gaps sit next to each other in the gaps5 backlog and share one root: umbral has good example-driven and unit coverage, but nothing that hammers the critical parsers and planners with generated input (#95), nothing that proves a project generated against an older release still migrates on the current crates (#96), and nothing that measures behaviour under scale or sustained load (#97). This document specifies each one concretely enough to build, names the exact surfaces to target with the real symbol names, and is honest about which of the three needs infrastructure we do not yet run.

The three are deliberately one doc because they answer the same maintainer question ("what breaks that our current tests cannot see?") from three angles: adversarial input, time (old artifacts against new code), and scale. #96 is a 1.0 gate per `STABILITY.md`; the other two are confidence work that can land incrementally.

## #95: Fuzz and property tests for critical parsers and planners

### The surfaces worth fuzzing

These are the pieces that take structured or attacker-influenced input and turn it into SQL, DDL, or in-memory state. A silent wrong answer in any of them is a correctness or security bug, which is exactly what property testing is good at surfacing.

1. **The migration autodetector and renderer** (`crates/umbral-core/src/migrate.rs`). The planner is `diff(previous: &Snapshot, current: &Snapshot) -> Result<Vec<Operation>, MigrateError>`, which emits an ordered `Vec<Operation>` (`CreateTable`, `DropTable`, `AddColumn`, `DropColumn`, `AlterColumn`, `RenameTable`, `CreateM2MTable`, `CreateView`, `DropView`, and the rest of the enum). `Snapshot` is `{ models: Vec<ModelMeta> }`, built live via `Snapshot::current()` and hashed via `Snapshot::hash()`.
2. **The filter and predicate builder** (`crates/umbral-core/src/orm/mod.rs`, `Predicate<T>` over `sea_query::SimpleExpr` with the SQLite override `cond_sqlite`) rendered through the QuerySet `to_sql()` / `to_sql_pg()` at `crates/umbral-core/src/orm/queryset/mod.rs`.
3. **Multipart parsing** (`crates/umbral-core/src/web/multipart.rs`, `parse_multipart` and `parse_multipart_capped`). This one takes raw wire bytes, which is the canonical fuzz target.
4. **Settings and env parsing** (`crates/umbral-core/src/settings.rs`). The figment `Env` / `Toml` merge, and specifically `deserialize_string_list` (the comma-vs-bracket coercion for `UMBRAL_ALLOWED_HOSTS` and friends) plus the byte-cap defaults like `default_max_form_body_bytes`.

Route-pattern matching and the raw SQL builders named in the gap are folded into targets 1 and 2 rather than given their own harnesses: route patterns exercise the same string-to-matcher path, and the SQL builders are what the predicate target already renders through.

### Tooling choice

Use **proptest** for targets 1, 2, and 4, and **cargo-fuzz (libFuzzer)** for target 3. The split is deliberate. proptest wants a value generator and shrinks to a minimal failing case, which fits structured inputs (a generated `Snapshot`, a tree of `Predicate<T>`, a set of env pairs) where we can assert a round-trip invariant. cargo-fuzz wants a raw `&[u8]`, which fits multipart bytes and needs a nightly toolchain plus a separate `fuzz/` crate, so it stays out of the default `cargo test` path and runs on its own CI cadence. proptest targets live in the normal `#[cfg(test)]` suites of `umbral-core` and run for free on every `cargo test`.

### The invariants (this is the actual work)

A fuzz target is only as good as the property it asserts. For each surface:

- **Autodetector round-trip (the headline property).** For an arbitrary pair of valid model states, `apply(diff(prev, curr))` starting from `prev` must produce a database whose introspected shape equals `curr`. Concretely the proptest generates a starting `Snapshot`, applies a random sequence of edits (add model, drop model, add or drop or alter a field, rename a table, add an M2M) to produce a second `Snapshot`, runs `diff` to get `Vec<Operation>`, applies those operations against an in-memory SQLite from the `prev` schema, and asserts the resulting live schema matches `curr`. Secondary invariants on the same generator: `diff(s, s)` is empty for every `s` (no spurious ops), every emitted `Vec<Operation>` is orderable without a dependency cycle, and `diff` never panics (it returns `Err(MigrateError)` or `Ok`).
- **Predicate render safety.** For an arbitrary tree of `Predicate<T>` combinators, `to_sql()` and `to_sql_pg()` both return a string with balanced parentheses and every literal bound as a parameter placeholder, never inlined. The property that catches the real bug: no generated predicate produces raw SQL where a string literal escapes the parameter list (a parameterization regression is a SQL-injection regression). Assert placeholder count equals the number of bound values.
- **Multipart never panics and honours the cap.** For arbitrary bytes with an arbitrary boundary, `parse_multipart_capped(bytes, cap)` returns `Ok` or `Err` but never panics, never allocates beyond the cap, and when it returns `Ok` the summed field sizes are within the cap. This is the classic "malformed body from the internet" target.
- **Settings coercion is total and idempotent.** For arbitrary env maps over the known `UMBRAL_*` keys, loading `Settings` either succeeds or returns a typed error, never panics; and `deserialize_string_list` treats `"a,b"` and `["a","b"]` as equal for every input, with whitespace trimmed and empty entries dropped (the documented contract in `settings.rs`).

### Cost and cadence

proptest targets are ordinary tests, cheap, and gate every commit at a low case count (256 cases) with a longer nightly run (a higher `PROPTEST_CASES`). The cargo-fuzz target needs a nightly toolchain and a `crates/umbral-core/fuzz/` crate; it runs as a scheduled CI job (a fixed wall-clock budget per target, corpus persisted between runs), not on the PR path. A regression corpus of every historical crash goes under `fuzz/corpus/` and is replayed as a fast deterministic test on every commit, so a fixed crash never comes back.

## #96: Upgrade compatibility tests (golden projects)

### The gap in one sentence

`STABILITY.md` promises the Stable tier does not break in a patch and ships a migration note for every Stable break in a minor, and it names this suite (#96) as a 1.0 gate, but nothing today proves a project scaffolded and migrated against 0.0.N still runs `makemigrations` and `migrate` cleanly on 0.0.N+1. This ties to gaps5 #28 (the migration-drift and forward-compat concern) and is the enforcement arm of the stability policy.

### Golden fixtures

A golden fixture is a frozen, committed snapshot of a real generated project at a known release:

- Generated by `umbral startproject <name>` (see `crates/umbral-cli/src/scaffold.rs`) at release 0.0.N, with a handful of representative models added and migrated, so the fixture carries real `migrations/*` files and a real snapshot, not an empty skeleton.
- Committed under `tests/golden/<release>/<project>/` including its `migrations/` directory and the model source. The migration files are the audit trail the CLAUDE.md migration rules protect, so these fixtures are never regenerated in place: a new release adds a new `tests/golden/0.0.N+1/` tree, it does not overwrite the old one.
- One fixture per prior release we still claim to support, growing by one tree per release.

### The CI test

For each golden fixture, in CI, against the current workspace crates (path deps to the local umbral):

1. Copy the fixture project to a temp dir (never mutate the committed fixture).
2. Point it at a fresh throwaway database seeded with a few rows, because per CLAUDE.md existing rows are the test, not an obstacle. A NOT NULL add, a new UNIQUE, or a cross-plugin FK ordering bug only shows up against populated tables.
3. Run `umbral migrate` to bring the old project's recorded migrations up on the current engine (proves old migration files still apply and deserialize under the current `Operation` enum, which is why the enum carries `serde(default)` on its newer fields).
4. Run `umbral makemigrations` and assert it emits **no** new migration, which proves the current autodetector reads the old snapshot as up to date and has not drifted (this is the #28 forward-compat assertion).
5. Add one documented model change, run `makemigrations` then `migrate`, and assert both succeed against the seeded rows.

Step 3 catches on-disk format breaks (a removed or renamed `Operation` variant, a changed field), step 4 catches autodetector drift (the engine inventing spurious diffs against stable models), and step 5 catches a broken upgrade of the everyday declare-migrate-change-migrate loop itself.

### Maintenance rule

Cutting a release adds that release's tree under `tests/golden/`. Fixtures are additive and frozen; the only edits allowed are to the copied working tree inside the test, never to the committed originals. When a Stable break is intentional and documented in the changelog per `STABILITY.md`, the corresponding golden test is updated in the same commit with a comment pointing at the changelog migration note, so an intentional break is visible and a silent one fails CI.

## #97: Load and soak tests

### Honesty up front

Unlike #95 and #96, this cannot live entirely in `cargo test`. Meaningful load and soak numbers need a running app on representative hardware, a real Postgres, a load generator, and time. This section specifies reproducible scenarios and a harness; running them at full scale and publishing results is an infrastructure task that depends on a CI runner or a dedicated box we do not yet operate. What we can commit now is the scenario definitions, the target app, and small-scale smoke versions that run in CI to catch gross regressions, with the full-scale profiles gated behind a manual or scheduled job.

### Tooling

Use **k6** for HTTP and WebSocket scenarios (it scripts WebSocket fanout natively, which vegeta cannot) and **oha** for raw HTTP throughput smoke tests (a single static binary, trivial to run in CI for a quick numbers check). This is the k6/vegeta/Locust family the gap names, narrowed to two tools: k6 for the scripted scenarios, oha for the throughput smoke. Scenarios live under `tests/load/` as committed scripts so a result is reproducible from the repo.

### The scenarios

Each targets a scaling axis the gap calls out, run against a purpose-built load example app (an `examples/loadtest/` consumer wiring the realtime, tasks, storage, and admin plugins):

1. **WebSocket fanout.** k6 ramps to a large number of concurrent WebSocket clients subscribed to one channel; a publisher pushes at a fixed rate; measure delivery latency percentiles and dropped connections as client count climbs. The 10k-client figure from the gap is the stretch target for the full profile; the CI smoke runs a small fraction to catch a fanout regression.
2. **Queue backlog drain.** Enqueue a large backlog into the `umbral-tasks` DB queue with workers stopped, then start N workers and measure drain throughput (jobs per second) and time to empty, plus whether the DB-backed queue's polling keeps up without lock contention.
3. **Large-table admin list.** Seed a table with a large row count, then drive the admin changelist (pagination, sort, filter, search) and measure p95 response time as row count grows, which exercises the QuerySet `LIMIT`/`OFFSET`/count path at scale and surfaces missing indexes.
4. **Upload throughput.** k6 uploads files of varied sizes through the storage plugin concurrently; measure sustained MB/s, error rate under concurrency, and memory behaviour (the multipart cap from #95's target 3 is the guard this validates under real load).
5. **Large migration.** Time a `migrate` that adds a column and an index to a table with a large row count, which is the operational reality of shipping a schema change to a populated production DB; assert it completes and record the wall clock as a tracked number.

### The soak profile

Separate from the ramp scenarios above, a soak profile runs scenarios 1, 2, and 4 concurrently at a moderate steady load for an extended duration (hours) and watches for the failures that only appear over time: memory growth (a leak in the connection pool or the WebSocket registry), file-descriptor exhaustion, connection-pool starvation, and latency creep. The soak asserts on stability of the numbers over time, not peak throughput.

### What ships now versus later

Now: the `examples/loadtest/` app, the committed k6 and oha scripts, and CI smoke versions at small scale that fail on a gross regression. Later, gated behind a scheduled or manual CI job on real infrastructure: the full-scale runs (the 10k-client fanout, the multi-hour soak) with results published to a tracked location so numbers are comparable release over release. This staging is the honest split: the scenarios are code we can write and review today; the full-scale numbers wait on infra.

## Relationship to the rest of the backlog

- #95 and #98 (the security regression suite) share the parameterization and multipart invariants; the fuzz targets here feed the security suite there rather than duplicating it.
- #96 is the enforcement mechanism for `STABILITY.md` and is a named 1.0 gate; it also closes the forward-compat concern in #28.
- #97's smoke tier can gate CI; its full-scale tier is explicitly infrastructure-dependent and does not gate anything until that infrastructure exists.
