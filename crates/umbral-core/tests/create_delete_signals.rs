//! `create()` and `delete()` are observable (gaps3 #29).
//!
//! They weren't. `save()` and `update_or_create()` both emitted a per-row
//! `post_save`, but `Manager::create()` emitted **nothing at all**, and
//! `QuerySet::delete()` emitted only the *bulk* signal — never the per-row
//! `post_delete` that `RealtimePlugin::on_model` subscribes to.
//!
//! The consequence was invisible and expensive: every `.create()` and `.delete()`
//! in app code was silently unobservable, so a live consumer hand-pushed a
//! realtime event after all 13 of its writes and wrote a comment explaining that
//! `.create()` "the on_model bridge doesn't observe". It was right, and the fix
//! belongs in the ORM, not in every handler.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "sig_note")]
pub struct SigNote {
    pub id: i64,
    pub body: String,
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        // File-backed, not `sqlite::memory:` — an in-memory SQLite gives EACH
        // pooled connection its own empty database, so the DDL below lands on one
        // connection and the tests' queries hit another ("no such table").
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("signals.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<SigNote>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

/// **The bug.** A `.create()` must fire `post_save:<table>` — otherwise realtime,
/// cache invalidation, and every other signal subscriber never learn the row
/// exists.
#[tokio::test]
async fn create_fires_a_per_row_post_save() {
    let _g = lock().lock().await;
    boot().await;
    umbral::signals::clear_for_tests();

    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = seen.clone();
    umbral::signals::subscribe("post_save:sig_note", move |payload| {
        sink.lock().unwrap().push(payload.to_string());
    });

    SigNote::objects()
        .create(SigNote {
            id: 0,
            body: "hello".into(),
        })
        .await
        .expect("create");

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        1,
        "`create()` must fire post_save — save() and update_or_create() do, and a \
         subscriber that misses creates is worse than no subscriber; got {got:?}",
    );
    assert!(
        got[0].contains("hello"),
        "the payload carries the row: {got:?}"
    );
}

/// A `.delete()` must fire the PER-ROW `post_delete`, not only the bulk signal —
/// `RealtimePlugin::on_model` subscribes per-row.
#[tokio::test]
async fn delete_fires_a_per_row_post_delete() {
    let _g = lock().lock().await;
    boot().await;
    umbral::signals::clear_for_tests();

    let row = SigNote::objects()
        .create(SigNote {
            id: 0,
            body: "doomed".into(),
        })
        .await
        .expect("create");

    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = seen.clone();
    umbral::signals::subscribe("post_delete:sig_note", move |payload| {
        sink.lock().unwrap().push(payload.to_string());
    });

    SigNote::objects()
        .filter(sig_note::ID.eq(row.id))
        .delete()
        .await
        .expect("delete");

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        1,
        "`delete()` must fire the per-row post_delete; got {got:?}",
    );
    assert!(
        got[0].contains(&row.id.to_string()),
        "a delete event carries the id — that's what it's FOR (invalidate that \
         row); got {got:?}",
    );
}

/// `update_or_create`'s CREATE branch must fire post_save **exactly once**.
///
/// It delegates to `create()`. Before gaps3 #29, `create()` was signal-free, so
/// gaps3 #14 patched the gap by emitting inside `update_or_create` — and once
/// `create()` started emitting, that patch became a DOUBLE emit (and, since
/// gaps3 #54, a double audit row). A subscriber seeing one write twice is its own
/// bug: it double-counts, double-invalidates, double-notifies.
#[tokio::test]
async fn update_or_create_fires_post_save_exactly_once_on_create() {
    let _g = lock().lock().await;
    boot().await;
    umbral::signals::clear_for_tests();

    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = seen.clone();
    umbral::signals::subscribe("post_save:sig_note", move |payload| {
        sink.lock().unwrap().push(payload.to_string());
    });

    SigNote::objects()
        .update_or_create(
            sig_note::BODY.eq("unique-uoc"),
            SigNote {
                id: 0,
                body: "unique-uoc".into(),
            },
        )
        .await
        .expect("update_or_create");

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        1,
        "exactly one post_save — create() emits, and update_or_create must not \
         emit again on top of it; got {got:?}",
    );
}
