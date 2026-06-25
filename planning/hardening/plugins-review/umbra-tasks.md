# umbral-tasks review

## Verdict

The plugin is a functional, surprisingly lean v1: the core claim/dispatch/retry loop is correct, the `#[task]` macro works end-to-end, and the architecture is clean (facade-only imports, proper Plugin impl, zero raw SQL in plugin src). The two main open wounds are both previously documented: BROKEN-2 (worker-crash orphans tasks permanently in `running` — at-most-once delivery, not the advertised at-least-once) is deferred with no gap number yet, and MISS-1 (`FOR UPDATE SKIP LOCKED` / `select_for_update`) is an ORM gap tracked in `planning/review/` but not filed in gaps2.md. Net-new findings are minor but real: a non-retriable detection using string prefix matching that will mis-classify in edge cases, a deliberate `mem::forget` sender leak on every `WorkerOptions::default()` call, doc inconsistency on instantiation style (`TasksPlugin` vs `TasksPlugin::default()`), and several test-coverage holes (panic handler, graceful shutdown, concurrent-worker correctness).

---

## Completeness (vs Celery)

### Implemented

- `#[task]` attribute macro — emits original fn + `register_<name>()` companion; enforces `async fn`, exactly one parameter, `Result<(), String>` return at compile time; `name = "..."` override works; `payload deserialise error:` message propagated to `error` column.
- `enqueue<P: Serialize>(name, payload, opts)` — inserts pending row, returns id; `scheduled_for` for delayed execution; configurable `max_attempts`.
- DB-backed queue model (`TaskRow`) — declared as an ORM model, registered via `Plugin::models()` so `makemigrations` creates `task_row`; fields cover status, attempt bookkeeping, timestamps, error.
- Worker loop (`run_worker`) — polling every 1s (configurable), graceful shutdown via `watch::Receiver<bool>`, sleeps when queue empty, logs worker-level errors without hot-looping.
- Single-iteration driver (`run_worker_once`) — clean test entry point, also exposed via `tasks-worker --once`.
- Claim guard — conditional `UPDATE WHERE status='pending'` inside a transaction; `affected == 0` ⇒ another worker took the row, return `None`. This is the BROKEN-1 fix (`98ef6e9`). Correct under READ COMMITTED Postgres (second worker's UPDATE blocks on first's row lock, then re-evaluates the `status='pending'` predicate against the committed `running` row and gets zero affected rows).
- Retry semantics — `attempts` incremented in `claim_one`; handler failure resets to `pending` until `attempts >= max_attempts`, then terminal `failed`; `handler not found` is immediately terminal regardless of attempts.
- Panic isolation — handler future `tokio::task::spawn`ed; join error / panic payload stringified and stored as the `error` column value; one panicking handler does not take the worker down.
- `Plugin::commands()` — `tasks-worker` subcommand contributed; dispatched via `umbral_cli::dispatch`; tested end-to-end in `integration.rs`.
- Handler registry — `OnceLock<RwLock<HashMap<&'static str, BoxedHandler>>>` with `register_handler` (idempotent) and `_clear_handlers_for_tests` escape hatch.
- Architecture boundary — imports only `umbral::prelude::*`; zero `umbral-core` direct deps; zero raw `sqlx::query` in `src/`.

### Stubs / Partial

- Graceful shutdown — `WorkerOptions::shutdown` channel exists and the poll loop checks it, but `WorkerCommand::run` passes `WorkerOptions::default()` which leaks a sender and never actually connects to any Ctrl-C signal handler (see Finding 4). A real Ctrl-C hook is not wired; the binary-level integration is incomplete.
- Doc status on Postgres locking — `tasks.mdx:172` still says "Postgres-aware locking is a later follow-on," which was true at BROKEN-1's discovery but is confusing post-fix since the conditional-UPDATE guard does prevent double-claim (just not as efficiently as `FOR UPDATE SKIP LOCKED`). The doc neither confirms what was fixed nor states what remains.

### Missing

- `FOR UPDATE SKIP LOCKED` — the ORM has no `select_for_update()` terminal. The existing conditional-UPDATE is *correct* but forces two workers to contend on the same row (SELECT → block → wasted attempt). On Postgres under high worker concurrency this degrades throughput and, under sufficiently high contention, could allow thundering-herd behavior. Tracked as MISS-1 in `planning/review/missing-features.md` and as a prerequisite in `planning/review/query-api-sufficiency.md:94`; not yet assigned a gaps2 number.
- Orphaned task reclaim — tasks stranded in `running` by a worker crash are never reclaimed. `claim_one` filters on `status = 'pending'`; there is no visibility-timeout / lease-expiry / reclaim watcher. The `started_at` column is already stamped and would make a safe reclaim trivial to add. Documented as BROKEN-2, deferred; not yet in gaps2.md.
- Exponential backoff — retries fire immediately; the only delay is the next worker poll cycle (1s). `features.md #43` acknowledges "exponential backoff" as missing. Not in gaps2.md yet.
- Periodic/cron ("beat") — no scheduled recurring tasks. Explicitly deferred in module header and `tasks.mdx`. The spec outline names it (`cargo run -- beat`) but it is not implemented.
- Result backend — no way to store or await a task's return value. The task handler returns `Result<(), String>`; the success path stores only status + timestamp, not a payload. Not even a deferred note in the current code.
- Priority queues — no `priority` column or per-queue routing. Documented as out of scope for v1 in module header.
- Admin visibility — `TaskRow` is a registered model but `TasksPlugin` does not call `admin_register` (nor does any admin auto-register it). Task queue depth and history are not visible in the admin UI.
- Metrics / queue-depth observable — no `queue_depth` gauge, no per-task timing, no integration with `umbral-health` readiness probe.
- Per-task execution timeout (`time_limit` / `soft_time_limit`) — a handler that blocks forever holds the worker indefinitely. `tokio::time::timeout` is not applied to the handler future.

---

## Findings

### [Important] Orphaned `running` tasks — worker crash is at-most-once — `src/lib.rs:60-63`
**Severity:** Important
**Status:** Already documented (BROKEN-2 in `planning/review/broken-features.md`) — deferred. Not yet assigned a gaps2 number.
**Description:** If the worker process is killed (SIGKILL, OOM, deploy) while a handler is executing, the row stays in `running` forever. `claim_one` only selects `status = 'pending'`; no lease/reclaim watcher exists. `started_at` is stamped (`:428` in `claim_one`) — all the data for a reclaim is present but the query is not.
**Fix:** Add a reclaim path in `run_worker`'s loop: `UPDATE task_row SET status='pending', started_at=NULL WHERE status='running' AND started_at < NOW() - $lease_timeout` before the `claim_one` call. `started_at` already set makes this safe. File a gaps2 entry so it has a trackable number (current max is #78).

---

### [Important] `non_retriable` detection via `starts_with("handler not found")` is fragile — `src/lib.rs:487`
**Severity:** Important
**Status:** NEW
**Description:** `process_one` determines whether an error is non-retriable by checking `err_msg.starts_with("handler not found")`. This string originates in `None => Err(format!("handler not found: {}", row.name))` at line 463, but the check couples the retry policy to the exact error string format. If the format string ever changes (e.g., to be prefixed with module info or a structured error id), the check silently stops being non-retriable and a missing-handler task retries `max_attempts` times before failing — calling into the void repeatedly and wasting worker cycles.

The `TaskError` enum already has a `HandlerNotFound` variant (`src/lib.rs:147`), which is the right place to encode this. The result type in `process_one` uses `String` for the error (the handler contract is `Result<(), String>`) but the non-retriable determination should be made at the `handler` match arm, not via string inspection.
**Fix:** Short-term: add a const `const HANDLER_NOT_FOUND_PREFIX: &str = "handler not found";` and use it in both sites. Correct-term: change `process_one`'s internal logic so the `None` (handler missing) branch directly writes the terminal-failed state without going through the `Err(String)` path — the retry policy is a property of the error variant, not the message text.

---

### [Important] `FOR UPDATE SKIP LOCKED` still missing — `src/lib.rs:391-436`
**Severity:** Important
**Status:** Already tracked as MISS-1 in `planning/review/missing-features.md` and `planning/review/query-api-sufficiency.md:94`. Not yet assigned a gaps2 number.
**Description:** The claim query is a plain SELECT with no row lock. The conditional UPDATE is a correct optimistic guard that prevents double-claim under normal load, but it pays a full SELECT contention cost on every poll when multiple workers run simultaneously: two workers SELECT the same row, one wins the UPDATE, the other wastes a round-trip and returns `None`. At high worker counts and/or high task throughput this becomes a thundering-herd poll. The `FOR UPDATE SKIP LOCKED` shape (Postgres) would route each worker to a different row atomically, eliminating the wasted round-trips.
**Fix:** Blocked on `select_for_update()` landing in the ORM queryset (MISS-1). Once it exists, `claim_one`'s candidate SELECT gains `.select_for_update(SkipLocked)` on Postgres; SQLite falls back to the current no-lock behavior (its WAL single-writer already prevents the contention). The existing conditional UPDATE can then be simplified or removed.

---

### [Optional] `mem::forget(_tx)` in `WorkerOptions::default()` leaks a channel sender on every call — `src/lib.rs:314`
**Severity:** Optional
**Status:** NEW
**Description:** `WorkerOptions::default()` creates a `watch::channel(false)` and immediately `mem::forget`s the sender `_tx` so the receiver `rx` stays live (an open channel). The comment justifies this as "one-time-registration so the leak is acceptable." In practice, every test that calls `run_worker(WorkerOptions::default())` (or any production code constructing a default `WorkerOptions`) leaks one sender allocation. For tests and single-binary production use where one default is constructed, this is genuinely negligible. If `WorkerOptions::default()` is ever called multiple times (e.g., in a retry-reconnect loop, a test harness that re-creates the worker), the leaks accumulate.
**Fix:** Replace the `mem::forget` pattern with a `once_cell::sync::Lazy` or `OnceLock` that holds the static sender, or provide a `WorkerOptions::new()` that takes an explicit `watch::Receiver<bool>` and reserve `default()` for the "never-fires" case. Alternatively, just document clearly that `WorkerOptions::default()` may only be called once per process lifetime.

---

### [Optional] `tasks.mdx:11` and `tasks.mdx:23` disagree on how to instantiate `TasksPlugin` — `documentation/docs/v0.0.1/plugins/tasks.mdx:11,23`
**Severity:** Optional (Nit borderline)
**Status:** NEW
**Description:** The module header `src/lib.rs:6` writes `.plugin(TasksPlugin)` (unit struct, no parens). The module header of `tasks.mdx:11` copies this: "`.plugin(TasksPlugin)` line." Then `tasks.mdx:23` gives a code block that uses `.plugin(TasksPlugin::default())`. The struct has `#[derive(Default)]` so both compile, but the doc page is internally inconsistent within 12 lines.
**Fix:** Pick one form and use it throughout. The unit-struct form (`.plugin(TasksPlugin)`) is shorter and idiomatic for a zero-field struct; `::default()` is fine but redundant. Update the code block at line 23 to match the prose at line 11.

---

### [Optional] `tasks.mdx:172` locking claim is misleading after BROKEN-1 fix
**Severity:** Optional
**Status:** NEW
**Description:** `tasks.mdx:172` under "What is NOT in v1" says: "SQLite is single-writer; a brief transaction is sufficient. Postgres-aware locking is a later follow-on." After the BROKEN-1 fix (`98ef6e9`), the conditional UPDATE correctly prevents double-claim on Postgres. The doc still implies the Postgres case is "not handled," which is now wrong in the correctness sense (double-claim is prevented). What remains is the *efficiency* gap (no `FOR UPDATE SKIP LOCKED`), not a correctness gap.
**Fix:** Reword the bullet to distinguish correctness (fixed) from efficiency: "Concurrent workers are guarded by a conditional UPDATE (status='pending') that prevents double-claim. `FOR UPDATE SKIP LOCKED` for lower contention under high worker counts is a planned ORM follow-on (MISS-1)."

---

### [Optional] `WorkerCommand::run` never connects shutdown to the process signal — `src/lib.rs:129-138`
**Severity:** Optional
**Status:** NEW
**Description:** `WorkerCommand::run` dispatches `run_worker(WorkerOptions::default())`. `WorkerOptions::default()` uses a never-fires shutdown channel. The `--once` path works cleanly (`run_worker_once` + return). But the continuous-worker path has no Ctrl-C hook: the worker will not shut down gracefully on SIGINT or SIGTERM. The user has to SIGKILL the process, which triggers the orphan scenario above (BROKEN-2). The `WorkerOptions::shutdown` field exists precisely for this, but the CLI entry point never populates it with a real signal receiver.
**Fix:** Wire `tokio::signal::ctrl_c()` (or `tokio::signal::unix::signal(SignalKind::terminate())`) into a `watch::Sender<bool>` inside `WorkerCommand::run`, pass the receiver as `shutdown: rx` in `WorkerOptions`. This is straightforward and makes the advertised graceful-shutdown behavior actually work from the CLI.

---

### [FYI] `#[task]` macro enforces `Deserialize` (via runtime `from_str`) but not `Serialize` at compile time
**Severity:** FYI
**Status:** NEW
**Description:** The generated `register_<fn>()` wrapper deserializes the payload JSON string. The compile-time constraint is on the handler parameter's type (it must implement `Deserialize` because `from_str` is called). However, `enqueue` separately requires `P: Serialize` — but `P` at the enqueue call site is the *caller's* type, not the *handler's* payload type. If the caller enqueues with a type that serializes to a shape incompatible with what the handler expects (e.g., different field names), the mismatch is caught only at worker runtime (as a "payload deserialise error" in the `error` column). This is a type-system gap noted in the spec outline under "open questions": "compile-time enforcement (so a non-serde arg fails at macro expansion, not at first enqueue) needs proc-macro work."
**Fix:** The spec outline acknowledges this. The long-term fix is a typed `enqueue::<HandlerFn>()` generated by the macro that enforces `SerdeCompat` between enqueuer and handler at the call site. For now, document clearly that the mismatch is a runtime failure mode.

---

### [FYI] `STATUS_RUNNING` orphan doc is not enforced — `src/lib.rs:62`
**Severity:** FYI
**Status:** Already documented (BROKEN-2).
**Description:** The `STATUS_RUNNING` constant's doc comment says "a crashed worker leaves the row in this state until manual cleanup or a future timeout-watcher reclaims it." The phrase "manual cleanup" is the only guidance. No query or management command is available to reclaim stuck tasks. This leaves operators with no tooling.
**Fix:** As part of the BROKEN-2 fix, either add a `tasks reclaim-stuck` management command or fold the reclaim into the worker loop.

---

### [FYI] Tests rely on raw `sqlx::query` for schema setup and read-back — `tests/integration.rs:47-88`
**Severity:** FYI (allowed exception per CLAUDE.md)
**Status:** NEW (expected pattern)
**Description:** Both test files use raw `sqlx::query("CREATE TABLE task_row ...")` for schema bootstrap and `sqlx::query_as("SELECT * FROM task_row WHERE id = ?")` for row read-back. The CLAUDE.md explicitly permits raw DDL in test-only schema setup ("A plugin that creates its own tables outside the migration system (e.g. `ensure_tables_for_tests`) is the lone allowed pattern, and only because tests bypass `make`/`run`"). The read-back queries are the only place where `sqlx::query_as` is acceptable under the test-isolation rationale. This is the documented exception; no action required.

---

## Tests

### Covered

- Happy path: enqueue → `run_worker_once` → `succeeded`, `completed_at` set, `error` null, `attempts = 1` (`integration.rs::enqueue_then_run_worker_once_processes_a_task`).
- Retry exhaustion: `max_attempts=2`, first attempt resets to `pending`, second attempt reaches terminal `failed` with `attempts = 2`, third step sees empty queue (`failed_handler_retries_until_max_attempts`).
- `scheduled_for` future: enqueued row with `+1h` stays `pending` and `run_worker_once` returns `false` (`enqueued_task_with_future_scheduled_for_is_not_picked_up`).
- Unknown handler: non-retriable immediately terminal `failed`, `error` contains "handler not found" (`unknown_handler_marks_task_failed_with_handler_not_found_error`).
- Basic enqueue shape: returns positive id, row has expected defaults — `status = pending`, `attempts = 0`, `max_attempts = DEFAULT_MAX_ATTEMPTS`, no timestamps or error (`enqueue_returns_new_id_and_writes_pending_row`).
- Empty queue idle: `run_worker_once` returns `Ok(false)` without blocking (`run_worker_once_returns_false_on_empty_queue`).
- CLI dispatch: `tasks-worker --once` dispatched through `umbral::cli::dispatch` runs the worker and flags the handler; `Unmatched` returned for unknown command (`dispatch_routes_tasks_worker_command_to_run_worker_once`, `dispatch_returns_unmatched_for_unknown_command`).
- `#[task]` macro: happy path with typed `GreetPayload`; `name =` override registered under custom key; bad payload (wrong fields) → `failed` with "payload deserialise error" message (`macro_integration.rs`, 3 tests).

### Not covered

- **Panic recovery** — no test that enqueues a task whose handler calls `panic!()` and asserts the row lands in `failed` with a "handler panicked" error message (rather than taking down the test process). The `tokio::task::spawn` catch is a critical correctness path and is completely untested.
- **Graceful shutdown** — no test for `run_worker` respecting a `shutdown` channel flip. The `WorkerOptions::shutdown` field's behavior is untested.
- **Concurrent workers** — no test that spawns two concurrent `run_worker_once` calls and asserts only one claims a given row. The conditional-UPDATE guard (the BROKEN-1 fix) is the most important correctness property and has zero test coverage.
- **`process_one` terminal-state write failure** — if `update_values` fails after the handler runs (DB hiccup), the executed task stays `running`. No test simulates this.
- **`mem::forget` leak accumulation** — no test exercises multiple `WorkerOptions::default()` constructions to validate the leak stays bounded.
- **`#[task]` compile-error paths** — the macro emits `compile_error!` for non-async, wrong param count, wrong return type, and `self` receiver. These are not tested via `trybuild` or similar (low priority for a compile-time-only check, but worth noting for regression protection).
- **Retry with non-zero `attempts` restart** — the `attempts` counter persists across retries (incremented in `claim_one`, not reset on retry), but there is no test validating the `attempts` column value at each intermediate step beyond the 2-attempt case.
