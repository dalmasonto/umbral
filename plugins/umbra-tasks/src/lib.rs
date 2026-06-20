//! umbra-tasks — DB-backed background task queue. The Celery-equivalent
//! shape: enqueue work that runs outside the request/response cycle,
//! with retries and a worker process you run alongside the web server.
//! v1 uses the application's own SQLite/Postgres pool as the broker, so
//! a fresh umbra project gets background work for the cost of one
//! `.plugin(TasksPlugin)` line.
//!
//! ## Surface
//!
//! - [`TaskRow`] model (one row per enqueued job; lives in the `task_row`
//!   table because the M3 derive snake_cases the struct name).
//! - [`TasksPlugin`] registers the model so `makemigrations` produces
//!   the right `CREATE TABLE`.
//! - [`enqueue`] inserts a `pending` row and returns its id.
//! - [`register_handler`] stores a handler in a process-wide `OnceLock`
//!   so the worker can look it up by name.
//! - [`run_worker`] is the polling loop a real binary drives; takes a
//!   `tokio::sync::watch::Receiver<bool>` for graceful shutdown.
//! - [`run_worker_once`] is the single-iteration variant tests drive
//!   inline.
//!
//! ## v1 scope and deferrals
//!
//! - No priority queue, no separate broker, no distributed locks.
//!   SQLite is single-writer anyway; a brief transaction is enough.
//! - Status is a String, not an enum: the M3 derive doesn't yet support
//!   enum SqlType. The four valid values are [`STATUS_PENDING`],
//!   [`STATUS_RUNNING`], [`STATUS_SUCCEEDED`], [`STATUS_FAILED`].
//! - Handlers register per-process at startup. A handler that wasn't
//!   registered before the worker spawns is the same as "handler not
//!   found", which the worker marks the task failed for.
//! - `#[task]` macro shipped: use `#[umbra::task]` on an `async fn` to
//!   generate typed registration helpers. See `umbra-macros` and the
//!   tasks docs page.
//! - Reliability & scheduling (this revision): every task carries a
//!   `run_at` instant. Enqueue can set it in the future (`eta` / `delay`)
//!   so the task runs later. A retriable failure pushes `run_at` forward
//!   by an exponential backoff (`retry_backoff_base * 2^(attempts-1)`,
//!   capped at `retry_backoff_max`) instead of re-queuing immediately. The
//!   worker wraps each handler in a [`WorkerOptions::task_timeout`]; a
//!   handler that overruns is recorded as a retriable failure (backed off
//!   or abandoned) rather than holding its claim until the visibility
//!   timeout.
//! - Periodic/cron scheduling ("beat", this revision): a [`PeriodicTask`]
//!   model carries a stable `name`, the handler `task` to fire, its JSON
//!   `payload`, a serialized [`Schedule`] (cron expression or fixed
//!   interval) and the computed `next_run`. [`TasksPlugin::periodic`]
//!   registers a recurring task Celery-`beat_schedule` style; [`run_beat`]
//!   is the separate beat process that, each tick, atomically claims every
//!   due row (an optimistic conditional `UPDATE` advances `next_run` so a
//!   second beat instance can't double-fire it) and enqueues the underlying
//!   task. Run it via the `tasks-beat` CLI command.
//! - No result backend, no task-status query API, no priority queues. Those
//!   are the remaining Celery gaps, deferred to planning/features.md #82.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

/// Re-export of `serde_json` for use in `#[task]` macro-generated code.
///
/// The `#[task]` proc-macro (in `umbra-macros`) emits
/// `::umbra_tasks::_serde_json::from_str(...)` in the generated wrapper
/// closure. Routing through this re-export means user crates don't need
/// a direct `serde_json` dep for the generated code to compile.
#[doc(hidden)]
pub use serde_json as _serde_json;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use umbra::prelude::*;

/// Status string for a freshly enqueued row, or a row a failing handler
/// has been reset to so it can retry.
pub const STATUS_PENDING: &str = "pending";
/// Status string while a worker is mid-execution. The worker loop calls
/// [`reclaim_orphaned_tasks`] on every iteration so tasks left in this state
/// by a crashed worker are moved back to [`STATUS_PENDING`] once the
/// visibility timeout has elapsed.
pub const STATUS_RUNNING: &str = "running";
/// Terminal status for a handler that returned `Ok`.
pub const STATUS_SUCCEEDED: &str = "succeeded";
/// Terminal status for a handler whose final attempt returned `Err`, or
/// for a task whose handler isn't registered.
pub const STATUS_FAILED: &str = "failed";

/// One enqueued task. `name` keys the handler registry; `payload` is the
/// JSON-encoded args the handler deserializes.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct TaskRow {
    pub id: i64,
    pub name: String,
    pub payload: String,
    pub status: String,
    pub attempts: i64,
    pub max_attempts: i64,
    pub scheduled_for: DateTime<Utc>,
    /// The instant this task becomes eligible to run. The dequeue query
    /// only claims rows whose `run_at <= now()` (a `NULL` `run_at` counts
    /// as "immediately eligible"). Set on enqueue from `EnqueueOptions`
    /// (`eta` / `delay`, default = now). On a retriable failure the worker
    /// pushes it into the future by the exponential backoff so the row
    /// isn't re-claimed until the delay elapses.
    ///
    /// Nullable rather than `DateTime<Utc>` because the migration engine
    /// can't yet emit `ADD COLUMN ... NOT NULL DEFAULT <now>` (see
    /// `migrate.rs` `Operation::AddColumn`); a nullable add applies cleanly
    /// against existing rows, which then read as immediately-runnable
    /// (`NULL = run now`). Enqueue always writes `Some`.
    pub run_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// The plugin. Registers the [`TaskRow`] and [`PeriodicTask`] models and
/// collects any [`TasksPlugin::periodic`] schedules.
#[derive(Debug, Default)]
pub struct TasksPlugin {
    /// Recurring schedules collected via [`TasksPlugin::periodic`].
    /// Published to [`REGISTERED_PERIODIC`] in [`Plugin::on_ready`] and
    /// upserted to `PeriodicTask` rows by [`run_beat`] on startup.
    periodic: Vec<PeriodicSpec>,
}

impl Plugin for TasksPlugin {
    fn name(&self) -> &'static str {
        "tasks"
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        vec![
            umbra::migrate::ModelMeta::for_::<TaskRow>(),
            umbra::migrate::ModelMeta::for_::<PeriodicTask>(),
        ]
    }

    fn commands(&self) -> Vec<Box<dyn umbra::cli::PluginCommand>> {
        vec![Box::new(WorkerCommand), Box::new(BeatCommand)]
    }

    fn on_ready(&self, _ctx: &umbra::plugin::AppContext) -> Result<(), umbra::plugin::PluginError> {
        // Install the builder-collected periodic specs into the ambient
        // registry so `run_beat` can sync them to `PeriodicTask` rows on
        // startup. `on_ready` is sync, so the async DB upsert happens in
        // the beat loop; here we only publish the in-memory specs.
        if !self.periodic.is_empty()
            && REGISTERED_PERIODIC.set(self.periodic.clone()).is_err()
        {
            tracing::warn!(
                "umbra-tasks: periodic specs already installed by another \
                 TasksPlugin; ignoring this registration"
            );
        }
        Ok(())
    }
}

impl TasksPlugin {
    /// Register a recurring task, Celery `beat_schedule` style. `name` is
    /// the schedule's stable key (one `PeriodicTask` row per name); `task`
    /// is the handler name [`run_beat`] enqueues each time the schedule
    /// fires; `payload` is the JSON args the handler receives.
    ///
    /// Specs are collected on the builder and installed into the ambient
    /// registry in [`Plugin::on_ready`]; [`run_beat`] upserts them to
    /// `PeriodicTask` rows on startup (insert new, update the
    /// schedule/task/payload of existing ones by name).
    ///
    /// ```ignore
    /// App::builder()
    ///     .plugin(
    ///         TasksPlugin::default()
    ///             .periodic(
    ///                 "nightly_cleanup",
    ///                 Schedule::cron("0 0 * * *"),
    ///                 "cleanup_task",
    ///                 serde_json::json!({ "older_than_days": 30 }),
    ///             ),
    ///     )
    ///     .build()?;
    /// ```
    pub fn periodic<P: Serialize>(
        mut self,
        name: &str,
        schedule: Schedule,
        task: &str,
        payload: P,
    ) -> Self {
        let payload = serde_json::to_string(&payload)
            .unwrap_or_else(|e| panic!("umbra-tasks: periodic payload not serializable: {e}"));
        self.periodic.push(PeriodicSpec {
            name: name.to_string(),
            schedule,
            task: task.to_string(),
            payload,
        });
        self
    }
}

/// `tasks worker`: drain the task queue.
///
/// `--once` runs one iteration of the claim/dispatch loop and exits
/// (suitable for tests, cron-driven workers, or anywhere
/// `run_worker`'s infinite loop is unwanted).
///
/// Without `--once` the command never returns; it keeps polling at
/// the default interval.
#[derive(Debug, Default)]
pub struct WorkerCommand;

#[async_trait::async_trait]
impl umbra::cli::PluginCommand for WorkerCommand {
    fn command(&self) -> clap::Command {
        clap::Command::new("tasks-worker")
            .about("Run the umbra-tasks background worker")
            .arg(
                clap::Arg::new("once")
                    .long("once")
                    .help("Run one iteration of the claim/dispatch loop and exit")
                    .action(clap::ArgAction::SetTrue),
            )
    }

    async fn run(&self, matches: &clap::ArgMatches) -> Result<(), umbra::cli::CliError> {
        if matches.get_flag("once") {
            let _ran = run_worker_once().await?;
            Ok(())
        } else {
            run_worker(WorkerOptions::default()).await;
            Ok(())
        }
    }
}

/// `tasks-beat`: run the periodic-task scheduler (Celery beat).
///
/// On startup it syncs the registered [`PeriodicSpec`]s to `PeriodicTask`
/// rows, then each tick claims every due row atomically and enqueues the
/// underlying task. Run it as its OWN process alongside `tasks-worker`
/// (the worker drains the queue beat fills).
///
/// `--once` runs one sync + one tick and exits (tests, cron-driven beats).
/// Without `--once` it polls forever at the default interval.
#[derive(Debug, Default)]
pub struct BeatCommand;

#[async_trait::async_trait]
impl umbra::cli::PluginCommand for BeatCommand {
    fn command(&self) -> clap::Command {
        clap::Command::new("tasks-beat")
            .about("Run the umbra-tasks periodic scheduler (Celery beat)")
            .arg(
                clap::Arg::new("once")
                    .long("once")
                    .help("Sync schedules, run one tick, and exit")
                    .action(clap::ArgAction::SetTrue),
            )
    }

    async fn run(&self, matches: &clap::ArgMatches) -> Result<(), umbra::cli::CliError> {
        if matches.get_flag("once") {
            let _fired = run_beat_once().await?;
            Ok(())
        } else {
            run_beat(BeatOptions::default()).await;
            Ok(())
        }
    }
}

/// Errors the task helpers and worker can produce.
#[derive(Debug)]
pub enum TaskError {
    /// sqlx error executing one of the queue queries.
    Sqlx(sqlx::Error),
    /// `payload` round-tripping through serde failed.
    Json(serde_json::Error),
    /// The worker pulled a row whose `name` isn't in the registry.
    /// Surfaced as the task's `error` column and marks the row failed
    /// regardless of `attempts` — a missing handler isn't a transient
    /// fault.
    HandlerNotFound(String),
    /// The handler future panicked. Caught via `tokio::task::JoinHandle`
    /// so one bad handler doesn't take the worker down with it.
    HandlerPanicked(String),
    /// Anything else, kept narrow so callers can match on the variants
    /// they care about and bucket the rest here.
    Other(String),
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskError::Sqlx(e) => write!(f, "umbra-tasks: sqlx: {e}"),
            TaskError::Json(e) => write!(f, "umbra-tasks: json: {e}"),
            TaskError::HandlerNotFound(name) => {
                write!(f, "umbra-tasks: handler not found: {name}")
            }
            TaskError::HandlerPanicked(msg) => {
                write!(f, "umbra-tasks: handler panicked: {msg}")
            }
            TaskError::Other(msg) => write!(f, "umbra-tasks: {msg}"),
        }
    }
}

impl std::error::Error for TaskError {}

impl From<sqlx::Error> for TaskError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

impl From<serde_json::Error> for TaskError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<umbra::orm::write::WriteError> for TaskError {
    fn from(e: umbra::orm::write::WriteError) -> Self {
        Self::Other(format!("write: {e:?}"))
    }
}

// =========================================================================
// Public helpers.
// =========================================================================

/// Options for [`enqueue`]. Both fields are optional with sensible
/// defaults: 3 attempts and immediate execution.
#[derive(Debug, Clone, Default)]
pub struct EnqueueOptions {
    /// How many times the worker retries before giving up. Defaults to 3.
    pub max_attempts: Option<i64>,
    /// Earliest time the worker is allowed to pick this row up. Rows
    /// whose `scheduled_for` is in the future stay invisible to the
    /// claim query. Defaults to `Utc::now()`.
    pub scheduled_for: Option<DateTime<Utc>>,
    /// Absolute instant the task becomes eligible to run (Celery's `eta`).
    /// Mutually exclusive with [`Self::delay`]; if both are set, `eta`
    /// wins. When neither is set the task is eligible immediately.
    pub eta: Option<DateTime<Utc>>,
    /// Run the task after this much delay from enqueue time (`run_at =
    /// now + delay`). A convenience over [`Self::eta`]; `eta` takes
    /// precedence if both are given.
    pub delay: Option<Duration>,
    /// Per-task timeout override (v1: API surface only). Reaching the
    /// worker with a per-row timeout needs a persisted column; to keep
    /// this revision to the single additive `run_at` column, the worker
    /// currently applies the worker-level [`WorkerOptions::task_timeout`]
    /// to every task. Persisting per-task `timeout` / backoff overrides as
    /// columns is the documented follow-up (planning/features.md #82). The
    /// field is accepted now so callers don't have to change later.
    pub timeout: Option<Duration>,
}

/// Default retry count when [`EnqueueOptions::max_attempts`] is `None`.
pub const DEFAULT_MAX_ATTEMPTS: i64 = 3;

/// Insert a pending task row and return its id. The handler must be
/// registered under `name` before the worker reaches the row, otherwise
/// the worker marks the row failed with [`TaskError::HandlerNotFound`].
pub async fn enqueue<P: Serialize>(
    name: &str,
    payload: P,
    opts: EnqueueOptions,
) -> Result<i64, TaskError> {
    let payload_json = serde_json::to_string(&payload)?;
    let now = Utc::now();
    let scheduled_for = opts.scheduled_for.unwrap_or(now);
    let max_attempts = opts.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS);
    // `eta` (absolute) wins over `delay` (relative); neither => run now.
    let run_at = opts.eta.or_else(|| {
        opts.delay.map(|d| {
            now + chrono::Duration::from_std(d).unwrap_or_else(|_| chrono::Duration::zero())
        })
    });

    let row = TaskRow::objects()
        .create(TaskRow {
            id: 0,
            name: name.to_string(),
            payload: payload_json,
            status: STATUS_PENDING.to_string(),
            attempts: 0,
            max_attempts,
            scheduled_for,
            run_at: Some(run_at.unwrap_or(now)),
            started_at: None,
            completed_at: None,
            error: None,
            created_at: now,
        })
        .await?;
    Ok(row.id)
}

/// The boxed handler type stored in the per-process registry. Returns
/// `Result<(), String>` so the error string lands directly in the
/// `error` column without an intermediate Display/Debug rendering step.
pub type BoxedHandler =
    Box<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;

/// Process-wide handler registry. Populated at startup before
/// [`run_worker`] spawns; queried by name on every claimed task.
/// `Mutex` not needed: registration is meant to happen during boot, and
/// the worker only ever reads.
static HANDLERS: OnceLock<std::sync::RwLock<HashMap<&'static str, BoxedHandler>>> = OnceLock::new();

fn handlers() -> &'static std::sync::RwLock<HashMap<&'static str, BoxedHandler>> {
    HANDLERS.get_or_init(|| std::sync::RwLock::new(HashMap::new()))
}

/// Register a handler under `name`. The handler takes the JSON-encoded
/// payload as `&str` and returns a future resolving to `Result<(), String>`.
/// The String error becomes the row's `error` column on failure.
///
/// Idempotent for ergonomics: re-registering the same name replaces the
/// previous handler. Tests rely on this to swap handlers between cases
/// without coordinating across the OnceLock.
pub fn register_handler<F, Fut>(name: &'static str, handler: F)
where
    F: Fn(&str) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    let boxed: BoxedHandler = Box::new(move |payload: &str| {
        let fut = handler(payload);
        Box::pin(fut)
    });
    handlers()
        .write()
        .expect("umbra-tasks: handler registry poisoned")
        .insert(name, boxed);
}

/// Clear the handler registry. Test-only escape hatch so cases can
/// guarantee a clean slate when they assert "unknown handler".
#[doc(hidden)]
pub fn _clear_handlers_for_tests() {
    if let Some(lock) = HANDLERS.get() {
        lock.write()
            .expect("umbra-tasks: handler registry poisoned")
            .clear();
    }
}

/// Default visibility timeout: tasks stuck in `running` for longer than
/// this are reclaimed and re-queued (or failed if at `max_attempts`).
pub const DEFAULT_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(5 * 60); // 5 minutes

/// Default base delay for exponential-backoff retries. The nth retry waits
/// `base * 2^(attempts-1)`, capped at [`DEFAULT_RETRY_BACKOFF_MAX`].
pub const DEFAULT_RETRY_BACKOFF_BASE: Duration = Duration::from_secs(2);
/// Default ceiling for the exponential-backoff retry delay.
pub const DEFAULT_RETRY_BACKOFF_MAX: Duration = Duration::from_secs(5 * 60); // 5 minutes
/// Default per-task timeout the worker wraps each handler in.
pub const DEFAULT_TASK_TIMEOUT: Duration = Duration::from_secs(5 * 60); // 5 minutes

/// The retry/timeout knobs the worker applies to a single task. Carved
/// out of [`WorkerOptions`] so the per-iteration helpers ([`process_one`],
/// [`reclaim_orphaned_tasks`]) can take a small `Copy` policy without the
/// non-`Copy` `shutdown` channel.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Base delay for exponential backoff. See
    /// [`WorkerOptions::retry_backoff_base`].
    pub backoff_base: Duration,
    /// Backoff ceiling. See [`WorkerOptions::retry_backoff_max`].
    pub backoff_max: Duration,
    /// Per-task handler timeout. See [`WorkerOptions::task_timeout`].
    pub task_timeout: Option<Duration>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            backoff_base: DEFAULT_RETRY_BACKOFF_BASE,
            backoff_max: DEFAULT_RETRY_BACKOFF_MAX,
            task_timeout: Some(DEFAULT_TASK_TIMEOUT),
        }
    }
}

impl RetryPolicy {
    /// `run_at` for a row that just failed retriably: `now + backoff`.
    fn next_run_at(&self, attempts: i64, now: DateTime<Utc>) -> DateTime<Utc> {
        let delay = backoff_delay(attempts, self.backoff_base, self.backoff_max);
        now + chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::zero())
    }
}

/// Compute the exponential backoff delay for a retry, given how many
/// attempts have already been made. The first retry (`attempts == 1`)
/// waits `base`, the next `base * 2`, then `base * 4`, … each capped at
/// `max`. `attempts <= 0` is treated as the first retry.
fn backoff_delay(attempts: i64, base: Duration, max: Duration) -> Duration {
    // `attempts` is the post-increment count from `claim_one`, so the
    // first failure arrives with `attempts == 1` => shift 0 => `base`.
    let shift = attempts.saturating_sub(1).clamp(0, 32) as u32;
    let scaled = base
        .checked_mul(1u32.checked_shl(shift).unwrap_or(u32::MAX))
        .unwrap_or(max);
    scaled.min(max)
}

/// Options for [`run_worker`]. Carries the poll interval, shutdown signal,
/// visibility timeout for orphan-task reclaim, the retry-backoff knobs, and
/// the per-task handler timeout.
pub struct WorkerOptions {
    /// How long to sleep when the queue is empty. Defaults to 1 second.
    pub poll_interval: Duration,
    /// Setting this to `true` cleanly exits the worker after the
    /// in-flight iteration finishes. A default never-fires channel is
    /// installed when callers use [`WorkerOptions::default`].
    pub shutdown: watch::Receiver<bool>,
    /// How long a task may stay in `running` before it is considered
    /// orphaned (i.e. its worker crashed). Orphaned tasks are moved back
    /// to `pending` so another worker can pick them up, unless they have
    /// already exhausted `max_attempts`, in which case they are marked
    /// `failed`. Defaults to [`DEFAULT_VISIBILITY_TIMEOUT`] (5 minutes).
    pub visibility_timeout: Duration,
    /// Base delay for exponential-backoff retries. On a retriable failure
    /// the worker sets `run_at = now + min(base * 2^(attempts-1), max)` so
    /// the row isn't re-claimed until the backoff elapses. Defaults to
    /// [`DEFAULT_RETRY_BACKOFF_BASE`] (2s).
    pub retry_backoff_base: Duration,
    /// Ceiling for the exponential-backoff retry delay. Defaults to
    /// [`DEFAULT_RETRY_BACKOFF_MAX`] (5 minutes).
    pub retry_backoff_max: Duration,
    /// How long a single handler invocation may run before the worker
    /// cancels it and records a retriable failure (backed off via `run_at`,
    /// or abandoned if `max_attempts` is exhausted). `None` disables the
    /// timeout. Defaults to [`DEFAULT_TASK_TIMEOUT`] (5 minutes). This is
    /// distinct from `visibility_timeout`: the timeout fails a *running*
    /// handler promptly, whereas the visibility timeout only reclaims a row
    /// after a *crashed* worker stops renewing its lease.
    pub task_timeout: Option<Duration>,
}

impl Default for WorkerOptions {
    fn default() -> Self {
        // A never-fires shutdown so the worker runs until killed.
        let (_tx, rx) = watch::channel(false);
        // Leak the sender so the receiver stays alive without anyone
        // holding a reference. The worker tolerates a closed channel
        // (treats it as no shutdown), but leaking keeps the contract
        // simple: the channel never closes.
        std::mem::forget(_tx);
        Self {
            poll_interval: Duration::from_secs(1),
            shutdown: rx,
            visibility_timeout: DEFAULT_VISIBILITY_TIMEOUT,
            retry_backoff_base: DEFAULT_RETRY_BACKOFF_BASE,
            retry_backoff_max: DEFAULT_RETRY_BACKOFF_MAX,
            task_timeout: Some(DEFAULT_TASK_TIMEOUT),
        }
    }
}

/// The polling loop. Runs until `opts.shutdown` flips to `true`: reclaim
/// any orphaned tasks (RUNNING longer than `opts.visibility_timeout`), claim
/// one pending due row, dispatch its handler, write back the terminal
/// state, loop. Returns normally on shutdown.
///
/// BROKEN-4: this used to `std::process::exit(0)` on shutdown and return
/// `!`. That's fatal in a single-binary deployment where the worker is
/// `tokio::spawn`ed alongside the web server — exiting the worker task
/// would tear down the entire process, HTTP server included. A library
/// function must never call `process::exit`; it returns and lets the
/// caller decide what happens next.
pub async fn run_worker(mut opts: WorkerOptions) {
    let policy = RetryPolicy {
        backoff_base: opts.retry_backoff_base,
        backoff_max: opts.retry_backoff_max,
        task_timeout: opts.task_timeout,
    };
    loop {
        if *opts.shutdown.borrow() {
            // Cooperative shutdown — hand control back to the caller
            // instead of killing the process.
            return;
        }
        // Reclaim orphaned tasks before claiming a new one so that a
        // crashed-worker's row becomes visible in the same iteration.
        if let Err(e) = reclaim_orphaned_tasks_with(opts.visibility_timeout, policy).await {
            tracing::error!(error = %e, "umbra-tasks: orphan reclaim failed");
        }
        match run_worker_once_with(policy).await {
            Ok(true) => {}
            Ok(false) => {
                // Queue empty: sleep before polling again. Cancellable
                // by the shutdown signal flipping in the meantime.
                tokio::select! {
                    _ = tokio::time::sleep(opts.poll_interval) => {}
                    _ = opts.shutdown.changed() => {}
                }
            }
            Err(e) => {
                // Worker-level error (DB unavailable, etc). Log and
                // sleep so we don't hot-loop on a persistent fault.
                tracing::error!(error = %e, "umbra-tasks: worker iteration failed");
                tokio::select! {
                    _ = tokio::time::sleep(opts.poll_interval) => {}
                    _ = opts.shutdown.changed() => {}
                }
            }
        }
    }
}

/// Reclaim orphaned tasks: any row whose `status = 'running'` and
/// `started_at < now - visibility_timeout` is considered abandoned by a
/// crashed worker. Rows within `max_attempts` are reset to `pending` so
/// another worker picks them up; rows already at `max_attempts` are
/// marked `failed` (no infinite retry loop).
///
/// This is the at-least-once guarantee: work is never silently dropped
/// because the worker that claimed it died mid-flight.
///
/// Uses the default [`RetryPolicy`] for backoff. The worker loop calls
/// [`reclaim_orphaned_tasks_with`] to honour the configured knobs.
pub async fn reclaim_orphaned_tasks(visibility_timeout: Duration) -> Result<u64, TaskError> {
    reclaim_orphaned_tasks_with(visibility_timeout, RetryPolicy::default()).await
}

/// [`reclaim_orphaned_tasks`] with an explicit backoff [`RetryPolicy`]. A
/// reclaimed-but-not-exhausted row is pushed forward by the same
/// exponential backoff a handler failure uses, so a flaky task that keeps
/// crashing its worker doesn't get retried in a tight loop.
pub async fn reclaim_orphaned_tasks_with(
    visibility_timeout: Duration,
    policy: RetryPolicy,
) -> Result<u64, TaskError> {
    let cutoff = Utc::now()
        - chrono::Duration::from_std(visibility_timeout)
            .unwrap_or(chrono::Duration::seconds(300));

    // Fetch all stuck-running rows whose lease has expired.
    let orphans: Vec<TaskRow> = TaskRow::objects()
        .filter(task_row::STATUS.eq(STATUS_RUNNING) & task_row::STARTED_AT.lt(cutoff))
        .fetch()
        .await?;

    if orphans.is_empty() {
        return Ok(0);
    }

    let mut reclaimed: u64 = 0;
    let now = Utc::now();

    for row in orphans {
        let exhausted = row.attempts >= row.max_attempts;
        let mut patch = serde_json::Map::new();
        if exhausted {
            // No retries left — mark permanently failed.
            patch.insert(
                "status".to_string(),
                serde_json::Value::String(STATUS_FAILED.to_string()),
            );
            patch.insert("completed_at".to_string(), serde_json::to_value(now)?);
            patch.insert(
                "error".to_string(),
                serde_json::Value::String(
                    "umbra-tasks: task abandoned by crashed worker; max_attempts exhausted"
                        .to_string(),
                ),
            );
        } else {
            // Still has retries — reset to pending so the next claim
            // picks it up. Clear `started_at` so the lease is fresh, and
            // push `run_at` forward by the backoff so a task that keeps
            // crashing its worker doesn't get re-claimed instantly.
            let run_at = policy.next_run_at(row.attempts, now);
            patch.insert(
                "status".to_string(),
                serde_json::Value::String(STATUS_PENDING.to_string()),
            );
            patch.insert("started_at".to_string(), serde_json::Value::Null);
            patch.insert("run_at".to_string(), serde_json::to_value(run_at)?);
            patch.insert(
                "error".to_string(),
                serde_json::Value::String(
                    "umbra-tasks: task abandoned by crashed worker; retrying".to_string(),
                ),
            );
        }
        // Conditional on STILL being running+expired to avoid a TOCTOU
        // race where another worker completed the task between the SELECT
        // and this UPDATE.
        let affected = TaskRow::objects()
            .filter(
                task_row::ID.eq(row.id)
                    & task_row::STATUS.eq(STATUS_RUNNING)
                    & task_row::STARTED_AT.lt(cutoff),
            )
            .update_values(patch)
            .await?;
        reclaimed += affected;
    }

    if reclaimed > 0 {
        tracing::info!(count = reclaimed, "umbra-tasks: reclaimed orphaned tasks");
    }

    Ok(reclaimed)
}

/// Single-iteration worker step. Returns `Ok(true)` if a task was
/// processed (regardless of whether the handler succeeded), `Ok(false)`
/// if the queue had no due rows.
///
/// Test-driver entry point: integration tests can drive deterministic
/// scenarios without spawning the polling loop. Uses the default
/// [`RetryPolicy`]; call [`run_worker_once_with`] to override the backoff
/// or timeout.
pub async fn run_worker_once() -> Result<bool, TaskError> {
    run_worker_once_with(RetryPolicy::default()).await
}

/// [`run_worker_once`] with an explicit [`RetryPolicy`] (backoff + per-task
/// timeout). The worker loop threads its [`WorkerOptions`] knobs through
/// here.
pub async fn run_worker_once_with(policy: RetryPolicy) -> Result<bool, TaskError> {
    let Some(row) = claim_one().await? else {
        return Ok(false);
    };
    process_one(row, policy).await?;
    Ok(true)
}

/// Atomically claim one pending due row by flipping it to `running` and
/// returning the row contents. Wrapped in a transaction so a concurrent
/// worker can't double-claim.
///
/// BROKEN-1: SQLite's single-writer model makes this safe there, but on
/// Postgres (READ COMMITTED) two workers could `SELECT` the same row
/// before either `UPDATE`s it, then both flip it to `running` — the same
/// task runs twice. The guard is the **conditional UPDATE**: the WHERE
/// clause re-asserts `status = 'pending'`, so the claim only counts if it
/// actually transitioned the row. On Postgres the second worker's UPDATE
/// blocks on the first's row lock, then re-evaluates the predicate
/// against the committed `running` row, matches nothing, and reports zero
/// affected rows — so it loses the race cleanly and we return `None`.
/// (A future `SELECT ... FOR UPDATE SKIP LOCKED` — MISS-1 — would avoid
/// the wasted SELECT, but this is already correct on both backends.)
async fn claim_one() -> Result<Option<TaskRow>, TaskError> {
    let now = Utc::now();
    umbra::transaction(|tx| {
        Box::pin(async move {
            let candidate = TaskRow::objects()
                .filter(
                    task_row::STATUS.eq(STATUS_PENDING)
                        & task_row::SCHEDULED_FOR.le(now)
                        // Only claim rows whose `run_at` is due. A NULL
                        // `run_at` (legacy rows, or rows from before this
                        // column existed) counts as immediately eligible.
                        & (task_row::RUN_AT.is_null() | task_row::RUN_AT.le(now)),
                )
                .order_by(task_row::SCHEDULED_FOR.asc())
                .order_by(task_row::ID.asc())
                .limit(1)
                .on_tx(tx)
                .first()
                .await?;
            let Some(mut row) = candidate else {
                return Ok::<Option<TaskRow>, TaskError>(None);
            };
            let new_attempts = row.attempts + 1;
            let mut patch = serde_json::Map::new();
            patch.insert(
                "status".to_string(),
                serde_json::Value::String(STATUS_RUNNING.to_string()),
            );
            patch.insert("started_at".to_string(), serde_json::to_value(now)?);
            patch.insert(
                "attempts".to_string(),
                serde_json::Value::from(new_attempts),
            );
            // Conditional claim: only transition the row if it's STILL
            // pending. `affected == 0` means another worker beat us to it.
            let affected = TaskRow::objects()
                .filter(task_row::ID.eq(row.id) & task_row::STATUS.eq(STATUS_PENDING))
                .on_tx(tx)
                .update_values(patch)
                .await?;
            if affected == 0 {
                return Ok(None);
            }
            // Reflect the in-DB mutations in the row we return so the
            // caller doesn't need to re-SELECT.
            row.status = STATUS_RUNNING.to_string();
            row.started_at = Some(now);
            row.attempts = new_attempts;
            Ok(Some(row))
        })
    })
    .await
}

/// Run the handler for a claimed row and write back the terminal state.
async fn process_one(row: TaskRow, policy: RetryPolicy) -> Result<(), TaskError> {
    let handler = {
        let guard = handlers()
            .read()
            .expect("umbra-tasks: handler registry poisoned");
        guard
            .get(row.name.as_str())
            .map(|h| h(&row.payload))
            .map(|fut| (fut,))
    };
    // Resolve to a typed `TaskError` so the retry decision can match on
    // the variant rather than inspect the error string.  The `err_msg`
    // string is kept separate from the variant so handler-returned
    // strings are stored verbatim in the `error` column (preserving the
    // original behaviour), while the variant drives the non-retriable
    // check without depending on the Display text.
    let result: Result<(), (TaskError, String)> = match handler {
        Some((fut,)) => {
            // Catch panics so one bad handler doesn't take the worker
            // down. `tokio::task::spawn` gives us the JoinHandle whose
            // join error carries panic payloads we can stringify. Wrap the
            // spawned handle in `tokio::time::timeout` so an overrunning
            // handler is cancelled (its task dropped) and recorded as a
            // retriable failure rather than pinning the worker.
            let join = tokio::task::spawn(fut);
            let outcome = match policy.task_timeout {
                Some(limit) => tokio::time::timeout(limit, join).await,
                None => Ok(join.await),
            };
            match outcome {
                Ok(Ok(Ok(()))) => Ok(()),
                Ok(Ok(Err(msg))) => Err((TaskError::Other(msg.clone()), msg)),
                Ok(Err(join)) if join.is_panic() => {
                    let msg = format!("handler panicked: {:?}", join.into_panic());
                    Err((TaskError::HandlerPanicked(msg.clone()), msg))
                }
                Ok(Err(join)) => {
                    let msg = format!("handler join error: {join}");
                    Err((TaskError::Other(msg.clone()), msg))
                }
                Err(_elapsed) => {
                    // Timed out. The `JoinHandle` is dropped here, which
                    // aborts the still-running handler task. Treat as a
                    // retriable failure (backed off below).
                    let secs = policy
                        .task_timeout
                        .map(|d| d.as_secs_f64())
                        .unwrap_or_default();
                    let msg = format!("umbra-tasks: handler timed out after {secs:.3}s");
                    tracing::warn!(task = %row.name, id = row.id, timeout_s = secs, "umbra-tasks: handler timed out");
                    Err((TaskError::Other(msg.clone()), msg))
                }
            }
        }
        None => {
            let err = TaskError::HandlerNotFound(row.name.clone());
            let msg = err.to_string();
            Err((err, msg))
        }
    };

    let now = Utc::now();
    match result {
        Ok(()) => {
            let mut patch = serde_json::Map::new();
            patch.insert(
                "status".to_string(),
                serde_json::Value::String(STATUS_SUCCEEDED.to_string()),
            );
            patch.insert("completed_at".to_string(), serde_json::to_value(now)?);
            patch.insert("error".to_string(), serde_json::Value::Null);
            TaskRow::objects()
                .filter(task_row::ID.eq(row.id))
                .update_values(patch)
                .await?;
        }
        Err((err, err_msg)) => {
            // `HandlerNotFound` is non-retriable — a missing handler
            // won't appear on the next attempt unless the operator
            // changes the code. Match on the typed variant so this
            // decision is robust to any future change in the Display
            // text.
            let exhausted = row.attempts >= row.max_attempts;
            let non_retriable = matches!(err, TaskError::HandlerNotFound(_));
            let mut patch = serde_json::Map::new();
            if exhausted || non_retriable {
                patch.insert(
                    "status".to_string(),
                    serde_json::Value::String(STATUS_FAILED.to_string()),
                );
                patch.insert("completed_at".to_string(), serde_json::to_value(now)?);
                patch.insert("error".to_string(), serde_json::Value::String(err_msg));
            } else {
                // Reset to pending so a later worker iteration retries.
                // `attempts` already incremented in `claim_one`, so the
                // count is accurate. Clear `started_at` so the next claim
                // stamps a fresh timestamp, and push `run_at` into the
                // future by the exponential backoff so the row isn't
                // re-claimed until the delay elapses (Celery-style retry
                // backoff instead of the old immediate re-queue).
                let run_at = policy.next_run_at(row.attempts, now);
                patch.insert(
                    "status".to_string(),
                    serde_json::Value::String(STATUS_PENDING.to_string()),
                );
                patch.insert("started_at".to_string(), serde_json::Value::Null);
                patch.insert("run_at".to_string(), serde_json::to_value(run_at)?);
                patch.insert("error".to_string(), serde_json::Value::String(err_msg));
            }
            TaskRow::objects()
                .filter(task_row::ID.eq(row.id))
                .update_values(patch)
                .await?;
        }
    }
    Ok(())
}

// =========================================================================
// Periodic / cron scheduling — the "beat" (Celery beat parity).
// =========================================================================

/// A recurring schedule: either a standard cron expression or a fixed
/// interval. Serializes to a single string column on [`PeriodicTask`] so
/// the schedule persists alongside the row.
///
/// ## Cron format
///
/// [`Schedule::cron`] accepts a **standard 5-field** expression
/// (`min hour day-of-month month day-of-week`, e.g. `"0 0 * * *"` for
/// midnight daily). Internally a leading `0 ` seconds field is prepended
/// for the `cron` crate, which wants a 6-field (`sec min hour dom mon dow`)
/// expression. A 6-field expression is passed through unchanged, so the
/// seconds field is available if you need it.
///
/// ## Serialized form
///
/// - Cron: `"cron:0 0 * * *"`
/// - Interval: `"every:3600"` (seconds)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    /// A cron expression (5 or 6 field — see the type docs).
    Cron(String),
    /// Fire every fixed `Duration` after the previous run.
    Every(Duration),
}

impl Schedule {
    /// A cron schedule from a standard 5-field (or 6-field) expression.
    pub fn cron(expr: impl Into<String>) -> Self {
        Schedule::Cron(expr.into())
    }

    /// A fixed-interval schedule firing every `period`.
    pub fn every(period: Duration) -> Self {
        Schedule::Every(period)
    }

    /// The next fire time strictly after `after`, or `None` if the schedule
    /// will never fire again (an exhausted or unparseable cron). For
    /// [`Schedule::Every`] this is always `Some(after + period)`.
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Schedule::Cron(expr) => {
                let normalized = normalize_cron(expr);
                let schedule = cron::Schedule::from_str(&normalized).ok()?;
                schedule.after(&after).next()
            }
            Schedule::Every(period) => {
                let delta = chrono::Duration::from_std(*period).ok()?;
                Some(after + delta)
            }
        }
    }

    /// Serialize to the single string stored in the `schedule` column.
    pub fn to_storage(&self) -> String {
        match self {
            Schedule::Cron(expr) => format!("cron:{expr}"),
            Schedule::Every(period) => format!("every:{}", period.as_secs()),
        }
    }

    /// Parse the stored string form back into a `Schedule`.
    pub fn from_storage(s: &str) -> Option<Schedule> {
        if let Some(expr) = s.strip_prefix("cron:") {
            Some(Schedule::Cron(expr.to_string()))
        } else if let Some(secs) = s.strip_prefix("every:") {
            secs.parse::<u64>().ok().map(|n| Schedule::Every(Duration::from_secs(n)))
        } else {
            None
        }
    }
}

/// Prepend a seconds field to a 5-field cron expression so the `cron`
/// crate (which wants `sec min hour dom mon dow`) accepts it. A 6+ field
/// expression is returned unchanged.
fn normalize_cron(expr: &str) -> String {
    let fields = expr.split_whitespace().count();
    if fields == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    }
}

use std::str::FromStr;

/// A persisted recurring task. One row per [`PeriodicSpec::name`]; the beat
/// loop advances `next_run` each time it fires the underlying `task`.
///
/// Columns are nullable / defaulted so the model migrates additively
/// against an existing `task_row`-only DB (same lesson as `run_at`): a
/// brand-new table is created by `makemigrations`/`migrate`, and the
/// non-nullable identity columns (`name`/`task`/`payload`/`schedule`/
/// `next_run`) are only ever written by code that fills them.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct PeriodicTask {
    pub id: i64,
    /// The schedule's stable key. One row per name; re-registering a spec
    /// with the same name updates this row rather than duplicating it.
    #[umbra(unique)]
    pub name: String,
    /// The handler name [`run_beat`] enqueues when the schedule fires.
    pub task: String,
    /// JSON args passed to the enqueued task.
    pub payload: String,
    /// The serialized [`Schedule`] (`"cron:..."` / `"every:..."`).
    pub schedule: String,
    /// The next instant this task is due. The beat loop claims rows whose
    /// `next_run <= now` and advances this forward.
    pub next_run: DateTime<Utc>,
    /// When the schedule last fired (`None` until the first fire).
    pub last_run: Option<DateTime<Utc>>,
    /// Whether the schedule is active. A schedule that yields no further
    /// fire time (`next_after` returns `None`) is disabled here.
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A recurring-task registration collected by [`TasksPlugin::periodic`] and
/// upserted to a [`PeriodicTask`] row on beat startup.
#[derive(Debug, Clone)]
pub struct PeriodicSpec {
    /// The schedule's stable key (the row's `name`).
    pub name: String,
    /// The schedule.
    pub schedule: Schedule,
    /// The handler name to enqueue each fire.
    pub task: String,
    /// JSON-encoded args for the enqueued task.
    pub payload: String,
}

/// Process-wide registry of the periodic specs collected on the builder.
/// Installed in [`Plugin::on_ready`] (sync), consumed by [`run_beat`]'s
/// async startup sync. Mirrors the worker's [`HANDLERS`] OnceLock.
static REGISTERED_PERIODIC: OnceLock<Vec<PeriodicSpec>> = OnceLock::new();

/// Test-only escape hatch: drop the installed periodic registry so a case
/// can re-publish its own specs. Only resets if it was set.
#[doc(hidden)]
pub fn _registered_periodic() -> Option<&'static Vec<PeriodicSpec>> {
    REGISTERED_PERIODIC.get()
}

/// Sync the registered [`PeriodicSpec`]s to `PeriodicTask` rows. Inserts a
/// new row for each previously-unseen `name` (computing `next_run` from the
/// schedule); for an existing row, updates `task`/`payload`/`schedule` and
/// recomputes `next_run` only if the schedule string changed, leaving
/// `last_run` intact. Idempotent.
///
/// Returns the number of rows inserted or updated. Driven on beat startup
/// from the ambient registry; tests call [`sync_periodic_specs`] directly.
pub async fn sync_registered_periodic() -> Result<u64, TaskError> {
    let Some(specs) = REGISTERED_PERIODIC.get() else {
        return Ok(0);
    };
    sync_periodic_specs(specs).await
}

/// [`sync_registered_periodic`] over an explicit spec slice (test entry
/// point that doesn't depend on the ambient OnceLock).
pub async fn sync_periodic_specs(specs: &[PeriodicSpec]) -> Result<u64, TaskError> {
    let now = Utc::now();
    let mut changed: u64 = 0;
    for spec in specs {
        let storage = spec.schedule.to_storage();
        let existing = PeriodicTask::objects()
            .filter(periodic_task::NAME.eq(spec.name.as_str()))
            .first()
            .await?;
        match existing {
            None => {
                let next_run = spec.schedule.next_after(now).unwrap_or(now);
                PeriodicTask::objects()
                    .create(PeriodicTask {
                        id: 0,
                        name: spec.name.clone(),
                        task: spec.task.clone(),
                        payload: spec.payload.clone(),
                        schedule: storage,
                        next_run,
                        last_run: None,
                        enabled: true,
                        created_at: now,
                        updated_at: now,
                    })
                    .await?;
                changed += 1;
            }
            Some(row) => {
                let mut patch = serde_json::Map::new();
                patch.insert(
                    "task".to_string(),
                    serde_json::Value::String(spec.task.clone()),
                );
                patch.insert(
                    "payload".to_string(),
                    serde_json::Value::String(spec.payload.clone()),
                );
                patch.insert(
                    "schedule".to_string(),
                    serde_json::Value::String(storage.clone()),
                );
                patch.insert("updated_at".to_string(), serde_json::to_value(now)?);
                // Only recompute `next_run` when the schedule actually
                // changed — otherwise an unchanged re-sync would keep
                // shoving the next fire time forward and starve the task.
                if row.schedule != storage {
                    let next_run = spec.schedule.next_after(now).unwrap_or(now);
                    patch.insert("next_run".to_string(), serde_json::to_value(next_run)?);
                }
                let affected = PeriodicTask::objects()
                    .filter(periodic_task::ID.eq(row.id))
                    .update_values(patch)
                    .await?;
                changed += affected;
            }
        }
    }
    Ok(changed)
}

/// Default beat poll interval: how long to sleep between ticks when no row
/// is due. Celery beat defaults to a 5-minute max-loop but checks far more
/// often; 5s is a reasonable resolution for second-granularity crons.
pub const DEFAULT_BEAT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Options for [`run_beat`].
pub struct BeatOptions {
    /// How long to sleep between ticks. Defaults to
    /// [`DEFAULT_BEAT_POLL_INTERVAL`] (5s).
    pub poll_interval: Duration,
    /// Flip to `true` to cleanly exit after the in-flight tick. A
    /// never-fires channel is installed by [`BeatOptions::default`].
    pub shutdown: watch::Receiver<bool>,
}

impl Default for BeatOptions {
    fn default() -> Self {
        let (_tx, rx) = watch::channel(false);
        std::mem::forget(_tx);
        Self {
            poll_interval: DEFAULT_BEAT_POLL_INTERVAL,
            shutdown: rx,
        }
    }
}

/// The beat loop. Syncs the registered schedules once on startup, then each
/// tick fires every due [`PeriodicTask`] (atomically claimed so multiple
/// beat instances can't double-enqueue) and sleeps `poll_interval`. Runs
/// until `opts.shutdown` flips. Like [`run_worker`], it never calls
/// `process::exit` — it returns so a single-binary deployment that spawned
/// it can tear down cleanly.
pub async fn run_beat(mut opts: BeatOptions) {
    if let Err(e) = sync_registered_periodic().await {
        tracing::error!(error = %e, "umbra-tasks: beat startup sync failed");
    }
    loop {
        if *opts.shutdown.borrow() {
            return;
        }
        match fire_due_periodic().await {
            Ok(fired) if fired > 0 => {
                tracing::info!(count = fired, "umbra-tasks: beat fired periodic tasks");
            }
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = %e, "umbra-tasks: beat tick failed");
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(opts.poll_interval) => {}
            _ = opts.shutdown.changed() => {}
        }
    }
}

/// Single beat step for tests / cron-driven beats: sync the registered
/// schedules, then fire every due row once. Returns the number of tasks
/// enqueued this tick.
pub async fn run_beat_once() -> Result<u64, TaskError> {
    sync_registered_periodic().await?;
    fire_due_periodic().await
}

/// Fire every due periodic task: for each `enabled` row whose
/// `next_run <= now`, atomically claim it (advance `next_run` to the next
/// fire time and stamp `last_run`) and — only if the claim won the race —
/// enqueue the underlying task. Returns the number of tasks enqueued.
///
/// The claim is an optimistic conditional UPDATE: `... WHERE id = ? AND
/// next_run = <the value we read>`. [`QuerySet::update_values`] returns the
/// affected-row count, so we enqueue only when it's `1`. A second beat
/// instance that read the same row loses the race — its UPDATE matches
/// nothing (the `next_run` guard already moved) and affects `0` rows, so it
/// enqueues nothing. This is the multi-instance double-enqueue guard,
/// mirroring [`claim_one`]'s conditional claim on the worker side.
pub async fn fire_due_periodic() -> Result<u64, TaskError> {
    let now = Utc::now();
    let due: Vec<PeriodicTask> = PeriodicTask::objects()
        .filter(periodic_task::ENABLED.eq(true) & periodic_task::NEXT_RUN.le(now))
        .order_by(periodic_task::NEXT_RUN.asc())
        .order_by(periodic_task::ID.asc())
        .fetch()
        .await?;

    let mut fired: u64 = 0;
    for row in due {
        let Some(schedule) = Schedule::from_storage(&row.schedule) else {
            tracing::warn!(
                name = %row.name,
                schedule = %row.schedule,
                "umbra-tasks: periodic task has an unparseable schedule; disabling"
            );
            disable_periodic(row.id).await?;
            continue;
        };

        match schedule.next_after(now) {
            Some(next_run) => {
                let mut patch = serde_json::Map::new();
                patch.insert("next_run".to_string(), serde_json::to_value(next_run)?);
                patch.insert("last_run".to_string(), serde_json::to_value(now)?);
                patch.insert("updated_at".to_string(), serde_json::to_value(now)?);
                // Optimistic claim: only one beat instance can advance the
                // row from THIS exact `next_run`. `affected == 1` means we
                // won and may enqueue; `0` means another instance beat us.
                let affected = PeriodicTask::objects()
                    .filter(
                        periodic_task::ID.eq(row.id) & periodic_task::NEXT_RUN.eq(row.next_run),
                    )
                    .update_values(patch)
                    .await?;
                if affected == 1 {
                    enqueue_periodic(&row).await?;
                    fired += 1;
                }
            }
            None => {
                // No further fire time — disable the schedule.
                disable_periodic(row.id).await?;
            }
        }
    }
    Ok(fired)
}

/// Enqueue the task a periodic row fires. The stored `payload` is already a
/// JSON string, so it's enqueued verbatim (re-serializing a `&str` would
/// double-encode it).
async fn enqueue_periodic(row: &PeriodicTask) -> Result<(), TaskError> {
    let now = Utc::now();
    TaskRow::objects()
        .create(TaskRow {
            id: 0,
            name: row.task.clone(),
            payload: row.payload.clone(),
            status: STATUS_PENDING.to_string(),
            attempts: 0,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            scheduled_for: now,
            run_at: Some(now),
            started_at: None,
            completed_at: None,
            error: None,
            created_at: now,
        })
        .await?;
    Ok(())
}

/// Disable a periodic row (a schedule that will never fire again).
async fn disable_periodic(id: i64) -> Result<(), TaskError> {
    let now = Utc::now();
    let mut patch = serde_json::Map::new();
    patch.insert("enabled".to_string(), serde_json::Value::Bool(false));
    patch.insert("updated_at".to_string(), serde_json::to_value(now)?);
    PeriodicTask::objects()
        .filter(periodic_task::ID.eq(id))
        .update_values(patch)
        .await?;
    Ok(())
}
