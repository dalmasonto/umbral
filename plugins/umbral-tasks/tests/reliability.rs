//! Reliability & scheduling coverage for umbral-tasks: eta/delay
//! scheduling (the `run_at` eligibility gate), exponential-backoff retries
//! (a failure pushes `run_at` into the future and grows with attempts,
//! then abandons at max_attempts), and per-task timeouts (an overrunning
//! handler is recorded as a retriable failure, not left hanging).
//!
//! Same boot shape as `integration.rs`: one OnceCell-backed tempfile
//! sqlite pool, registered TasksPlugin, raw SQL CREATE TABLE because the
//! integration test owns its own schema without standing up the M5
//! migration loop.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbral_tasks::{
    _clear_handlers_for_tests, EnqueueOptions, RetryPolicy, STATUS_FAILED, STATUS_PENDING, TaskRow,
    TasksPlugin, enqueue, register_handler, run_worker_once, run_worker_once_with,
};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tasks_reliability.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(TasksPlugin::default())
            .build()
            .expect("App::build with TasksPlugin");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

async fn fetch(id: i64) -> TaskRow {
    let pool = umbral::db::pool();
    sqlx::query_as::<_, TaskRow>("SELECT * FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch row")
}

/// Drain every row so each test sees only its own data.
async fn drain_queue() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM task_row")
        .execute(&pool)
        .await
        .expect("drain");
}

/// These tests mutate process-global state (the handler registry, the
/// shared queue), so they must not interleave.
static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

// =========================================================================
// 1. eta / delay scheduling — the run_at eligibility gate.
// =========================================================================

/// A task enqueued with a future `delay` is NOT claimed before its
/// `run_at`; a task enqueued with an `eta` already in the past IS claimed.
/// We drive "time" by enqueuing with a future vs. past run_at rather than
/// sleeping, asserting on what the claim query returns for each.
#[tokio::test(flavor = "multi_thread")]
async fn delayed_task_is_not_claimed_before_run_at() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    static CALLS: OnceLock<AtomicUsize> = OnceLock::new();
    CALLS
        .get_or_init(|| AtomicUsize::new(0))
        .store(0, Ordering::SeqCst);
    register_handler("delayed", |_payload: &str| async move {
        CALLS.get().unwrap().fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    // Future delay: run_at = now + 1h. Must NOT be claimed.
    let future_id = enqueue(
        "delayed",
        serde_json::json!({}),
        EnqueueOptions {
            delay: Some(Duration::from_secs(3600)),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue future");

    let processed = run_worker_once().await.expect("worker step (future)");
    assert!(
        !processed,
        "a task whose run_at is in the future must not be claimed"
    );
    let row = fetch(future_id).await;
    assert_eq!(row.status, STATUS_PENDING, "future task stays pending");
    assert_eq!(row.attempts, 0, "future task not attempted");
    assert!(
        row.run_at.expect("run_at set") > Utc::now(),
        "delay should put run_at in the future"
    );
    assert_eq!(
        CALLS.get().unwrap().load(Ordering::SeqCst),
        0,
        "handler must not run for a not-yet-due task"
    );

    // Past eta: run_at = now - 1h. MUST be claimed and run.
    let past_id = enqueue(
        "delayed",
        serde_json::json!({}),
        EnqueueOptions {
            eta: Some(Utc::now() - chrono::Duration::hours(1)),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue past");

    let processed = run_worker_once().await.expect("worker step (past)");
    assert!(processed, "a task whose run_at is past must be claimed");
    let row = fetch(past_id).await;
    assert_eq!(
        row.status,
        umbral_tasks::STATUS_SUCCEEDED,
        "past-eta task should run to completion"
    );
    assert_eq!(
        CALLS.get().unwrap().load(Ordering::SeqCst),
        1,
        "exactly the due task ran"
    );

    // The future task is still untouched.
    let row = fetch(future_id).await;
    assert_eq!(row.status, STATUS_PENDING);
    assert_eq!(row.attempts, 0);
}

/// `eta` takes precedence over `delay` when both are supplied.
#[tokio::test(flavor = "multi_thread")]
async fn eta_wins_over_delay() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    let eta = Utc::now() + chrono::Duration::hours(5);
    let id = enqueue(
        "never_registered",
        serde_json::json!({}),
        EnqueueOptions {
            eta: Some(eta),
            // A tiny delay that, if it won, would make the row due now.
            delay: Some(Duration::from_secs(0)),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    let row = fetch(id).await;
    let run_at = row.run_at.expect("run_at set");
    assert!(
        (run_at - eta).num_seconds().abs() < 2,
        "eta must win over delay; run_at={run_at}, eta={eta}"
    );
}

// =========================================================================
// 2. exponential-backoff retries — run_at grows with attempts, then abandon.
// =========================================================================

/// A failing task's `run_at` after a retry is advanced into the future by
/// ~the backoff, and the delay grows with each attempt; after
/// `max_attempts` it is abandoned (FAILED), not re-queued.
#[tokio::test(flavor = "multi_thread")]
async fn retry_backs_off_then_abandons() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    register_handler("always_fails", |_payload: &str| async move {
        Err::<(), String>("boom".to_string())
    });

    // base=10s, max=1h: attempt 1 -> ~10s, attempt 2 -> ~20s.
    let policy = RetryPolicy {
        backoff_base: Duration::from_secs(10),
        backoff_max: Duration::from_secs(3600),
        task_timeout: None,
    };

    let id = enqueue(
        "always_fails",
        serde_json::json!({}),
        EnqueueOptions {
            max_attempts: Some(3),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    // First failure: attempts=1, backed off ~10s into the future.
    let before = Utc::now();
    assert!(run_worker_once_with(policy).await.expect("step 1"));
    let row = fetch(id).await;
    assert_eq!(
        row.status, STATUS_PENDING,
        "retriable failure stays pending"
    );
    assert_eq!(row.attempts, 1);
    let run_at_1 = row.run_at.expect("run_at set after retry");
    assert!(
        run_at_1 > before,
        "run_at must be pushed into the future on retry"
    );
    let delay_1 = (run_at_1 - before).num_seconds();
    assert!(
        (8..=14).contains(&delay_1),
        "first backoff ~10s, got {delay_1}s"
    );
    // started_at cleared so a future claim re-stamps it.
    assert!(row.started_at.is_none(), "started_at cleared on retry");

    // The row is NOT due yet, so a default-policy worker step finds nothing.
    let processed = run_worker_once().await.expect("nothing due");
    assert!(
        !processed,
        "backed-off task must not be re-claimed before run_at"
    );

    // Simulate the backoff elapsing: clear run_at so it's eligible again.
    let pool = umbral::db::pool();
    sqlx::query("UPDATE task_row SET run_at = NULL WHERE id = ?")
        .bind(id)
        .execute(&pool)
        .await
        .expect("clear run_at");

    // Second failure: attempts=2, backed off ~20s (grows with attempts).
    let before2 = Utc::now();
    assert!(run_worker_once_with(policy).await.expect("step 2"));
    let row = fetch(id).await;
    assert_eq!(row.status, STATUS_PENDING);
    assert_eq!(row.attempts, 2);
    let delay_2 = (row.run_at.expect("run_at") - before2).num_seconds();
    assert!(
        (16..=28).contains(&delay_2),
        "second backoff ~20s, got {delay_2}s"
    );
    assert!(
        delay_2 > delay_1,
        "backoff must grow with attempts: {delay_1}s -> {delay_2}s"
    );

    // Make it eligible once more, then the third failure exhausts attempts.
    sqlx::query("UPDATE task_row SET run_at = NULL WHERE id = ?")
        .bind(id)
        .execute(&pool)
        .await
        .expect("clear run_at");

    assert!(run_worker_once_with(policy).await.expect("step 3"));
    let row = fetch(id).await;
    assert_eq!(
        row.status, STATUS_FAILED,
        "after max_attempts the task is abandoned, not re-queued"
    );
    assert_eq!(row.attempts, 3, "exactly max_attempts attempts");
    assert!(row.completed_at.is_some(), "completed_at set on abandon");
}

/// The backoff is capped at `retry_backoff_max`.
#[tokio::test(flavor = "multi_thread")]
async fn backoff_is_capped_at_max() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    register_handler("always_fails_capped", |_payload: &str| async move {
        Err::<(), String>("boom".to_string())
    });

    // base huge, max tiny: first retry must clamp to max (~5s).
    let policy = RetryPolicy {
        backoff_base: Duration::from_secs(3600),
        backoff_max: Duration::from_secs(5),
        task_timeout: None,
    };

    let id = enqueue(
        "always_fails_capped",
        serde_json::json!({}),
        EnqueueOptions {
            max_attempts: Some(5),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    let before = Utc::now();
    assert!(run_worker_once_with(policy).await.expect("step"));
    let row = fetch(id).await;
    let delay = (row.run_at.expect("run_at") - before).num_seconds();
    assert!(
        delay <= 7,
        "backoff must be capped at retry_backoff_max (~5s), got {delay}s"
    );
}

// =========================================================================
// 3. per-task timeout — an overrunning handler is a retriable failure.
// =========================================================================

/// A handler that sleeps past a tiny `task_timeout` is recorded as a
/// failed attempt (and backed off / abandoned) rather than hanging the
/// worker.
#[tokio::test(flavor = "multi_thread")]
async fn slow_handler_times_out_and_is_recorded_as_failure() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    register_handler("slow", |_payload: &str| async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    });

    // 50ms timeout, 500ms handler. Zero backoff so we can re-drive quickly,
    // and max_attempts=1 so the single timeout is terminal.
    let policy = RetryPolicy {
        backoff_base: Duration::from_secs(0),
        backoff_max: Duration::from_secs(0),
        task_timeout: Some(Duration::from_millis(50)),
    };

    let id = enqueue(
        "slow",
        serde_json::json!({}),
        EnqueueOptions {
            max_attempts: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    // The whole step must finish well under the handler's 500ms sleep:
    // the timeout cancels it at ~50ms. Bound the test so a hang fails loud.
    let processed = tokio::time::timeout(Duration::from_secs(2), run_worker_once_with(policy))
        .await
        .expect("worker step must not hang past the task timeout")
        .expect("worker step");
    assert!(processed, "a timed-out task still counts as processed");

    let row = fetch(id).await;
    assert_eq!(
        row.status, STATUS_FAILED,
        "a single timeout with max_attempts=1 is terminal"
    );
    assert_eq!(row.attempts, 1, "the timeout counts as one attempt");
    let err = row.error.expect("timeout error recorded");
    assert!(
        err.contains("timed out"),
        "error column should explain the timeout; got {err:?}"
    );
}

/// A timeout on a task with retries left is retriable: it backs off and
/// stays pending rather than going terminal.
#[tokio::test(flavor = "multi_thread")]
async fn timeout_with_retries_left_backs_off() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();

    register_handler("slow_retry", |_payload: &str| async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    });

    let policy = RetryPolicy {
        backoff_base: Duration::from_secs(30),
        backoff_max: Duration::from_secs(3600),
        task_timeout: Some(Duration::from_millis(50)),
    };

    let id = enqueue(
        "slow_retry",
        serde_json::json!({}),
        EnqueueOptions {
            max_attempts: Some(3),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    let before = Utc::now();
    let processed = tokio::time::timeout(Duration::from_secs(2), run_worker_once_with(policy))
        .await
        .expect("must not hang")
        .expect("worker step");
    assert!(processed);

    let row = fetch(id).await;
    assert_eq!(
        row.status, STATUS_PENDING,
        "a timeout with retries left is retriable"
    );
    assert_eq!(row.attempts, 1);
    let delay = (row.run_at.expect("run_at") - before).num_seconds();
    assert!(
        (25..=40).contains(&delay),
        "timeout retry should back off ~30s, got {delay}s"
    );
}
