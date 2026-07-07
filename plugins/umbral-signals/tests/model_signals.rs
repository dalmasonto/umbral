//! Integration tests for the typed per-model signal API.
//!
//! Validates:
//! - `post_save` fires on `Manager::save` when `created = true` (INSERT).
//! - `pre_save` fires before INSERT.
//! - `post_save` fires with `created = false` on UPDATE (existing PK).
//! - `pre_delete` + `post_delete` fire from `Manager::delete_instance`.
//! - `save` with new PK triggers create shape (`created = true`).
//! - `save` with existing PK triggers update shape (`created = false`).
//! - Bulk `update_values` does NOT fire any signal.
//! - Bulk `QuerySet::delete()` does NOT fire any signal.
//!
//! The signals registry and the App pool are both process-wide globals.
//! All tests in this binary are serialised on `TEST_LOCK` to avoid
//! interference between tests.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex as TokioMutex, OnceCell};
use umbral_signals::{clear_for_tests, on_model};

// ---------------------------------------------------------------------------
// Test model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "sig_post")]
pub struct SigPost {
    pub id: i64,
    pub title: String,
}

// ---------------------------------------------------------------------------
// Boot fixture
// ---------------------------------------------------------------------------

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("model_signals.sqlite");
        std::mem::forget(tmp); // keep the dir alive for the process lifetime
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let _app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<SigPost>()
            .build()
            .expect("App::build");

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sig_post (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL\
             )",
        )
        .execute(&umbral::db::pool())
        .await
        .expect("create sig_post table");
    })
    .await;
}

// ---------------------------------------------------------------------------
// Test serialisation
// ---------------------------------------------------------------------------

fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

use std::sync::OnceLock;

/// Truncate the sig_post table and clear all signal handlers between tests.
async fn reset() {
    boot().await;
    sqlx::query("DELETE FROM sig_post")
        .execute(&umbral::db::pool())
        .await
        .expect("truncate sig_post");
    clear_for_tests();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_save_fires_on_create_with_created_true() {
    let _guard = test_lock().lock().await;
    reset().await;

    let fired = Arc::new(AtomicUsize::new(0));
    let last_created = Arc::new(AtomicUsize::new(99)); // sentinel
    {
        let f = fired.clone();
        let lc = last_created.clone();
        on_model::<SigPost>().post_save(move |_post, created| {
            let f = f.clone();
            let lc = lc.clone();
            async move {
                f.fetch_add(1, Ordering::SeqCst);
                lc.store(if created { 1 } else { 0 }, Ordering::SeqCst);
            }
        });
    }

    let new_post = SigPost {
        id: 0,
        title: "hello".into(),
    };
    let _saved = SigPost::objects().save(new_post).await.expect("save");

    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "post_save should have fired once"
    );
    assert_eq!(
        last_created.load(Ordering::SeqCst),
        1,
        "created should be true on INSERT"
    );
}

#[tokio::test]
async fn pre_save_fires_before_insert() {
    let _guard = test_lock().lock().await;
    reset().await;

    // Use an AtomicUsize as a sequencer: pre_save sets 1, post_save sets 2.
    // This proves pre fires before post (and before the INSERT).
    let seq = Arc::new(AtomicUsize::new(0));
    {
        let s = seq.clone();
        on_model::<SigPost>().pre_save(move |_post, _created| {
            let s = s.clone();
            async move {
                s.store(1, Ordering::SeqCst);
            }
        });
    }
    {
        let s = seq.clone();
        on_model::<SigPost>().post_save(move |_post, _created| {
            let s = s.clone();
            async move {
                // By the time post_save fires, pre_save should have run.
                assert_eq!(
                    s.load(Ordering::SeqCst),
                    1,
                    "pre_save must fire before post_save"
                );
                s.store(2, Ordering::SeqCst);
            }
        });
    }

    SigPost::objects()
        .save(SigPost {
            id: 0,
            title: "seq test".into(),
        })
        .await
        .expect("save");

    assert_eq!(
        seq.load(Ordering::SeqCst),
        2,
        "both signals should have fired"
    );
}

#[tokio::test]
async fn post_save_fires_with_created_false_on_update() {
    let _guard = test_lock().lock().await;
    reset().await;

    // Insert the row first without any signal handler.
    let inserted = SigPost::objects()
        .save(SigPost {
            id: 0,
            title: "original".into(),
        })
        .await
        .expect("insert");
    let pk = inserted.id;

    // Register the signal handler AFTER the first insert so it doesn't
    // count the creation.
    clear_for_tests();

    let created_flag = Arc::new(AtomicUsize::new(99));
    {
        let cf = created_flag.clone();
        on_model::<SigPost>().post_save(move |_post, created| {
            let cf = cf.clone();
            async move {
                cf.store(if created { 1 } else { 0 }, Ordering::SeqCst);
            }
        });
    }

    // Now save with the existing PK → UPDATE path.
    SigPost::objects()
        .save(SigPost {
            id: pk,
            title: "updated".into(),
        })
        .await
        .expect("update");

    assert_eq!(
        created_flag.load(Ordering::SeqCst),
        0,
        "created should be false on UPDATE"
    );
}

#[tokio::test]
async fn pre_delete_and_post_delete_fire_from_delete_instance() {
    let _guard = test_lock().lock().await;
    reset().await;

    // Insert a row.
    let post = SigPost::objects()
        .save(SigPost {
            id: 0,
            title: "to delete".into(),
        })
        .await
        .expect("insert");

    let pre_count = Arc::new(AtomicUsize::new(0));
    let post_count = Arc::new(AtomicUsize::new(0));
    {
        let pc = pre_count.clone();
        on_model::<SigPost>().pre_delete(move |_post| {
            let pc = pc.clone();
            async move {
                pc.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
    {
        let pc = post_count.clone();
        on_model::<SigPost>().post_delete(move |_post| {
            let pc = pc.clone();
            async move {
                pc.fetch_add(1, Ordering::SeqCst);
            }
        });
    }

    let affected = SigPost::objects()
        .delete_instance(&post)
        .await
        .expect("delete_instance");

    assert_eq!(affected, 1, "should have deleted one row");
    assert_eq!(
        pre_count.load(Ordering::SeqCst),
        1,
        "pre_delete should fire once"
    );
    assert_eq!(
        post_count.load(Ordering::SeqCst),
        1,
        "post_delete should fire once"
    );
}

#[tokio::test]
async fn bulk_update_values_does_not_fire_any_signal() {
    let _guard = test_lock().lock().await;
    reset().await;

    // Insert two rows.
    SigPost::objects()
        .save(SigPost {
            id: 0,
            title: "a".into(),
        })
        .await
        .expect("insert a");
    SigPost::objects()
        .save(SigPost {
            id: 0,
            title: "b".into(),
        })
        .await
        .expect("insert b");
    clear_for_tests();

    let signal_count = Arc::new(AtomicUsize::new(0));
    {
        let sc = signal_count.clone();
        on_model::<SigPost>().pre_save(move |_, _| {
            let sc = sc.clone();
            async move {
                sc.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
    {
        let sc = signal_count.clone();
        on_model::<SigPost>().post_save(move |_, _| {
            let sc = sc.clone();
            async move {
                sc.fetch_add(1, Ordering::SeqCst);
            }
        });
    }

    // Bulk UPDATE — must NOT fire signals.
    // `update_values` lives on QuerySet. Use `.on(pool)` to get an
    // unfiltered QuerySet (updates all rows).
    let mut map = serde_json::Map::new();
    map.insert("title".into(), json!("bulk-updated"));
    let pool = umbral::db::pool();
    SigPost::objects()
        .on(&pool)
        .update_values(map)
        .await
        .expect("bulk update_values");

    assert_eq!(
        signal_count.load(Ordering::SeqCst),
        0,
        "bulk update_values must not fire save signals"
    );
}

#[tokio::test]
async fn bulk_queryset_delete_does_not_fire_any_signal() {
    let _guard = test_lock().lock().await;
    reset().await;

    SigPost::objects()
        .save(SigPost {
            id: 0,
            title: "x".into(),
        })
        .await
        .expect("insert x");
    SigPost::objects()
        .save(SigPost {
            id: 0,
            title: "y".into(),
        })
        .await
        .expect("insert y");
    clear_for_tests();

    let signal_count = Arc::new(AtomicUsize::new(0));
    {
        let sc = signal_count.clone();
        on_model::<SigPost>().pre_delete(move |_| {
            let sc = sc.clone();
            async move {
                sc.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
    {
        let sc = signal_count.clone();
        on_model::<SigPost>().post_delete(move |_| {
            let sc = sc.clone();
            async move {
                sc.fetch_add(1, Ordering::SeqCst);
            }
        });
    }

    // Bulk DELETE — must NOT fire signals.
    // `delete()` lives on QuerySet. Use `.on(pool)` for an unfiltered delete.
    let pool = umbral::db::pool();
    SigPost::objects()
        .on(&pool)
        .delete()
        .await
        .expect("bulk delete");

    assert_eq!(
        signal_count.load(Ordering::SeqCst),
        0,
        "bulk QuerySet::delete must not fire delete signals"
    );
}
