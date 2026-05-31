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
//! - No `#[task]` macro yet; callers pass the handler name as a string
//!   to [`enqueue`] and register a closure under the same name. The
//!   proc-macro lands when the deep spec promotes from the outline.
//! - No periodic scheduling ("beat"). Deferred to the deep spec.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::watch;
use umbra::prelude::*;

/// Status string for a freshly enqueued row, or a row a failing handler
/// has been reset to so it can retry.
pub const STATUS_PENDING: &str = "pending";
/// Status string while a worker is mid-execution. Useful for observability;
/// a crashed worker leaves the row in this state until manual cleanup or a
/// future timeout-watcher reclaims it.
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
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// The plugin. Registers the [`TaskRow`] model.
#[derive(Debug, Default)]
pub struct TasksPlugin;

impl Plugin for TasksPlugin {
    fn name(&self) -> &'static str {
        "tasks"
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        vec![umbra::migrate::ModelMeta::for_::<TaskRow>()]
    }

    fn commands(&self) -> Vec<Box<dyn umbra::cli::PluginCommand>> {
        vec![Box::new(WorkerCommand)]
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
            run_worker(WorkerOptions::default()).await
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
    let pool = umbra::db::pool();
    let payload_json = serde_json::to_string(&payload)?;
    let now = Utc::now();
    let scheduled_for = opts.scheduled_for.unwrap_or(now);
    let max_attempts = opts.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS);

    let row: (i64,) = sqlx::query_as(
        "INSERT INTO task_row (\
            name, payload, status, attempts, max_attempts, \
            scheduled_for, started_at, completed_at, error, created_at\
         ) VALUES (?, ?, ?, 0, ?, ?, NULL, NULL, NULL, ?) RETURNING id",
    )
    .bind(name)
    .bind(&payload_json)
    .bind(STATUS_PENDING)
    .bind(max_attempts)
    .bind(scheduled_for)
    .bind(now)
    .fetch_one(&pool)
    .await?;
    Ok(row.0)
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

/// Options for [`run_worker`]. Carries the poll interval and a
/// shutdown receiver so a real binary can wire `Ctrl-C` into the loop.
pub struct WorkerOptions {
    /// How long to sleep when the queue is empty. Defaults to 1 second.
    pub poll_interval: Duration,
    /// Setting this to `true` cleanly exits the worker after the
    /// in-flight iteration finishes. A default never-fires channel is
    /// installed when callers use [`WorkerOptions::default`].
    pub shutdown: watch::Receiver<bool>,
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
        }
    }
}

/// The polling loop. Runs forever (or until `opts.shutdown` flips to
/// `true`): claim one pending due row, dispatch its handler, write back
/// the terminal state, loop.
///
/// Never returns under normal operation; the `!` return type signals
/// that to the type system. A graceful shutdown is modelled as a
/// `panic!` rather than a normal return because v1 only needs the
/// process-exit semantics; M10+ can lift this to a `Result`.
pub async fn run_worker(mut opts: WorkerOptions) -> ! {
    loop {
        if *opts.shutdown.borrow() {
            // Cooperative shutdown. Process exits cleanly.
            std::process::exit(0);
        }
        match run_worker_once().await {
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

/// Single-iteration worker step. Returns `Ok(true)` if a task was
/// processed (regardless of whether the handler succeeded), `Ok(false)`
/// if the queue had no due rows.
///
/// Test-driver entry point: integration tests can drive deterministic
/// scenarios without spawning the polling loop.
pub async fn run_worker_once() -> Result<bool, TaskError> {
    let pool = umbra::db::pool();
    let Some(row) = claim_one(&pool).await? else {
        return Ok(false);
    };
    process_one(&pool, row).await?;
    Ok(true)
}

/// Atomically claim one pending due row by flipping it to `running` and
/// returning the row contents. Wrapped in a transaction so a concurrent
/// worker can't double-claim — SQLite's single-writer model already
/// guarantees this, but the explicit transaction keeps the contract
/// correct for the Postgres backend.
async fn claim_one(pool: &SqlitePool) -> Result<Option<TaskRow>, TaskError> {
    let now = Utc::now();
    let mut tx = pool.begin().await?;
    let candidate: Option<TaskRow> = sqlx::query_as::<_, TaskRow>(
        "SELECT * FROM task_row \
         WHERE status = ? AND scheduled_for <= ? \
         ORDER BY scheduled_for ASC, id ASC \
         LIMIT 1",
    )
    .bind(STATUS_PENDING)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(mut row) = candidate else {
        tx.commit().await?;
        return Ok(None);
    };
    sqlx::query(
        "UPDATE task_row SET status = ?, started_at = ?, attempts = attempts + 1 WHERE id = ?",
    )
    .bind(STATUS_RUNNING)
    .bind(now)
    .bind(row.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    // Reflect the in-DB mutations in the row we return so the caller
    // doesn't need to re-SELECT.
    row.status = STATUS_RUNNING.to_string();
    row.started_at = Some(now);
    row.attempts += 1;
    Ok(Some(row))
}

/// Run the handler for a claimed row and write back the terminal state.
async fn process_one(pool: &SqlitePool, row: TaskRow) -> Result<(), TaskError> {
    let handler = {
        let guard = handlers()
            .read()
            .expect("umbra-tasks: handler registry poisoned");
        guard
            .get(row.name.as_str())
            .map(|h| h(&row.payload))
            .map(|fut| (fut,))
    };
    let result = match handler {
        Some((fut,)) => {
            // Catch panics so one bad handler doesn't take the worker
            // down. `tokio::task::spawn` gives us the JoinHandle whose
            // join error carries panic payloads we can stringify.
            let outcome = tokio::task::spawn(fut).await;
            match outcome {
                Ok(r) => r,
                Err(join) if join.is_panic() => {
                    Err(format!("handler panicked: {:?}", join.into_panic()))
                }
                Err(join) => Err(format!("handler join error: {join}")),
            }
        }
        None => Err(format!("handler not found: {}", row.name)),
    };

    let now = Utc::now();
    match result {
        Ok(()) => {
            sqlx::query(
                "UPDATE task_row SET status = ?, completed_at = ?, error = NULL WHERE id = ?",
            )
            .bind(STATUS_SUCCEEDED)
            .bind(now)
            .bind(row.id)
            .execute(pool)
            .await?;
        }
        Err(err_msg) => {
            // "handler not found" is non-retriable — a missing handler
            // won't appear on the next attempt unless the operator
            // changes the code. Treat as terminal failure regardless
            // of attempts.
            let exhausted = row.attempts >= row.max_attempts;
            let non_retriable = err_msg.starts_with("handler not found");
            if exhausted || non_retriable {
                sqlx::query(
                    "UPDATE task_row SET status = ?, completed_at = ?, error = ? WHERE id = ?",
                )
                .bind(STATUS_FAILED)
                .bind(now)
                .bind(&err_msg)
                .bind(row.id)
                .execute(pool)
                .await?;
            } else {
                // Reset to pending so the next worker iteration retries.
                // `attempts` already incremented in `claim_one`, so the
                // count is accurate. Clear `started_at` so the next
                // claim stamps a fresh timestamp.
                sqlx::query(
                    "UPDATE task_row SET status = ?, started_at = NULL, error = ? WHERE id = ?",
                )
                .bind(STATUS_PENDING)
                .bind(&err_msg)
                .bind(row.id)
                .execute(pool)
                .await?;
            }
        }
    }
    Ok(())
}
