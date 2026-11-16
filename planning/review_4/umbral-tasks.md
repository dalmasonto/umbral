# review_4 · umbral-tasks

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 5803 LOC (`plugins/umbral-tasks`; src 2199 + tests 3604) |
| **Reviewer** | review_4 agent |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 2 high · 4 medium · 2 low · 0 info — 8 new, 7 known |

## Summary

- umbral-tasks is the DB-backed background queue: `TaskRow` as the broker table, a polling worker (`run_worker`), a cron/interval beat (`run_beat`), enqueue/claim/retry/reclaim plumbing, a result backend, and optional admin + auth-mailer surfaces.
- It is well built where history already bit it: the double-claim guard (conditional UPDATE inside the claim transaction + `for_update_skip_locked`), the TOCTOU-safe orphan reclaim, handler-panic containment, and the no-`process::exit` shutdown are all present and behaviourally tested. The ORM-only rule is held absolutely — zero `sqlx::query` in `src/`.
- Two high-priority problems: `task_timeout` **does not cancel** the handler (dropping a `JoinHandle` detaches it), so a timed-out handler keeps running while its retry starts; and `periodic_admin_model()` lost the `#[cfg(feature = "admin")]` gate, so the documented `default-features = false` slim build no longer compiles.
- Beat-side durability is weaker than the worker side: a schedule fire is claimed and enqueued in two separate statements (a crash between them silently loses that occurrence), and a `.periodic()` spec removed from code keeps firing its row forever, accumulating `HandlerNotFound` failures.
- New since the last sweep: nothing regressed on the previously fixed items; the two HIGHs are new code paths (per-task timeout; periodic admin model) whose contracts drifted from their docs.
- Fix first: TASKS-1 (`join.abort()`, or correct three doc sites), then TASKS-2 (one cfg attribute + a no-default-features build check).

## Findings

### TASKS-1 · HIGH · async correctness · self-verified
**`task_timeout` never cancels the handler — dropping the `JoinHandle` detaches it, so a timed-out handler keeps running and can execute concurrently with its own retry; the code comment, the rustdoc, and the user docs all claim it aborts.**

- **Where**: `plugins/umbral-tasks/src/lib.rs:1151-1158` and `:1172-1175`; the same premise is restated at `:821-835` (rustdoc), `:41-44` (module doc), and in `documentation/docs/v0.0.1/plugins/tasks.mdx` ("Per-task timeout": "A handler that overruns `WorkerOptions::task_timeout` is cancelled…").
- **Evidence**:
  ```rust
  // Catch panics so one bad handler doesn't take the worker
  // down. `tokio::task::spawn` gives us the JoinHandle whose
  // join error carries panic payloads we can stringify. Wrap the
  // spawned handle in `tokio::time::timeout` so an overrunning
  // handler is cancelled (its task dropped) and recorded as a
  // retriable failure rather than pinning the worker.
  let join = tokio::task::spawn(fut);
  let outcome = match policy.task_timeout {
      Some(limit) => tokio::time::timeout(limit, join).await,
  ```
  ```rust
  Err(_elapsed) => {
      // Timed out. The `JoinHandle` is dropped here, which
      // aborts the still-running handler task. Treat as a
      // retriable failure (backed off below).
  ```
  tokio disagrees — `tokio-1.52.3/src/runtime/task/join.rs:35-36`, the version Cargo.lock pins: "If a `JoinHandle` is dropped, then the task continues running in the background and its return value is lost." Nothing in the crate ever calls `join.abort()` (grep: "abort" appears only in these comments).
- **Impact**: `task_timeout: Some(30s)`, a handler that takes 90 s (blocked SMTP send, slow HTTP call), `max_attempts: 3`, default 2 s backoff. At t=30 s the worker records a retriable failure and sets `run_at = t+2s`; at t=32 s another `send_welcome_email` invocation starts while the first is still inside the handler, and every further retry overlaps again. Because no cancellation is ever requested, the rustdoc caveat ("Cancellation is cooperative… only takes effect at an `.await` point") is moot: even a perfectly yielding async handler runs to completion. `tests/reliability.rs:383-384` ("the timeout cancels it at ~50ms") asserts only the row state, so the suite passes whether or not anything is cancelled.
- **Fix**: call `join.abort();` in the `Err(_elapsed)` arm before recording the timeout — that yields exactly the cooperative cancellation the docs describe. If detach-and-continue is actually intended, then fix the three doc sites plus the test comment to say the handler keeps running and may overlap its retry.

### TASKS-2 · HIGH · build / feature-flag hygiene · self-verified
**`periodic_admin_model()` is missing `#[cfg(feature = "admin")]`, so the documented `default-features = false` tasks-only build does not compile.**

- **Where**: `plugins/umbral-tasks/src/lib.rs:1977-1980`, against the correctly gated `admin_model()` at `:1877-1878`.
- **Evidence**:
  ```rust
  #[cfg(feature = "admin")]
  pub fn admin_model() -> umbral_admin::AdminModel {
  ```
  ```rust
  pub fn periodic_admin_model() -> umbral_admin::AdminModel {
      use umbral_admin::{Action, ActionResult, ActionScope, AdminModel, ToastLevel};
  ```
  and `plugins/umbral-tasks/Cargo.toml:33-38`: "turn it off (`default-features = false`) for a slim tasks-only build that doesn't pull the admin into its dependency graph." `grep -n "cfg(" src/lib.rs` finds gates only at 1877, 2075, 2157, 2177, 2195 — the function at 1979 and its inner `use` are ungated (no enclosing cfg'd module, no `cfg_attr`). `features.md #82`'s "base build verified" claim predates this function.
- **Impact**: any app that follows the advertised slim build (`umbral-tasks = { ..., default-features = false }`) gets a hard compile error from the plugin's own source — `error[E0433]: failed to resolve: use of undeclared crate or module 'umbral_admin'` at `lib.rs:1979` — while every default-features build and test stays green, so CI cannot see it.
- **Fix**: add `#[cfg(feature = "admin")]` to `periodic_admin_model` (same gate as `admin_model`), and add `cargo check -p umbral-tasks --no-default-features` to CI so the next ungated reference fails there instead of in a consumer.

### TASKS-3 · MEDIUM · async/perf · self-verified
**A closed shutdown channel turns the worker/beat sleep into a hot DB-poll loop — the "worker tolerates a closed channel" comment has it backwards.**

- **Where**: `lib.rs:892-895` and `:901-904` (`run_worker`), `:1692-1695` (`run_beat`); the comment claiming it is handled at `:843-847`.
- **Evidence**:
  ```rust
  // Leak the sender so the receiver stays alive without anyone
  // holding a reference. The worker tolerates a closed channel
  // (treats it as no shutdown), but leaking keeps the contract
  // simple: the channel never closes.
  std::mem::forget(_tx);
  ```
  ```rust
  tokio::select! {
      _ = tokio::time::sleep(opts.poll_interval) => {}
      _ = opts.shutdown.changed() => {}
  }
  ```
  In the pinned tokio, `Receiver::changed()` on a closed channel is ready immediately with `Err` (`tokio-1.52.3/src/sync/watch.rs:975-978`: `if state.is_closed() { return Some(Err(...)) }`), so the arm fires at once on every iteration.
- **Impact**: a caller that builds its own `watch::channel(false)` and drops the sender without sending (a shutdown controller dropped at teardown, a `(tx, rx)` where `tx` is never stored) leaves the receiver closed with value `false`. `*opts.shutdown.borrow()` stays `false`, the select never sleeps, and the loop spins at full speed — a reclaim SELECT, a claim SELECT, and (beat) a due-schedule scan per iteration, indefinitely. The default `WorkerOptions` masks this only via the leaked sender, and no test or system check exercises the closed case the comment says is handled.
- **Fix**: treat `Err` from `changed()` as shutdown: `res = opts.shutdown.changed() => { if res.is_err() { return; } }` (or fall back to a slow re-armable sleep). Apply the same guard at all three `select!` sites.

### TASKS-4 · MEDIUM · semantics/docs · self-verified
**Nothing renews a running task's lease, but the docs say the visibility timeout only reclaims "after a crashed worker stops renewing its lease" — a live worker's long-running handler is reclaimed and re-run concurrently.**

- **Where**: `lib.rs:825-827` (rustdoc), reclaim predicate at `:937-938`.
- **Evidence**:
  ```rust
  /// This is
  /// distinct from `visibility_timeout`: the timeout fails a *running*
  /// handler promptly, whereas the visibility timeout only reclaims a row
  /// after a *crashed* worker stops renewing its lease.
  ```
  ```rust
  let orphans: Vec<TaskRow> = TaskRow::objects()
      .filter(task_row::STATUS.eq(STATUS_RUNNING) & task_row::STARTED_AT.lt(cutoff))
      .fetch()
  ```
  `grep -n "heartbeat\|renew" src/lib.rs` matches only that doc sentence — `started_at` is stamped once at claim (`:1105`) and never refreshed while the handler runs, so the reclaim can only distinguish "crashed" from "busy" by age. `tasks.mdx` repeats the crash-only framing ("If the worker process crashes mid-handler … The visibility timeout closes this gap").
- **Impact**: `task_timeout: None` (documented as supported, and the natural choice for a legitimately long job) or `task_timeout > visibility_timeout`, 2+ worker processes, a 10-minute handler: at t=5 min (the default `visibility_timeout`) worker B's reclaim pass finds `started_at < now - 5min`, resets the row to pending, claims it, and runs a second copy while worker A's handler is still alive — duplicate side effects on a healthy system, with the docs steering the operator away from suspecting it.
- **Fix**: renew `started_at` on a heartbeat while a task runs (making the doc true and the timeout lease-based); failing that, warn at boot when `visibility_timeout` cannot cover `task_timeout` and correct the "stops renewing its lease" wording to "reclaims any run longer than `visibility_timeout`, crashed or not".

### TASKS-5 · MEDIUM · correctness · self-verified
**A beat fire can be lost: the optimistic claim commits, then the enqueue is a separate statement — a crash or an error in between advances `next_run` with no task row.**

- **Where**: `lib.rs:1746-1756` (`fire_due_periodic`), `:1770-1785` (`enqueue_periodic`).
- **Evidence**:
  ```rust
  let affected = PeriodicTask::objects()
      .filter(periodic_task::ID.eq(row.id) & periodic_task::NEXT_RUN.eq(row.next_run))
      .update_values(patch)
      .await?;
  if affected == 1 {
      enqueue_periodic(&row).await?;
      fired += 1;
  }
  ```
- **Impact**: the claim UPDATE and the `task_row` INSERT are two independent commits (both ambient-pool ORM terminals). If the beat process dies between them (deploy, OOM, `kill -9`), `next_run` has already moved to the following occurrence and `last_run` is stamped — that occurrence fires nothing, and unlike the worker side there is no reclaim that could recover it. The same loss happens if the INSERT errors: `?` propagates, `fire_due_periodic` returns Err, and the claimed row's occurrence is gone (the log line is only "beat tick failed").
- **Fix**: run the claim UPDATE and `enqueue_periodic` inside one `db::transaction`, so a crash rolls the claim back and the next beat start re-fires the same occurrence (at-least-once), instead of silently skipping it.

### TASKS-6 · MEDIUM · lifecycle
**Removing a `.periodic()` spec never disables its row — the schedule keeps firing forever, enqueuing a handler that may no longer exist.**

- **Where**: `lib.rs:1585-1590` (`sync_periodic_specs` upsert loop) and `:1721-1726` (due query reads whatever is `enabled`).
- **Evidence**:
  ```rust
  for spec in specs {
      let storage = spec.schedule.to_storage();
      let existing = PeriodicTask::objects()
          .filter(periodic_task::NAME.eq(spec.name.as_str()))
          .first()
          .await?;
  ```
  The loop only inserts unseen names and updates present ones; nothing looks at rows whose `name` is absent from `specs`. The function doc calls it "Idempotent", and the periodic admin's doc says everything but `enabled` "is declared in code via [`TasksPlugin::periodic`] and re-synced at boot" (`:1974-1976`) — so a row with no corresponding spec is stale by the crate's own model.
- **Impact**: delete (or rename) the `.periodic(...)` registration and the old row stays `enabled = true`: beat keeps firing it, `enqueue_periodic` inserts a `task_row` for the old task name, and once the handler went with the spec the worker marks each fire non-retriably failed (`HandlerNotFound`) — one junk failed row per tick, forever, while the admin's Schedules page shows an enabled schedule no code declares. Renaming produces two firing rows. Workaround: disable manually in the admin.
- **Fix**: after the upsert pass, disable (and log once) every `PeriodicTask` whose `name` is not in `specs` — `sync_periodic_specs` already holds the full slice, so the set difference is local. If hand-created rows are meant to be supported, document that removal must be paired with a manual disable instead.

### TASKS-7 · LOW · docs
**A doc-comment was split across two items: `Task`'s rustdoc opens with half of `EnqueueOptions`' summary.**

- **Where**: `lib.rs:512-513` and `:549`.
- **Evidence**:
  ```rust
  /// Options for [`enqueue`]. Both fields are optional with sensible
  /// A task, identified by its type rather than by a string (gaps3 #48).
  ```
  ```rust
  /// defaults: 3 attempts and immediate execution.
  #[derive(Debug, Clone, Default)]
  pub struct EnqueueOptions {
  ```
- **Impact**: docs.rs renders `Task` opening with a dangling fragment about enqueue options, and `EnqueueOptions` opening with the sentence's tail; neither public item's summary reads as a sentence.
- **Fix**: restore `/// Options for [`enqueue`]. Both fields are optional with sensible` + `/// defaults: 3 attempts and immediate execution.` above `EnqueueOptions` and delete the stray first line from `Task`.

### TASKS-8 · LOW · docs
**Cron schedules are evaluated in UTC and neither the rustdoc nor `tasks.mdx` says so.**

- **Where**: `lib.rs:1421-1428` (format docs), `:1456-1462` (`next_after`), `:1720`.
- **Evidence**:
  ```rust
  pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
      match self {
          Schedule::Cron(expr) => {
              let normalized = normalize_cron(expr);
              let schedule = cron::Schedule::from_str(&normalized).ok()?;
              schedule.after(&after).next()
  ```
  Neither the `Schedule` rustdoc (1421-1428: format, prepended seconds field, serialized form) nor the schedules section of `documentation/docs/v0.0.1/plugins/tasks.mdx` mentions a timezone (`grep -i "utc\|timezone"` over the page: no match in that section).
- **Impact**: an operator schedules `Schedule::cron("0 9 * * *")` for the 9 am digest and gets 09:00 UTC — 12:00 in EAT, 02:00 in PDT. Nothing in the admin's `next_run` rendering or the docs explains the zone, so the first symptom is a job that "fires at the wrong time". (The broader operator-grade scheduler work — timezone UX, missed-run policy, pause/resume — is tracked at `gaps5 #56`; this specific doc gap is not.)
- **Fix**: one sentence on `Schedule` and in `tasks.mdx`: expressions are matched against UTC; convert inside the handler or schedule in UTC terms.

## Strengths

- **The claim path is genuinely race-correct and documented**: `claim_one` (`:1044-1129`) runs one `umbral::transaction` holding a `SELECT … order_by(priority DESC, scheduled_for ASC, id ASC) … .limit(1).for_update_skip_locked()` and then a conditional UPDATE `WHERE id = ? AND status = 'pending'` whose `affected == 0` loses cleanly — the BROKEN-1/audit_2 #6 fix, pinned by `tests/concurrency_panic.rs` (deterministic double-claim) and `tests/claim_index.rs` (index render on both backends).
- **The orphan reclaim is TOCTOU-safe by construction**: the reset UPDATE re-asserts `status = running AND started_at < cutoff` (`:985-995`), so a worker finishing between the SELECT and the UPDATE can't be clobbered, and reclaimed-not-exhausted rows get the same exponential backoff as handler failures.
- **Failure classification is typed, not string-matched**: `HandlerNotFound` is explicitly non-retriable (`:1223-1232`) so a typo'd task name can't burn `max_attempts`; panics surface as `HandlerPanicked` via `JoinError::is_panic` (`:1164-1167`).
- **Backoff arithmetic is hardened** (`:790-794`: saturating shift + `checked_mul` + clamp) and threaded through a single `RetryPolicy` shared by `process_one` and `reclaim_orphaned_tasks_with`.
- **The ORM-only rule is held absolutely**: `grep -rn "sqlx::query\|query_as\|query_scalar" plugins/umbral-tasks/src/` → zero hits; raw SQL appears only in tests (the blessed pattern). Priority NULL handling is solved at write time rather than by a raw-SQL COALESCE, and the trade-off is spelled out in a 25-line comment (`:1057-1081`).
- **Sharp edges are documented rather than hidden**: plaintext-payload warning (rustdoc `:597-601` + a `tasks.mdx` callout), warn-on-unpersisted-`timeout` (gaps4 #29), and the `attempts`-incremented-at-claim trade-off (audit_2 #10).

## Clean areas checked

- Multi-worker double-claim (review_1 BROKEN-1, audit_2 #6): held — conditional UPDATE + `for_update_skip_locked` inside the claim transaction; race test present.
- Worker-crash reclaim / visibility timeout (review_1 BROKEN-2): held — runs every worker iteration before claiming (`:882-886`); conditional re-assert prevents clobbering.
- `run_worker` shutdown via `process::exit` (review_1 BROKEN-4): held — returns; no `process::exit` anywhere in `src/`.
- review_3/codex #17 (per-task timeout is API-only): held — `enqueue` warns when `opts.timeout.is_some()` (`:608-620`).
- SQL injection: not applicable — no raw SQL in `src/`; every value is bound by the ORM.
- SQLite/Postgres parity of the claim query: the conditional UPDATE guards both; SKIP LOCKED is a no-op on SQLite by design (single writer); `tests/claim_index.rs` checks both renderings.
- Beat double-fire guard: optimistic claim on `next_run` is race-correct; `tests/beat.rs::double_fire_enqueues_exactly_once` pins it.
- Task-status/result API: single-row reads by id, no N+1; the 50 ms `await_result` poll and its timeout behavior are documented.
- `#[task]` autodiscovery: `inventory`-based registration, `DISCOVERY_DISABLED` escape hatch, and typed enqueue covered by `tests/auto_discovery.rs`, `typed_enqueue.rs`, `macro_integration.rs` (the macro expansion itself lives in `umbral-macros`).

## Known issues already tracked elsewhere (not counted in the totals)

- `planning/gaps6.md` #21(e) — comment at `lib.rs:1042-1043` frames `SELECT … FOR UPDATE SKIP LOCKED` as future work while `:1092` ships it `[open]`
- `planning/gaps6.md` #30 — framework scaling: "worker concurrency, heartbeats, required-worker checks" named as scope for `plugins/umbral-tasks` `[open]` — note: TASKS-4 (no lease renewal) is the concrete defect inside that "heartbeats" class; the entry records the feature, not the defect.
- `planning/audit_2/findings/plugin-storage-tasks.md` #5 — claim-query index shipped and verified (`#[umbral(indexes = [["status", "run_at"]])]`, `tests/claim_index.rs`); the retention sweep for terminal rows remains open `[open]`
- `planning/audit_2/findings/plugin-storage-tasks.md` #9 — per-task-timeout "cooperative cancellation" caveat, marked done as a docs note `[open]` — its premise ("relies on dropping the `JoinHandle` to abort") is refuted above: tokio detaches, so the docs it added describe a cancellation that never happens. The specific defect is TASKS-1.
- `planning/hardening/plugins-review/umbra-tasks.md` — `WorkerOptions::default()` leaks the sender via `mem::forget` (`lib.rs:841-847`), and `WorkerCommand`/`BeatCommand` never wire Ctrl-C (`lib.rs:393`, `:433` still call the defaults) `[open]`
- `planning/features.md` #82 — per-task `timeout`/backoff persistence deferred; `EnqueueOptions::timeout` stays documented as API-only `[open]`
- `planning/gaps5.md` #56 — operator-grade scheduler expansion (pause/resume, per-schedule locks, missed-run policy, calendar/timezone UX) `[open]` — the class TASKS-8's doc gap falls under.
- `review_1` BROKEN-1 / BROKEN-2 / BROKEN-4, `audit_2` #6 / #10, `review_3/codex` #17 — all re-read this sweep and `[fixed/verified held]`.

## Out-of-scope notes

- `crates/umbral-core/src/signals.rs:61,335` — `emit()` awaits every async subscriber in series under a single 30 s `ASYNC_SUBSCRIBER_TIMEOUT`; anything that emits on a latency-sensitive path inherits the slowest subscriber's latency. Routed to the umbral-core review for sizing (this crate only consumes the registry, and does not emit).
- `crates/umbral-macros` — the `#[task]` expansion (name derivation, registration record) is the other half of this crate's typed API and was not reviewed here; `tests/typed_enqueue.rs` + `tests/macro_integration.rs` assert the consumer-visible contract only.
