//! Priority-queue coverage for umbra-tasks: higher-priority rows are
//! claimed before lower-priority ones, ties break FIFO (scheduled_for then
//! id), and a legacy `NULL`-priority row drains at the lowest priority.
//!
//! Same boot shape as `integration.rs`: one OnceCell-backed tempfile sqlite
//! pool, a registered `TasksPlugin`, and a raw `CREATE TABLE task_row` that
//! owns the schema without standing up the M5 migration loop. The
//! `priority INTEGER` column mirrors the additive nullable migration the
//! real app's `makemigrations`/`migrate` would emit.

use std::sync::OnceLock;
use std::sync::Mutex as StdMutex;

use chrono::{Duration as ChronoDuration, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbra_tasks::{
    EnqueueOptions, TaskRow, TasksPlugin, _clear_handlers_for_tests, enqueue, register_handler,
    run_worker_once, STATUS_PENDING, STATUS_SUCCEEDED,
};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tasks_priority.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(TasksPlugin::default())
            .build()
            .expect("App::build with TasksPlugin");

        let pool = umbra::db::pool();
        sqlx::query(
            "CREATE TABLE task_row (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                name TEXT NOT NULL,\
                payload TEXT NOT NULL,\
                status TEXT NOT NULL,\
                attempts INTEGER NOT NULL,\
                max_attempts INTEGER NOT NULL,\
                scheduled_for TEXT NOT NULL,\
                run_at TEXT,\
                started_at TEXT,\
                completed_at TEXT,\
                error TEXT,\
                result TEXT,\
                priority INTEGER,\
                created_at TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create task_row");
    })
    .await;
}

/// Per-test serialisation: every test shares the handler registry and the
/// `task_row` table, so they can't run in parallel.
static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

async fn drain_queue() {
    let pool = umbra::db::pool();
    sqlx::query("DELETE FROM task_row")
        .execute(&pool)
        .await
        .expect("drain task_row");
}

async fn fetch(id: i64) -> TaskRow {
    let pool = umbra::db::pool();
    sqlx::query_as::<_, TaskRow>("SELECT * FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch row")
}

/// Order in which the handler observed tasks. The handler records its
/// payload `label` so a test can assert the claim sequence.
static CLAIM_ORDER: OnceLock<StdMutex<Vec<String>>> = OnceLock::new();

fn claim_order() -> &'static StdMutex<Vec<String>> {
    CLAIM_ORDER.get_or_init(|| StdMutex::new(Vec::new()))
}

fn reset_claim_order() {
    claim_order().lock().unwrap().clear();
}

fn observed_order() -> Vec<String> {
    claim_order().lock().unwrap().clone()
}

/// Register a single recording handler under `record`. Every enqueued task
/// in these tests fires it; the payload's `label` field is appended to
/// [`CLAIM_ORDER`] in claim order.
fn register_recorder() {
    register_handler("record", |payload: &str| {
        // Parse the label up front (synchronously) so the borrow of
        // `payload` doesn't cross the await boundary the async block needs.
        let v: serde_json::Value = serde_json::from_str(payload).unwrap_or(serde_json::Value::Null);
        let label = v
            .get("label")
            .and_then(|l| l.as_str())
            .unwrap_or("?")
            .to_string();
        async move {
            claim_order().lock().unwrap().push(label);
            Ok::<(), String>(())
        }
    });
}

/// Drain the whole queue one task at a time, asserting each step claimed a
/// row, until `run_worker_once` reports the queue empty. Returns nothing;
/// the claim order is in [`CLAIM_ORDER`].
async fn drain_recording() {
    // Generous upper bound so a logic bug can't spin forever.
    for _ in 0..32 {
        let processed = run_worker_once().await.expect("worker step");
        if !processed {
            break;
        }
    }
}

async fn enqueue_record(label: &str, priority: Option<i32>) -> i64 {
    enqueue(
        "record",
        serde_json::json!({ "label": label }),
        EnqueueOptions {
            priority,
            ..Default::default()
        },
    )
    .await
    .expect("enqueue")
}

// =========================================================================
// 1. higher priority claimed first
// =========================================================================

/// Enqueue a priority-0 task, THEN a priority-9 task. Despite the 0 being
/// enqueued first (lower id), the worker must claim the priority-9 task
/// first, then the priority-0 one.
#[tokio::test(flavor = "multi_thread")]
async fn higher_priority_is_claimed_before_lower() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    reset_claim_order();
    register_recorder();

    enqueue_record("normal", Some(0)).await;
    enqueue_record("urgent", Some(9)).await;

    drain_recording().await;

    assert_eq!(
        observed_order(),
        vec!["urgent".to_string(), "normal".to_string()],
        "priority-9 task must be claimed before the priority-0 task",
    );
}

// =========================================================================
// 2. FIFO within a priority
// =========================================================================

/// Two priority-5 tasks enqueued in order claim in enqueue order: ties on
/// priority break by `scheduled_for` then `id`, so the first-enqueued (lower
/// id) wins.
#[tokio::test(flavor = "multi_thread")]
async fn ties_break_fifo_within_a_priority() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    reset_claim_order();
    register_recorder();

    enqueue_record("first", Some(5)).await;
    enqueue_record("second", Some(5)).await;

    drain_recording().await;

    assert_eq!(
        observed_order(),
        vec!["first".to_string(), "second".to_string()],
        "within one priority, claims stay FIFO (enqueue order)",
    );
}

// =========================================================================
// 3. mixed priorities order strictly by priority, then FIFO
// =========================================================================

/// A spread of priorities enqueued out of order still drains
/// highest-priority-first, FIFO within each band.
#[tokio::test(flavor = "multi_thread")]
async fn mixed_priorities_drain_high_to_low_then_fifo() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    reset_claim_order();
    register_recorder();

    // Enqueue deliberately scrambled so id-order != priority-order.
    enqueue_record("p1", Some(1)).await;
    enqueue_record("p9_a", Some(9)).await;
    enqueue_record("p5", Some(5)).await;
    enqueue_record("p9_b", Some(9)).await; // same priority as p9_a, later id
    enqueue_record("p0", Some(0)).await;

    drain_recording().await;

    assert_eq!(
        observed_order(),
        vec![
            "p9_a".to_string(), // priority 9, lower id first
            "p9_b".to_string(), // priority 9, higher id second (FIFO)
            "p5".to_string(),
            "p1".to_string(),
            "p0".to_string(),
        ],
        "drain order = priority DESC, then scheduled_for/id ASC",
    );
}

// =========================================================================
// 4. None priority defaults to 0 and is persisted as Some(0)
// =========================================================================

/// `EnqueueOptions::priority = None` enqueues at normal priority. The row is
/// written with `Some(0)` (never NULL), so it sorts identically to an
/// explicit `Some(0)` and never jumps ahead of higher-priority work.
#[tokio::test(flavor = "multi_thread")]
async fn none_priority_persists_as_zero_not_null() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    reset_claim_order();
    register_recorder();

    let default_id = enqueue_record("defaulted", None).await;
    let row = fetch(default_id).await;
    assert_eq!(
        row.priority,
        Some(0),
        "enqueue must materialise None as Some(0), never NULL",
    );

    enqueue_record("urgent", Some(3)).await;

    drain_recording().await;

    assert_eq!(
        observed_order(),
        vec!["urgent".to_string(), "defaulted".to_string()],
        "a None (=> 0) task is claimed after an explicit priority-3 task",
    );
}

// =========================================================================
// 5. legacy NULL-priority row drains at/below the defaults
// =========================================================================

/// A row inserted directly with a NULL `priority` (simulating a legacy row
/// that predates the column) must NOT jump ahead of an explicit priority-1
/// task. On SQLite, `priority DESC` sorts NULLs LAST, so the NULL row drains
/// after every explicit priority. (On Postgres NULLs sort FIRST under DESC —
/// documented in `claim_one`: new rows are always written `Some`, so only
/// rare legacy rows are NULL there.)
#[tokio::test(flavor = "multi_thread")]
async fn legacy_null_priority_does_not_jump_ahead() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    reset_claim_order();
    register_recorder();

    let now = Utc::now();
    let pool = umbra::db::pool();
    // Insert a legacy row with NULL priority directly (the additive-migration
    // shape: the column exists but the row predates it).
    sqlx::query(
        "INSERT INTO task_row \
         (name, payload, status, attempts, max_attempts, scheduled_for, run_at, priority, created_at) \
         VALUES (?, ?, ?, 0, 3, ?, ?, NULL, ?)",
    )
    .bind("record")
    .bind(serde_json::json!({ "label": "legacy_null" }).to_string())
    .bind(STATUS_PENDING)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await
    .expect("insert legacy null-priority row");

    // An explicit priority-1 task enqueued AFTER the legacy row.
    enqueue_record("explicit_1", Some(1)).await;

    drain_recording().await;

    let order = observed_order();
    assert_eq!(
        order,
        vec!["explicit_1".to_string(), "legacy_null".to_string()],
        "explicit priority-1 must drain before the legacy NULL-priority row (NULL = lowest)",
    );
}

// =========================================================================
// 6. priority respects scheduled_for visibility (a high-priority future
//    task stays invisible until due)
// =========================================================================

/// A high-priority task scheduled for the future is NOT claimed ahead of a
/// due lower-priority task: visibility (`scheduled_for`) gates eligibility
/// before priority orders the eligible set.
#[tokio::test(flavor = "multi_thread")]
async fn future_high_priority_does_not_preempt_a_due_task() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    reset_claim_order();
    register_recorder();

    // A high-priority task that isn't eligible yet.
    let future = Utc::now() + ChronoDuration::seconds(3600);
    enqueue(
        "record",
        serde_json::json!({ "label": "future_urgent" }),
        EnqueueOptions {
            priority: Some(9),
            scheduled_for: Some(future),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue future");

    // A due normal-priority task.
    let due_id = enqueue_record("due_normal", Some(0)).await;

    // One worker step should claim the DUE task, not the future high-priority
    // one (which is still invisible to the claim query).
    let processed = run_worker_once().await.expect("worker step");
    assert!(processed, "the due task should have been claimed");
    assert_eq!(
        observed_order(),
        vec!["due_normal".to_string()],
        "a not-yet-due priority-9 task must not preempt a due priority-0 task",
    );

    let due_row = fetch(due_id).await;
    assert_eq!(due_row.status, STATUS_SUCCEEDED);

    // The queue should now report empty (the future task is still invisible).
    let again = run_worker_once().await.expect("worker step 2");
    assert!(!again, "the future task must stay invisible until its eta");

    // Wait for the duration to elapse would be slow; instead assert the
    // future row is still pending and was never claimed.
    assert!(
        !observed_order().contains(&"future_urgent".to_string()),
        "the future high-priority task must not have run",
    );
}
