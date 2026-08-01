//! gaps4 #40: `#[umbral::task]` handlers are DISCOVERED — nobody calls the
//! generated `register_<fn>()` companion anywhere in this file.
//!
//! Before this, the companion had to be called by hand at boot, and a
//! forgotten call was invisible until production: the enqueued row failed
//! with `HandlerNotFound`. Now the attribute also submits a
//! `TaskRegistration` to the link-time inventory slice, and
//! `register_discovered()` (run from `TasksPlugin::on_ready` and from the
//! `tasks-worker` / `tasks-beat` commands) installs every linked handler.
//!
//! Own test binary: discovery-on is the default under test here, and the
//! sibling suites exercise the manual-registration path with cleared
//! registries — the two must not share a process.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

use umbral_tasks::{EnqueueOptions, STATUS_SUCCEEDED, enqueue, run_worker_once};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auto_discovery.sqlite");
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

        // `build()` (not build_deferred) fires on_ready — the discovery
        // moment under test. Note: NO register_* call anywhere in this file.
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(umbral_tasks::TasksPlugin::default())
            .build()
            .expect("App::build with TasksPlugin");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

#[derive(serde::Serialize, serde::Deserialize)]
struct NudgePayload {
    who: String,
}

static RAN: OnceLock<AtomicBool> = OnceLock::new();
fn ran() -> &'static AtomicBool {
    RAN.get_or_init(|| AtomicBool::new(false))
}

#[umbral::task]
async fn discovered_nudge(payload: NudgePayload) -> Result<(), String> {
    let _ = payload.who;
    ran().store(true, Ordering::SeqCst);
    Ok(())
}

/// The headline: enqueue → worker → succeeded, with the handler installed
/// purely by discovery at `on_ready`.
#[tokio::test(flavor = "multi_thread")]
async fn a_task_runs_without_any_manual_registration() {
    let _guard = test_lock().await;
    boot().await;

    let id = enqueue(
        "discovered_nudge",
        serde_json::json!({ "who": "auto" }),
        EnqueueOptions::default(),
    )
    .await
    .expect("enqueue");

    let processed = run_worker_once().await.expect("worker step");
    assert!(processed, "worker should have claimed the row");
    assert!(
        ran().load(Ordering::SeqCst),
        "the discovered handler must actually have run"
    );

    let pool = umbral::db::pool();
    let (status,): (String,) = sqlx::query_as("SELECT status FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch row");
    assert_eq!(
        status, STATUS_SUCCEEDED,
        "a #[task] handler serves the queue with zero manual registration"
    );
}

/// `register_discovered` reports the linked handlers and is idempotent —
/// calling it again neither errors nor double-counts.
#[tokio::test(flavor = "multi_thread")]
async fn register_discovered_is_idempotent_and_counts_distinct_names() {
    let _guard = test_lock().await;
    boot().await;

    let first = umbral_tasks::register_discovered();
    let second = umbral_tasks::register_discovered();
    assert!(
        first >= 1,
        "at least this binary's #[task] must be discovered; got {first}"
    );
    assert_eq!(first, second, "re-running discovery finds the same set");
}
