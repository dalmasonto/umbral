# review_4 · umbral-health

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 926 LOC (`plugins/umbral-health`) |
| **Reviewer** | Claude (rev4 wave) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 0 medium · 1 low · 0 info — 1 new, 2 known |

## Summary

- `plugins/umbral-health` is the probe pair: `GET /healthz` (liveness — always 200, never touches a dependency) and `GET /ready` / `GET /readyz` (readiness — DB ping via `umbral::db::ping()`, an opt-in pending-migrations gate over `umbral::migrate::drift_report()`, then every registered `HealthCheck`, each bounded by a 5 s default timeout, with a shutdown-drain short-circuit). Handlers + 4 behavioral test files, 926 LOC total.
- Overall state is good and the prior sweeps' fixes verified held in code: HE-1's raw `sqlx::query("SELECT 1")` is now `umbral::db::ping()` (the crate has no sqlx dependency at all), HE-2's missing per-check timeout is now applied to all three probe kinds and tested for promptness, and the review_3 squash-aware drift fix in core is present with a comment naming health readiness as the motivation.
- The one new finding is small (HEALTH-1, LOW): duplicate `HealthCheck::name()` values silently collide in the readiness JSON — the last check's result overwrites the earlier one's, so the body can report every listed dependency `"ok"` while the status code is 503 and a failing check's name/reason has vanished. Fail-closed on the *decision*, misleading on the *diagnosis*; a build-time duplicate check closes it.
- Two tracked design items remain open (HE-3: probes are unconditionally public with no allowlist option — documented posture; HE-4: checks run sequentially, and the in-tree comment still justifies it with a backwards rationale).
- Fix order: HEALTH-1 (reject duplicate names in `HealthPlugin::check`), then, when the crate's next pass opens, HE-4's comment/rationale and the optional HE-3 allowlist from the existing tracker entry.

## Findings

### HEALTH-1 · LOW · correctness/ergonomics · self-verified
**Two `HealthCheck`s that return the same `name()` silently collide in the readiness body: `checks.insert(name, …)` overwrites the earlier entry while `all_ok` is a separate flag, so `/ready` can answer 503 with a body in which every listed check says `"ok"` — the failing dependency's name and reason are gone.**

- **Where**: `plugins/umbral-health/src/lib.rs:371-394` (the check loop and its three `checks.insert` sites at `:375`, `:380-383`, `:388-391`); `:181-184` (registration with no uniqueness validation); `:92-97` (trait doc).
- **Evidence**:
  ```rust
  // lib.rs:93-97 — the contract operators are told to rely on:
      /// Short stable identifier surfaced in the readiness JSON
      /// (`"redis"`, `"stripe"`, `"s3"`). Mostly free-form;
      /// operators key off this when alerting.
      fn name(&self) -> &'static str;
  ```
  ```rust
  // lib.rs:181-184 — registration never validates the name:
      pub fn check<C: HealthCheck>(mut self, check: C) -> Self {
          self.checks.push(Arc::new(check));
          self
      }
  ```
  ```rust
  // lib.rs:371-383 — the map key is the name; a later check overwrites the earlier:
      for check in state.checks.iter() {
          let name = check.name().to_string();
          match tokio::time::timeout(timeout, check.check()).await {
              Ok(Ok(())) => {
                  checks.insert(name, serde_json::json!({"status": "ok"}));
              }
              Ok(Err(e)) => {
                  tracing::warn!(check = %check.name(), reason = %e, "health: check failed");
                  all_ok = false;
                  checks.insert(
                      name,
                      serde_json::json!({"status": "fail", "reason": e.reason}),
                  );
              }
  ```
  with the verdict decided independently of the map:
  ```rust
  // lib.rs:396-405 (trimmed):
      let status_code = if all_ok {
          StatusCode::OK
      } else {
          StatusCode::SERVICE_UNAVAILABLE
      };
  ```
- **Impact**: an app registers two checks named `"redis"` — a realistic copy-paste or class-of-two-clients case (e.g. the cache client and the broker client both report through a check whose author picked the obvious string; the trait doc calls names "mostly free-form", so nothing steers away from it). If the first fails and the second passes, `all_ok` is `false` (503, `"status":"fail"`) but the second `insert` replaces the first's entry, so the body reads `"checks":{"redis":{"status":"ok"}}` — the on-call sees a 503 whose visible detail is entirely green, and any alerting keyed on the documented per-name shape (`checks.redis.status`) never fires because the field says `ok`. The failure is fail-closed (readiness is correctly withheld), so the cost is diagnostic, not availability: the specific failing dependency and its redacted reason are unrecoverable from the probe (the `tracing::warn!` line still names it server-side, but the probe surface — the thing the doc says operators key off — lies). The reverse collision (first ok, second fail) happens to read correctly, which is what makes this an intermittent debugging trap rather than a flatly broken surface. LOW: requires an author error to trigger, bounded to the admin/diagnostics surface, one-line fix.
- **Fix**: validate at build — in `HealthPlugin::check`, reject or rename a duplicate `name()` (panic with the colliding name, matching the framework's other build-time contract checks) or suffix it deterministically (`"redis#2"`); optionally unit-test two same-named checks in `lib.rs`'s `#[cfg(test)]` block next to the `evaluate_migrations` tests.

## Strengths

- **The liveness/readiness split is real, not cosmetic.** `/healthz` returns 200 unconditionally and never touches the pool or the registry of checks (`lib.rs:259-263` — "liveness is 'the process answered the syscall', nothing more"; the handler takes no state extractor at all, so it *cannot* consult the database); `/ready` aggregates DB + opt-in migrations + registered checks and returns 503 on any failure. The k8s alias `/readyz` is the same handler, and `routes()`/`route_paths()` list exactly the same three routes (verified pairwise).
- **Every probe path is bounded.** `check_timeout` (default `DEFAULT_CHECK_TIMEOUT = 5 s`, `lib.rs:106-107`) wraps the DB ping, the migration drift report, and every registered check; a timed-out check is recorded as `"fail"`/`"timed out"` and the response returns promptly — pinned by `tests/ping_and_timeout.rs::slow_check_is_recorded_as_timed_out_and_runner_returns_promptly`, which asserts wall-clock promptness against a 30 s sleeper with a 200 ms budget. This is HE-2's fix verified at the behavioral level, not just present.
- **Error-text posture is deliberate and commented.** Real errors go to `tracing::warn!` server-side; the unauthenticated body carries only `"unavailable"` / `"timed out"` (with the audit_2 #3 reference in the comment) — the DSN/host-leak class is closed everywhere the plugin touches the DB.
- **Shutdown drain is checked first and short-circuits everything** (`lib.rs:282-295`), which is what makes `AppBuilder::shutdown_drain` a usable zero-downtime-rollout primitive; tested live (`tests/shutdown_readiness.rs`).
- **Migration gating is a pure, unit-tested rule.** `evaluate_migrations` (`lib.rs:133-140`) blocks only on `Pending` — the rollback-friendly choice (an ahead-of-code DB stays ready) and the squash case is handled in core with a comment naming health readiness as the consumer (`crates/umbral-core/src/migrate.rs:3346-3358`). Opt-in (`require_migrations()`), default off, five focused unit tests including the squash-shadow and out-of-order cases.
- **Tests are behavioral.** A real SQLite pool booted through `App::builder()` and real router calls via tower oneshot (`tests/integration.rs`, `tests/migration_readiness.rs`, `tests/ping_and_timeout.rs`, `tests/shutdown_readiness.rs`) — no mock proxies; the timeout test measures elapsed time; the drain test flips real global state.
- **Fix-don't-patch was followed literally.** HE-1's workaround-grade raw SQL was replaced by an ORM-surface addition (`umbral::db::ping()`, `crates/umbral-core/src/db.rs:309-314`) rather than a local cfg or a comment — the crate now has zero database-driver dependencies.

## Clean areas checked

- **Row-level SQL**: `grep -rn "sqlx::query" plugins/umbral-health/src/` → no hits (verified); `Cargo.toml` declares no sqlx at all — the two DB touchpoints are `umbral::db::ping()` and `umbral::migrate::drift_report()`, both facade surfaces.
- **DB outage does not hang the probe**: the ping is wrapped in `tokio::time::timeout(timeout, …)` → after 5 s the body is `{"database":{"status":"fail","reason":"timed out"}}` with 503; the pool acquire behind `ping()` is cancellation-safe, so the timeout actually cancels rather than leaking a task. No `unwrap`/`expect` anywhere in `src/`, no `unsafe`.
- **Information disclosure**: no DSN, host, user, file path, row data, or raw error text in any probe body — the DB and migration arms redact to generic reasons (server-side log keeps the detail), a pending-migration reason carries only a count (`"1 migration pending"` / `"{n} migrations pending"`), and a developer check's `reason` is the check author's own opt-in text (documented). Check names are intentionally public and documented as such (`lib.rs:142-149`: routes "never carry authentication — by design").
- **Auth posture**: the three routes are mounted unconditionally and unauthenticated for LB/k8s reachability — a documented design decision, tracked (HE-3 below); no CSRF/state-changing surface exists behind them (GET-only), and the routes expose no mutation.
- **Liveness-vs-readiness correctness**: `/healthz` cannot fail on a DB outage (never calls it — asserted by test); `/ready` fails on DB error/timeout, any check error/timeout, pending migrations when opted in, and draining; `/ready` stays 200 when the DB is *ahead* of the binary (rollback case) — unit-tested.
- **`routes()`/`route_paths()` sync**: three paths each, identical strings and methods (verified line by line).
- **The timeout plumbing** reaches every await point inside the handler (DB, migrations, per-check) — no unbounded await besides the infallible `is_draining()` flag read.
- **Docs**: crate-level docs (`lib.rs:1-27`) describe exactly what the handlers do (liveness unconditional, readiness aggregation, drain, opt-in migrations); `check_timeout`'s and `require_migrations`'s doc claims match the implemented behavior.

## Known issues already tracked elsewhere (not counted in the totals)

- `hardening/plugins-review umbra-health HE-1` — raw `sqlx::query("SELECT 1")` for the DB probe `[fixed/verified held]` — now `umbral::db::ping()` (`lib.rs:297-300`); the crate has no sqlx dependency.
- `hardening/plugins-review umbra-health HE-2` — no per-check timeout `[fixed/verified held]` — `check_timeout` (5 s default) wraps DB, migrations, and every check; tests assert prompt return (`tests/ping_and_timeout.rs`).
- `hardening/plugins-review umbra-health HE-3` — probes are unconditionally public with no allowlist option, and the body enumerates registered check names `[open]` — unchanged at this commit (the raw-error-leak half of the old finding is fixed via audit_2 #3's generic reasons). Design posture, documented; the optional `allowlist` idea from the entry still stands.
- `hardening/plugins-review umbra-health HE-4` — checks run sequentially; the comment justifying it ("concurrency would multiply tail latencies") argues the opposite of the usual trade-off `[open]` — unchanged at `lib.rs:367-370`; per-check timeouts bound the worst case to `n_checks × timeout`, so the status quo is fail-safe but the comment should be corrected or the loop parallelized.
- `review_3` (health readiness vs squash) — squash-aware drift in core `[fixed/verified held]` — `detect_drift`'s `shadowed` set built from applied squashes' `replaces` (`crates/umbral-core/src/migrate.rs:3346-3358`, classification `:3383-3386`), with the health-readiness comment in place.
- `audit_2 plugin-observability #3` — `/ready` leaked the raw DB error `[fixed/verified held]` — generic `"unavailable"`/`"timed out"` reasons plus server-side `tracing::warn!` (`lib.rs:303-321`, `:346-363`).

## Out-of-scope notes

- `drift_report()` resolves `migrations/` relative to the process CWD (`MIGRATIONS_DIR = "migrations"`, `crates/umbral-core/src/migrate.rs:1721` and `:3678`): a server started outside the app root reads an empty tree, so `require_migrations` silently reports ready. The consequence surfaces in health, but the contract (where the migrations dir is resolved from) is core's — core reviewer.
- `umbral::db::ping()` semantics (`crates/umbral-core/src/db.rs:309-314`) are the ORM surface health now depends on; any change there (return shape, timeout behavior) is core's.
- `umbral::shutdown::is_draining()` is a one-way flag owned by core (health consumes it and its drain test depends on the never-reset behavior) — core reviewer if its lifecycle is ever reworked.
