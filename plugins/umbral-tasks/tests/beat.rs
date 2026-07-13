//! Periodic / cron "beat" coverage for umbral-tasks: the [`Schedule`]
//! next-fire computation, `run_beat_once` firing a due `PeriodicTask`
//! (enqueuing exactly one `TaskRow` and advancing `next_run` + stamping
//! `last_run`), the optimistic-claim double-enqueue guard (two beat
//! instances racing the same due row produce exactly one enqueue), and the
//! registration upsert (a `PeriodicSpec` syncs to a row, and re-syncing
//! with a changed schedule updates without duplicating).
//!
//! Same boot shape as `reliability.rs`: one OnceCell-backed tempfile sqlite
//! pool, registered TasksPlugin, raw SQL CREATE TABLE for BOTH `task_row`
//! and `periodic_task` because the integration test owns its own schema
//! without standing up the M5 migration loop.

use std::sync::OnceLock;
use std::time::Duration;

use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbral_tasks::{
    PeriodicSpec, PeriodicTask, Schedule, TasksPlugin, fire_due_periodic, run_beat_once,
    sync_periodic_specs,
};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tasks_beat.sqlite");
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

async fn fetch_periodic(id: i64) -> PeriodicTask {
    let pool = umbral::db::pool();
    sqlx::query_as::<_, PeriodicTask>("SELECT * FROM periodic_task WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch periodic row")
}

async fn count_task_rows(name: &str) -> i64 {
    let pool = umbral::db::pool();
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM task_row WHERE name = ?")
        .bind(name)
        .fetch_one(&pool)
        .await
        .expect("count task rows")
}

async fn count_periodic(name: &str) -> i64 {
    let pool = umbral::db::pool();
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM periodic_task WHERE name = ?")
        .bind(name)
        .fetch_one(&pool)
        .await
        .expect("count periodic rows")
}

/// Insert a periodic row directly so a test can place `next_run` in the
/// past/future deterministically. Returns the new row id.
async fn insert_periodic(
    name: &str,
    task: &str,
    schedule: &Schedule,
    next_run: chrono::DateTime<Utc>,
    enabled: bool,
) -> i64 {
    let pool = umbral::db::pool();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO periodic_task \
         (name, task, payload, schedule, next_run, last_run, enabled, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, NULL, ?, ?, ?)",
    )
    .bind(name)
    .bind(task)
    .bind("{}")
    .bind(schedule.to_storage())
    .bind(next_run.to_rfc3339())
    .bind(enabled)
    .bind(now.to_rfc3339())
    .bind(now.to_rfc3339())
    .execute(&pool)
    .await
    .expect("insert periodic row");
    sqlx::query_scalar::<_, i64>("SELECT id FROM periodic_task WHERE name = ?")
        .bind(name)
        .fetch_one(&pool)
        .await
        .expect("fetch inserted id")
}

async fn drain() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM task_row")
        .execute(&pool)
        .await
        .expect("drain task_row");
    sqlx::query("DELETE FROM periodic_task")
        .execute(&pool)
        .await
        .expect("drain periodic_task");
}

static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

// =========================================================================
// 1. Schedule::next_after — cron and interval both compute a sane future time.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn schedule_next_after_cron_and_interval() {
    // "every minute" as a standard 5-field cron.
    let cron = Schedule::cron("* * * * *");
    let now = Utc::now();
    let next = cron.next_after(now).expect("cron yields a next time");
    assert!(
        next > now,
        "cron next must be after `after`: {next} > {now}"
    );
    // Within the next minute (the next minute boundary at most ~60s out).
    assert!(
        (next - now).num_seconds() <= 61,
        "every-minute cron should fire within ~60s, got {}s",
        (next - now).num_seconds()
    );

    // A specific 5-field cron: midnight daily.
    let midnight = Schedule::cron("0 0 * * *");
    let next = midnight.next_after(now).expect("daily cron yields a time");
    assert!(next > now);

    // Fixed interval.
    let every = Schedule::every(Duration::from_secs(60));
    let next = every.next_after(now).expect("interval always has a next");
    let delta = (next - now).num_seconds();
    assert_eq!(delta, 60, "Every(60s) next = after + 60s, got {delta}s");
}

#[tokio::test(flavor = "multi_thread")]
async fn schedule_round_trips_through_storage() {
    let cron = Schedule::cron("0 0 * * *");
    assert_eq!(cron.to_storage(), "cron:0 0 * * *");
    assert_eq!(Schedule::from_storage("cron:0 0 * * *"), Some(cron));

    let every = Schedule::every(Duration::from_secs(3600));
    assert_eq!(every.to_storage(), "every:3600");
    assert_eq!(Schedule::from_storage("every:3600"), Some(every));

    assert_eq!(Schedule::from_storage("garbage"), None);
}

// =========================================================================
// 2. run_beat_once fires a due task; a future task is left alone.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn due_periodic_fires_once_and_advances_next_run() {
    let _guard = test_lock().await;
    boot().await;
    drain().await;

    let schedule = Schedule::cron("* * * * *");
    let past = Utc::now() - chrono::Duration::hours(1);
    let id = insert_periodic("due_job", "do_work", &schedule, past, true).await;

    let before = Utc::now();
    let fired = run_beat_once().await.expect("beat tick");
    assert_eq!(fired, 1, "exactly one due task should fire");

    // Exactly one TaskRow enqueued for the underlying task name.
    assert_eq!(
        count_task_rows("do_work").await,
        1,
        "beat must enqueue exactly one TaskRow for the fired task"
    );

    // next_run advanced into the future, last_run stamped.
    let row = fetch_periodic(id).await;
    assert!(
        row.next_run > before,
        "next_run must advance into the future: {} > {before}",
        row.next_run
    );
    let last_run = row.last_run.expect("last_run stamped on fire");
    assert!(last_run >= before, "last_run should be ~now");
    assert!(row.enabled, "a firing task stays enabled");
}

#[tokio::test(flavor = "multi_thread")]
async fn future_periodic_does_not_fire() {
    let _guard = test_lock().await;
    boot().await;
    drain().await;

    let schedule = Schedule::cron("* * * * *");
    let future = Utc::now() + chrono::Duration::hours(1);
    insert_periodic("future_job", "later_work", &schedule, future, true).await;

    let fired = run_beat_once().await.expect("beat tick");
    assert_eq!(fired, 0, "a not-yet-due task must not fire");
    assert_eq!(
        count_task_rows("later_work").await,
        0,
        "no TaskRow should be enqueued for a future periodic task"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_periodic_does_not_fire() {
    let _guard = test_lock().await;
    boot().await;
    drain().await;

    let schedule = Schedule::cron("* * * * *");
    let past = Utc::now() - chrono::Duration::hours(1);
    insert_periodic("off_job", "off_work", &schedule, past, false).await;

    let fired = run_beat_once().await.expect("beat tick");
    assert_eq!(fired, 0, "a disabled task must not fire even when due");
    assert_eq!(count_task_rows("off_work").await, 0);
}

// =========================================================================
// 3. Atomic claim — two racing claims produce exactly one enqueue.
// =========================================================================

/// Calling the claim path twice for the same due row results in exactly ONE
/// enqueue: the first advances `next_run` into the future, so the second
/// sees nothing due. Proves the optimistic `next_run`-guarded UPDATE wins
/// the race and gates the enqueue.
#[tokio::test(flavor = "multi_thread")]
async fn double_fire_enqueues_exactly_once() {
    let _guard = test_lock().await;
    boot().await;
    drain().await;

    let schedule = Schedule::cron("* * * * *");
    let past = Utc::now() - chrono::Duration::hours(1);
    insert_periodic("race_job", "race_work", &schedule, past, true).await;

    // First fire claims and enqueues.
    let fired_1 = fire_due_periodic().await.expect("fire 1");
    assert_eq!(fired_1, 1, "first claim wins and enqueues");

    // Second fire (same tick, simulating a concurrent beat instance) finds
    // next_run already advanced — nothing due, nothing enqueued.
    let fired_2 = fire_due_periodic().await.expect("fire 2");
    assert_eq!(
        fired_2, 0,
        "second claim must see the advanced next_run and enqueue nothing"
    );

    assert_eq!(
        count_task_rows("race_work").await,
        1,
        "exactly ONE TaskRow despite two claim passes (no double-enqueue)"
    );
}

// =========================================================================
// 4. Registration sync — upsert, then update on a changed schedule.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn registration_upserts_and_updates_without_duplicating() {
    let _guard = test_lock().await;
    boot().await;
    drain().await;

    let spec = PeriodicSpec {
        name: "reg_job".to_string(),
        schedule: Schedule::every(Duration::from_secs(3600)),
        task: "reg_work".to_string(),
        payload: "{}".to_string(),
    };

    // First sync inserts a row with a computed next_run.
    let changed = sync_periodic_specs(std::slice::from_ref(&spec))
        .await
        .expect("first sync");
    assert_eq!(changed, 1, "first sync inserts one row");
    assert_eq!(count_periodic("reg_job").await, 1);

    let pool = umbral::db::pool();
    let row = sqlx::query_as::<_, PeriodicTask>("SELECT * FROM periodic_task WHERE name = ?")
        .bind("reg_job")
        .fetch_one(&pool)
        .await
        .expect("fetch");
    let first_next_run = row.next_run;
    assert!(
        first_next_run > Utc::now(),
        "interval schedule computes a future next_run"
    );

    // Re-sync the SAME spec: updates in place, no duplicate, next_run kept
    // (schedule unchanged).
    let changed = sync_periodic_specs(std::slice::from_ref(&spec))
        .await
        .expect("idempotent re-sync");
    assert_eq!(changed, 1, "re-sync updates the existing row");
    assert_eq!(
        count_periodic("reg_job").await,
        1,
        "re-sync must NOT duplicate the row"
    );
    let row = sqlx::query_as::<_, PeriodicTask>("SELECT * FROM periodic_task WHERE name = ?")
        .bind("reg_job")
        .fetch_one(&pool)
        .await
        .expect("fetch");
    assert_eq!(
        row.next_run, first_next_run,
        "an unchanged schedule must NOT shove next_run forward"
    );

    // Re-sync with a CHANGED schedule: row updated, next_run recomputed.
    let changed_spec = PeriodicSpec {
        schedule: Schedule::every(Duration::from_secs(7200)),
        ..spec.clone()
    };
    let changed = sync_periodic_specs(std::slice::from_ref(&changed_spec))
        .await
        .expect("changed sync");
    assert_eq!(changed, 1);
    assert_eq!(count_periodic("reg_job").await, 1, "still one row");
    let row = sqlx::query_as::<_, PeriodicTask>("SELECT * FROM periodic_task WHERE name = ?")
        .bind("reg_job")
        .fetch_one(&pool)
        .await
        .expect("fetch");
    assert_eq!(row.schedule, "every:7200", "schedule string updated");
    assert!(
        row.next_run != first_next_run,
        "a changed schedule recomputes next_run"
    );
}
