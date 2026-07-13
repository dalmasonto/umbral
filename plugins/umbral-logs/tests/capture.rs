//! End-to-end coverage for umbral-logs.
//!
//! Boots a tiny test app (LogsPlugin) over a tempfile-backed sqlite pool —
//! same pattern as the umbral-sessions integration tests — drives synthetic
//! requests through the capture layer via `tower::ServiceExt::oneshot`, then
//! `flush()`es the in-flight capture tasks and reads the `logs_requestlog`
//! rows straight back through the ORM.
//!
//! The table is created directly here (the allowed test-only DDL exception:
//! tests bypass `make` / `migrate`).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral::prelude::*;
use umbral_logs::{LogsPlugin, RequestLog, flush, request_log};

static BOOT: OnceCell<()> = OnceCell::const_new();

/// Serialises the DB/`PENDING`-touching tests. `flush()` drains the
/// process-global in-flight capture buffer, so two tests flushing in parallel
/// can each drain the other's spawned insert before it's registered — a
/// #30-family race that flakes under full-workspace load. Every test that
/// `send()`s + `flush()`es holds this for the duration.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Boot the app + DB exactly once for the whole test binary. The capture
/// layer reads its config ambiently, so all tests in this file share the
/// default config (`sample_rate = 1.0`, `min_status = 0`, default exclusions).
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("logs_capture.sqlite");
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
            .plugin(LogsPlugin::default())
            .build()
            .expect("App::build with LogsPlugin");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // Create the logs_requestlog table (test-only DDL; tests bypass migrate).
    })
    .await;
}

/// Build a router with a couple of routes wrapped by the capture layer, the
/// same way `LogsPlugin::wrap_router` mounts it in a real app.
fn app() -> Router {
    let plugin = LogsPlugin::default();
    let router = Router::new()
        .route("/hello", get(|| async { "hi" }))
        .route(
            "/boom",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
        )
        .route("/health", get(|| async { "ok" }));
    plugin.wrap_router(router)
}

async fn send(app: Router, method: &str, path: &str) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    app.oneshot(req).await.unwrap().status()
}

async fn count_path(path: &str) -> i64 {
    RequestLog::objects()
        .filter(request_log::PATH.eq(path))
        .count()
        .await
        .expect("count logs_requestlog")
}

/// (a) The capture layer records a row with the right method/path/status.
#[tokio::test]
async fn records_request_with_method_path_status() {
    let _guard = TEST_LOCK.lock().await;
    boot().await;
    let path = "/hello";

    let before = count_path(path).await;
    let status = send(app(), "GET", path).await;
    assert_eq!(status, StatusCode::OK);

    // Fire-and-forget capture is async — await the spawned insert.
    flush().await;

    let after = count_path(path).await;
    assert_eq!(after, before + 1, "exactly one row recorded for {path}");

    let row: RequestLog = RequestLog::objects()
        .filter(request_log::PATH.eq(path))
        .order_by(request_log::ID.desc())
        .first()
        .await
        .expect("query")
        .expect("a row exists");
    assert_eq!(row.method, "GET");
    assert_eq!(row.path, path);
    assert_eq!(row.status, 200);
    assert!(row.duration_ms >= 0);
    // user_id is best-effort and None for a request with no identity header.
    assert!(row.user_id.is_none());
}

/// (b) An excluded prefix (the default `/health`) is NOT logged.
#[tokio::test]
async fn excluded_prefix_is_not_logged() {
    let _guard = TEST_LOCK.lock().await;
    boot().await;
    let path = "/health";

    let before = count_path(path).await;
    let status = send(app(), "GET", path).await;
    assert_eq!(status, StatusCode::OK);

    flush().await;

    let after = count_path(path).await;
    assert_eq!(after, before, "excluded /health must not produce a row");
}

/// (c1) `min_status` drops requests below the floor, keeps those at/above it.
///
/// The capture layer reads a process-global config sealed once at boot, so a
/// second router with a different `min_status` can't be exercised in the same
/// process. Instead assert the exact predicate the layer applies
/// (`LogsConfig::should_capture`) with an explicit config — the layer routes
/// every request through this same function.
#[test]
fn min_status_drops_low_status() {
    let cfg = LogsPlugin::default().min_status(500).resolved_config();
    // sample_rate defaults to 1.0, so sampling never interferes here.
    assert!(
        !cfg.should_capture("/ok", 200, 0),
        "200 below floor=500 dropped"
    );
    assert!(
        !cfg.should_capture("/redir", 302, 1),
        "302 below floor=500 dropped"
    );
    assert!(cfg.should_capture("/err", 500, 2), "500 at floor=500 kept");
    assert!(
        cfg.should_capture("/err", 503, 3),
        "503 above floor=500 kept"
    );
}

/// (c2) The deterministic sampler keeps exactly the expected cadence through
/// the real `should_capture` predicate, and the builder clamps out-of-range
/// rates.
#[test]
fn sample_rate_is_deterministic() {
    let cfg = LogsPlugin::default().sample_rate(0.25).resolved_config();
    // Status 200 with min_status default 0 and a non-excluded path, so the
    // only gate is sampling: keep 1-in-4 on a fixed cadence (0, 4, 8, …).
    let kept: Vec<u64> = (0..12)
        .filter(|&seq| cfg.should_capture("/x", 200, seq))
        .collect();
    assert_eq!(
        kept,
        vec![0, 4, 8],
        "rate 0.25 keeps 1-in-4 deterministically"
    );

    // Over-range clamps to 1.0 — everything logged.
    let full = LogsPlugin::default().sample_rate(2.0).resolved_config();
    assert!(
        (0..8).all(|seq| full.should_capture("/x", 200, seq)),
        "clamped rate 1.0 keeps all"
    );

    // Negative clamps to 0.0 — nothing logged.
    let none = LogsPlugin::default().sample_rate(-1.0).resolved_config();
    assert!(
        (0..8).all(|seq| !none.should_capture("/x", 200, seq)),
        "clamped rate 0.0 drops all"
    );
}

/// The default exclusions are honoured by `should_capture` too (the /health
/// case is also covered end-to-end above).
#[test]
fn default_exclusions_drop_static_and_health() {
    let cfg = LogsPlugin::default().resolved_config();
    assert!(!cfg.should_capture("/health", 200, 0));
    assert!(!cfg.should_capture("/static/app.css", 200, 1));
    assert!(!cfg.should_capture("/admin/static/x.js", 200, 2));
    assert!(!cfg.should_capture("/favicon.ico", 200, 3));
    assert!(
        cfg.should_capture("/api/things", 200, 4),
        "non-excluded path kept"
    );
}

/// (d) `RequestLog` round-trips through the ORM: create then read back.
#[tokio::test]
async fn request_log_round_trips_through_orm() {
    let _guard = TEST_LOCK.lock().await;
    boot().await;

    let created = RequestLog::objects()
        .create(RequestLog {
            id: 0,
            method: "DELETE".to_string(),
            path: "/round-trip".to_string(),
            status: 204,
            duration_ms: 7,
            user_id: Some("42".to_string()),
            ip: Some("203.0.113.7".to_string()),
            user_agent: Some("umbral-test/1.0".to_string()),
            created_at: Utc::now(),
        })
        .await
        .expect("create RequestLog");
    assert!(created.id > 0, "DB assigned a primary key");

    let fetched: RequestLog = RequestLog::objects()
        .filter(request_log::ID.eq(created.id))
        .first()
        .await
        .expect("query")
        .expect("row exists");

    assert_eq!(fetched.method, "DELETE");
    assert_eq!(fetched.path, "/round-trip");
    assert_eq!(fetched.status, 204);
    assert_eq!(fetched.duration_ms, 7);
    assert_eq!(fetched.user_id, Some("42".to_string()));
    assert_eq!(fetched.ip.as_deref(), Some("203.0.113.7"));
    assert_eq!(fetched.user_agent.as_deref(), Some("umbral-test/1.0"));
}
