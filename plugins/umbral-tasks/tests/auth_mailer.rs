//! Behavioral coverage for `umbral_tasks::auth_mailer` / `auth_mailer_with`
//! (gaps4 #82a): the task-backed `umbral_auth::AuthMailer` adapter.
//!
//! Compiled only with `--features auth-mailer` (this crate's optional dep
//! on umbral-auth); without it the file is an empty crate so
//! `cargo test -p umbral-tasks` (default features) stays green with no
//! reference to a feature that isn't enabled.

#![cfg(feature = "auth-mailer")]

use std::sync::{Arc, Mutex, OnceLock};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral_auth::{AuthMailError, AuthMailer, MailKind, OutgoingMail};
use umbral_tasks::{
    _clear_handlers_for_tests, SEND_AUTH_EMAIL_TASK, STATUS_FAILED, STATUS_PENDING,
    STATUS_SUCCEEDED, TaskRow, TasksPlugin, auth_mailer, auth_mailer_with, register_discovered,
    run_worker_once,
};

// =========================================================================
// Boot helpers — mirrors tests/integration.rs and tests/macro_integration.rs.
// =========================================================================

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auth_mailer.sqlite");
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

async fn fetch_status(id: i64) -> String {
    let pool = umbral::db::pool();
    let row: (String,) = sqlx::query_as("SELECT status FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch row");
    row.0
}

async fn fetch_payload(id: i64) -> String {
    let pool = umbral::db::pool();
    let row: (String,) = sqlx::query_as("SELECT payload FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch row");
    row.0
}

async fn drain_queue() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM task_row")
        .execute(&pool)
        .await
        .expect("drain");
}

static TEST_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

fn sample_mail(to: &str) -> OutgoingMail {
    OutgoingMail {
        to: to.to_string(),
        username: "alice".to_string(),
        kind: MailKind::PasswordReset {
            reset_url: "https://app.example/auth/reset?token=abc123".to_string(),
        },
        subject: "Reset your password".to_string(),
        html: "<p>reset</p>".to_string(),
        text: "reset: https://app.example/auth/reset?token=abc123".to_string(),
    }
}

/// A recording `AuthMailer` used as the delivery target for
/// `auth_mailer_with`, so a test can prove the queued task actually reached
/// a custom delivery mailer instead of the `ConsoleMailer` default.
#[derive(Default, Clone)]
struct Recorder(Arc<Mutex<Vec<OutgoingMail>>>);

#[async_trait::async_trait]
impl AuthMailer for Recorder {
    async fn send(&self, mail: OutgoingMail) -> Result<(), AuthMailError> {
        self.0.lock().unwrap().push(mail);
        Ok(())
    }
}

// =========================================================================
// 1. `.send()` enqueues instead of delivering inline: the row exists,
//    pending, BEFORE any worker runs.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn send_enqueues_a_pending_row_and_returns_before_delivery() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    register_discovered();

    let mailer = auth_mailer();
    let mail = sample_mail("carol@example.com");

    // `.send()` is the exact call `challenge.rs` makes inline today
    // (`active_mailer().send(mail).await`). With the task-backed mailer
    // this returns as soon as the row is written — no SMTP/console I/O has
    // happened yet.
    mailer.send(mail).await.expect("enqueue via AuthTaskMailer");

    let pool = umbral::db::pool();
    let row: TaskRow = sqlx::query_as("SELECT * FROM task_row WHERE name = ? ORDER BY id DESC")
        .bind(SEND_AUTH_EMAIL_TASK)
        .fetch_one(&pool)
        .await
        .expect("the send_auth_email task row was written");

    assert_eq!(
        row.status, STATUS_PENDING,
        "row should be queued, not yet processed"
    );
    // The serialized payload carries the real mail content — proof this is
    // the actual OutgoingMail, not a placeholder.
    let payload = fetch_payload(row.id).await;
    assert!(
        payload.contains("carol@example.com") && payload.contains("abc123"),
        "payload should carry the enqueued OutgoingMail; got {payload}"
    );
}

// =========================================================================
// 2. The default task body delivers through ConsoleMailer without panicking
//    when no `auth_mailer_with` delivery override was installed.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn worker_processes_the_queued_mail_and_marks_it_succeeded() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    register_discovered();

    let mailer = auth_mailer();
    mailer
        .send(sample_mail("dev@example.com"))
        .await
        .expect("enqueue");

    let pool = umbral::db::pool();
    let (id,): (i64,) = sqlx::query_as("SELECT id FROM task_row WHERE name = ? ORDER BY id DESC")
        .bind(SEND_AUTH_EMAIL_TASK)
        .fetch_one(&pool)
        .await
        .expect("row exists");

    let processed = run_worker_once().await.expect("worker step");
    assert!(
        processed,
        "worker should have claimed and run the send_auth_email task"
    );
    assert_eq!(fetch_status(id).await, STATUS_SUCCEEDED);
}

// =========================================================================
// 3. `auth_mailer_with(custom)`: a caller-supplied AuthMailer redirects
//    where the queued task actually delivers, with zero umbral-auth
//    changes — the "custom registered email task can redirect delivery"
//    case from the gap.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn auth_mailer_with_redirects_delivery_to_a_custom_mailer() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    register_discovered();

    let recorder = Recorder::default();
    let mailer = auth_mailer_with(recorder.clone());

    let mail = sample_mail("redirect@example.com");
    mailer.send(mail).await.expect("enqueue");

    let processed = run_worker_once().await.expect("worker step");
    assert!(processed);

    let captured = recorder.0.lock().unwrap();
    assert_eq!(
        captured.len(),
        1,
        "the custom delivery mailer should have received the mail"
    );
    assert_eq!(captured[0].to, "redirect@example.com");
    assert!(
        matches!(&captured[0].kind, MailKind::PasswordReset { reset_url } if reset_url.contains("abc123"))
    );
}

// =========================================================================
// 4. A malformed payload fails the same way any other task's bad payload
//    does — no special-casing for the auth-mailer task.
// =========================================================================

#[tokio::test(flavor = "multi_thread")]
async fn bad_payload_marks_the_row_failed_with_a_deserialise_error() {
    let _guard = test_lock().await;
    boot().await;
    drain_queue().await;
    _clear_handlers_for_tests();
    register_discovered();

    let id = umbral_tasks::enqueue(
        SEND_AUTH_EMAIL_TASK,
        serde_json::json!({"not": "an outgoing mail"}),
        umbral_tasks::EnqueueOptions {
            max_attempts: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue");

    let processed = run_worker_once().await.expect("worker step");
    assert!(processed);
    assert_eq!(fetch_status(id).await, STATUS_FAILED);

    let pool = umbral::db::pool();
    let (error,): (Option<String>,) = sqlx::query_as("SELECT error FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("fetch row");
    assert!(
        error
            .unwrap_or_default()
            .contains("payload deserialise error"),
        "should report a payload deserialise error, matching every other task"
    );
}
