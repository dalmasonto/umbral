//! Security/correctness coverage for the signals registry (gaps2 #85):
//!
//! 1. **async-panic isolation** — a `subscribe_async` handler that panics
//!    must NOT propagate out of `emit()` into the ORM write that fired it.
//!    `emit` completes, returns the full subscriber count, and a second
//!    subscriber on the same signal still fires (the c186e71 `catch_unwind`
//!    fix). The sync path's isolation is exercised too.
//! 2. **bulk signals** — `bulk_create([...])` fires `bulk_post_save`; a
//!    bulk `QuerySet::delete()` fires `bulk_post_delete` with the affected
//!    ids; an EMPTY `bulk_create([])` fires nothing.
//! 3. **m2m_changed** — `M2M::add` / `remove` / `set` / `clear` each fire
//!    `m2m_changed:<junction>` with the right `action`.
//! 4. **actor envelope** — every emitted payload carries an `"actor"` key
//!    sourced from the nearest `with_actor(...)` scope (Null outside one).
//!
//! The signals registry and the App pool are both process-wide globals, so
//! every test in this binary serialises on `TEST_LOCK` and clears the
//! registry first.

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex as TokioMutex, OnceCell};

use umbra::orm::M2M;
use umbra::signals::{current_actor, with_actor};
use umbra_signals::{clear_for_tests, emit, subscribe, subscribe_async};

// ---------------------------------------------------------------------------
// Models: a parent (i64 PK) with an M2M to a child, both i64 PK.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(plugin = "sigx")]
pub struct SigBook {
    pub id: i64,
    pub title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: M2M<SigTag>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(plugin = "sigx")]
pub struct SigTag {
    pub id: i64,
    pub name: String,
}

/// `<plugin>_<parent_struct>_<field>` → the junction table the macro
/// derives for `SigBook.tags`.
const JUNCTION: &str = "sigx_sig_book_tags";
const BOOK_TABLE: &str = "sigx_sig_book";

// ---------------------------------------------------------------------------
// Boot — build the App once, then make/run migrations so the parent,
// child, and junction tables exist.
// ---------------------------------------------------------------------------

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("panic_bulk_m2m.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<SigBook>()
            .model::<SigTag>()
            .build()
            .expect("App::build");

        let migration_tmp = tempfile::tempdir().expect("migration tempdir");
        let migration_path = migration_tmp.path().to_path_buf();
        std::mem::forget(migration_tmp);
        umbra::migrate::make_in(&migration_path)
            .await
            .expect("make_in");
        umbra::migrate::run_in(&migration_path)
            .await
            .expect("run_in");
    })
    .await;
}

fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

async fn reset() {
    boot().await;
    let pool = umbra::db::pool();
    sqlx::query(&format!("DELETE FROM {JUNCTION}"))
        .execute(&pool)
        .await
        .expect("clear junction");
    sqlx::query(&format!("DELETE FROM {BOOK_TABLE}"))
        .execute(&pool)
        .await
        .expect("clear books");
    sqlx::query("DELETE FROM sigx_sig_tag")
        .execute(&pool)
        .await
        .expect("clear tags");
    clear_for_tests();
}

// ===========================================================================
// 1. async-panic isolation
// ===========================================================================

/// A panicking async subscriber must not unwind through `emit()`. The emit
/// returns its full subscriber count, and a sibling subscriber on the same
/// signal still fires.
#[tokio::test]
async fn async_handler_panic_is_isolated_and_other_subscribers_still_fire() {
    let _guard = test_lock().lock().await;
    clear_for_tests();

    let survivor_fired = Arc::new(AtomicUsize::new(0));

    // First subscriber: panics.
    subscribe_async("kaboom_signal", move |_payload| async move {
        panic!("async handler intentionally panics");
    });
    // Second subscriber: must still run despite the first panicking.
    {
        let s = survivor_fired.clone();
        subscribe_async("kaboom_signal", move |_payload| {
            let s = s.clone();
            async move {
                s.fetch_add(1, Ordering::SeqCst);
            }
        });
    }

    // The emit itself must not panic — if the panic escaped, this `.await`
    // would unwind the test (and, in production, the ORM write).
    let n = emit("kaboom_signal", json!({"x": 1})).await;

    assert_eq!(
        n, 2,
        "emit must count both subscribers even though one panicked"
    );
    assert_eq!(
        survivor_fired.load(Ordering::SeqCst),
        1,
        "the non-panicking subscriber must still fire after the panic"
    );
}

/// The sync path has the same `catch_unwind` isolation.
#[tokio::test]
async fn sync_handler_panic_is_isolated_and_other_subscribers_still_fire() {
    let _guard = test_lock().lock().await;
    clear_for_tests();

    let survivor_fired = Arc::new(AtomicUsize::new(0));
    subscribe("sync_kaboom", move |_payload| {
        panic!("sync handler intentionally panics");
    });
    {
        let s = survivor_fired.clone();
        subscribe("sync_kaboom", move |_payload| {
            s.fetch_add(1, Ordering::SeqCst);
        });
    }

    let n = emit("sync_kaboom", json!({})).await;
    assert_eq!(n, 2, "emit counts both sync subscribers");
    assert_eq!(
        survivor_fired.load(Ordering::SeqCst),
        1,
        "the non-panicking sync subscriber must still fire"
    );
}

// ===========================================================================
// 2. bulk signals
// ===========================================================================

/// `bulk_create` of N rows fires exactly one `bulk_post_save:<table>` with
/// `created: true` and the N inserted ids.
#[tokio::test]
async fn bulk_create_fires_bulk_post_save_with_ids() {
    let _guard = test_lock().lock().await;
    reset().await;

    let captured = Arc::new(Mutex::new(Value::Null));
    let fired = Arc::new(AtomicUsize::new(0));
    {
        let c = captured.clone();
        let f = fired.clone();
        subscribe(&format!("bulk_post_save:{BOOK_TABLE}"), move |payload| {
            *c.lock().unwrap() = payload.clone();
            f.fetch_add(1, Ordering::SeqCst);
        });
    }

    let n = SigBook::objects()
        .bulk_create(vec![
            SigBook {
                id: 0,
                title: "a".into(),
                tags: M2M::empty(),
            },
            SigBook {
                id: 0,
                title: "b".into(),
                tags: M2M::empty(),
            },
        ])
        .await
        .expect("bulk_create");
    assert_eq!(n, 2, "two rows inserted");

    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "bulk_create fires exactly one bulk_post_save"
    );
    let payload = captured.lock().unwrap().clone();
    assert_eq!(payload["created"], json!(true), "created=true on insert");
    let ids = payload["ids"].as_array().expect("ids array");
    assert_eq!(ids.len(), 2, "ids carries both inserted PKs; got {payload}");
}

/// An EMPTY `bulk_create([])` must fire NO signal (the early `Ok(0)`).
#[tokio::test]
async fn empty_bulk_create_fires_no_signal() {
    let _guard = test_lock().lock().await;
    reset().await;

    let fired = Arc::new(AtomicUsize::new(0));
    {
        let f = fired.clone();
        subscribe(&format!("bulk_post_save:{BOOK_TABLE}"), move |_| {
            f.fetch_add(1, Ordering::SeqCst);
        });
    }

    let n = SigBook::objects()
        .bulk_create(Vec::<SigBook>::new())
        .await
        .expect("empty bulk_create");
    assert_eq!(n, 0, "no rows inserted");
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "an empty bulk_create must not fire bulk_post_save"
    );
}

/// A bulk `QuerySet::delete()` fires `bulk_post_delete:<table>` carrying
/// the ids of the rows it removed.
#[tokio::test]
async fn bulk_delete_fires_bulk_post_delete_with_ids() {
    let _guard = test_lock().lock().await;
    reset().await;

    // Seed two rows with no signal handler registered yet.
    let b1 = SigBook::objects()
        .create(SigBook {
            id: 0,
            title: "x".into(),
            tags: M2M::empty(),
        })
        .await
        .expect("create x");
    let b2 = SigBook::objects()
        .create(SigBook {
            id: 0,
            title: "y".into(),
            tags: M2M::empty(),
        })
        .await
        .expect("create y");
    clear_for_tests();

    let captured = Arc::new(Mutex::new(Value::Null));
    let fired = Arc::new(AtomicUsize::new(0));
    {
        let c = captured.clone();
        let f = fired.clone();
        subscribe(&format!("bulk_post_delete:{BOOK_TABLE}"), move |payload| {
            *c.lock().unwrap() = payload.clone();
            f.fetch_add(1, Ordering::SeqCst);
        });
    }

    let pool = umbra::db::pool();
    let removed = SigBook::objects()
        .on(&pool)
        .delete()
        .await
        .expect("bulk delete");
    assert_eq!(removed, 2, "both rows deleted");

    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "bulk delete fires exactly one bulk_post_delete"
    );
    let payload = captured.lock().unwrap().clone();
    let ids = payload["ids"].as_array().expect("ids array");
    assert_eq!(ids.len(), 2, "ids carries both removed PKs; got {payload}");
    // The ids should be exactly the two we created.
    let mut got: Vec<i64> = ids.iter().map(|v| v.as_i64().unwrap()).collect();
    got.sort_unstable();
    let mut want = vec![b1.id, b2.id];
    want.sort_unstable();
    assert_eq!(got, want, "ids must be the deleted rows' PKs");
}

// ===========================================================================
// 3. m2m_changed
// ===========================================================================

/// `M2M::add` / `remove` / `set` / `clear` each fire `m2m_changed:<junction>`
/// with the corresponding `action`.
#[tokio::test]
async fn m2m_mutations_fire_m2m_changed_with_action() {
    let _guard = test_lock().lock().await;
    reset().await;

    let book = SigBook::objects()
        .create(SigBook {
            id: 0,
            title: "novel".into(),
            tags: M2M::empty(),
        })
        .await
        .expect("create book");
    let t1 = SigTag::objects()
        .create(SigTag {
            id: 0,
            name: "fiction".into(),
        })
        .await
        .expect("create tag1");
    let t2 = SigTag::objects()
        .create(SigTag {
            id: 0,
            name: "classic".into(),
        })
        .await
        .expect("create tag2");

    // Record every action the junction emits, in order.
    let actions = Arc::new(Mutex::new(Vec::<String>::new()));
    {
        let a = actions.clone();
        subscribe(&format!("m2m_changed:{JUNCTION}"), move |payload| {
            if let Some(act) = payload["action"].as_str() {
                a.lock().unwrap().push(act.to_string());
            }
        });
    }

    book.tags.add(&t1).await.expect("add");
    book.tags.remove(&t1).await.expect("remove");
    book.tags.set(&[&t1, &t2]).await.expect("set");
    book.tags.clear().await.expect("clear");

    let seen = actions.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            "add".to_string(),
            "remove".to_string(),
            "set".to_string(),
            "clear".to_string()
        ],
        "each M2M mutation must fire m2m_changed with its action; got {seen:?}"
    );
}

// ===========================================================================
// 4. actor envelope
// ===========================================================================

/// Every emitted payload carries an `"actor"` key. Inside a `with_actor`
/// scope it equals that actor; outside any scope it is `Null`.
#[tokio::test]
async fn emit_payload_carries_actor_from_with_actor_scope() {
    let _guard = test_lock().lock().await;
    clear_for_tests();

    let captured = Arc::new(Mutex::new(Value::Null));
    {
        let c = captured.clone();
        subscribe("actor_probe", move |payload| {
            *c.lock().unwrap() = payload["actor"].clone();
        });
    }

    // Outside any scope: actor is Null but the key is present.
    emit("actor_probe", json!({"k": 1})).await;
    assert_eq!(
        *captured.lock().unwrap(),
        Value::Null,
        "no scope → actor is Null"
    );

    // Inside a with_actor scope: the actor flows through to the payload.
    let identity = json!({"user_id": 7, "kind": "staff"});
    let id_clone = identity.clone();
    with_actor(identity, async move {
        // Sanity: the task-local read agrees.
        assert_eq!(current_actor(), id_clone);
        emit("actor_probe", json!({"k": 2})).await;
    })
    .await;

    assert_eq!(
        *captured.lock().unwrap(),
        json!({"user_id": 7, "kind": "staff"}),
        "with_actor scope must propagate the actor into the signal payload"
    );
}
