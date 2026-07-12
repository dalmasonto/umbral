//! Type-safe enqueue — `#[task]` generates a typed handle (gaps3 #48).
//!
//! Before this, enqueueing named the task with a bare string:
//!
//! ```ignore
//! enqueue("send_welcom", payload, opts).await?;   // typo — compiles fine
//! ```
//!
//! and the typo surfaced at *runtime*, as a `HandlerNotFound` that marks the row
//! `failed` — in production, on the worker, long after the deploy. A rename of the
//! handler had exactly the same shape, which is worse: it's silent and nothing in
//! CI notices.
//!
//! Now `#[task] async fn send_welcome(p: Welcome)` also generates `SendWelcome`,
//! and you enqueue with `SendWelcome::enqueue(payload)`. A typo or a rename is a
//! compile error, and the payload type can't drift from what the handler
//! deserialises — both sides name the same type.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use umbral_tasks::{EnqueueOptions, Task, TasksPlugin};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    pub user_id: i64,
}

#[umbral::task]
async fn send_welcome(payload: Welcome) -> Result<(), String> {
    let _ = payload.user_id;
    Ok(())
}

/// A second task sharing NOTHING with the first — proves the generated handle is
/// per-task, and that two tasks can coexist (a trait impl on the *payload* type
/// would have collided here if they shared one).
#[umbral::task]
async fn send_reminder(payload: Welcome) -> Result<(), String> {
    let _ = payload.user_id;
    Ok(())
}

async fn boot() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(TasksPlugin::default())
            .build()
            .expect("App::build");
        let pool = umbral::db::pool();
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
        .expect("ddl");
    })
    .await;
}

async fn queued_name(id: i64) -> String {
    let pool = umbral::db::pool();
    let row = sqlx::query_as::<_, (String,)>("SELECT name FROM task_row WHERE id = ?")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("row");
    row.0
}

/// The handle carries the task's registered name — so the enqueue site never
/// spells it, and a rename of the function moves the name with it.
#[test]
fn the_handle_carries_the_registered_name() {
    assert_eq!(SendWelcome::NAME, "send_welcome");
    assert_eq!(SendReminder::NAME, "send_reminder");
}

/// `SendWelcome::enqueue(payload)` queues a row under the right handler name —
/// with no string at the call site.
#[tokio::test]
async fn typed_enqueue_queues_under_the_right_handler() {
    boot().await;
    let id = SendWelcome::enqueue(Welcome { user_id: 7 })
        .await
        .expect("enqueue");
    assert_eq!(
        queued_name(id).await,
        "send_welcome",
        "the handle's NAME is what the worker looks up",
    );
}

/// The options form still works, so the typed handle isn't a downgrade.
#[tokio::test]
async fn typed_enqueue_with_options() {
    boot().await;
    let id = SendWelcome::enqueue_with(
        Welcome { user_id: 9 },
        EnqueueOptions {
            max_attempts: Some(5),
            ..Default::default()
        },
    )
    .await
    .expect("enqueue_with");
    assert_eq!(queued_name(id).await, "send_welcome");
}

/// `enqueue_task::<T>` — the same thing when you're generic over the task.
#[tokio::test]
async fn enqueue_task_by_type_parameter() {
    boot().await;
    let id = umbral_tasks::enqueue_task::<SendReminder>(
        Welcome { user_id: 1 },
        EnqueueOptions::default(),
    )
    .await
    .expect("enqueue_task");
    assert_eq!(queued_name(id).await, "send_reminder");
}

/// The string API still works — this is additive, not a migration.
#[tokio::test]
async fn the_string_api_still_works() {
    boot().await;
    let id = umbral_tasks::enqueue(
        "send_welcome",
        Welcome { user_id: 2 },
        EnqueueOptions::default(),
    )
    .await
    .expect("string enqueue");
    assert_eq!(queued_name(id).await, "send_welcome");
}
