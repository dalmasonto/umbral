//! gaps2 #92 — `pre_update:<table>` / `post_update:<table>` signals.
//!
//! These fire ONLY on the typed per-row UPDATE path (`Manager::save` of an
//! existing row), carry both the OLD (`previous`) and NEW (`instance`) row,
//! and are gated by a subscriber-presence check: with no `*_update`
//! subscriber, the save path skips the extra old-row SELECT entirely (and
//! no `*_update` signal fires). `pre_save` / `post_save` are unchanged and
//! still fire on both INSERT and UPDATE.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex as TokioMutex, OnceCell};

use umbral::orm::Predicate;
use umbral_core::signals::{clear_for_tests, has_subscribers, subscribe};

/// Process-wide serialiser so the shared `updpost` table isn't raced.
static SERIALISE: TokioMutex<()> = TokioMutex::const_new(());

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "updpost")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub published: bool,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("update_signals.sqlite");
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
            .expect("pool");
        let _ = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

async fn truncate() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM updpost")
        .execute(&pool)
        .await
        .expect("truncate");
}

/// Subscribe to a signal and capture every payload it receives.
fn capture(signal: &str) -> Arc<Mutex<Vec<Value>>> {
    let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let c = captured.clone();
    subscribe(signal, move |p| c.lock().unwrap().push(p.clone()));
    captured
}

/// gaps3 #14 — `update_or_create` must fire the per-row `post_save` on BOTH
/// its branches. The CREATE branch already did (via `self.create()`); the
/// UPDATE branch previously only fired `bulk_post_save` (via `update_values`),
/// so signal / realtime `on_model` consumers silently missed upsert-updates.
#[tokio::test]
async fn post_save_fires_on_both_branches_of_update_or_create() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    clear_for_tests();

    let captured = capture("post_save:updpost");

    // First call: no matching row → CREATE branch → post_save (created=true).
    let (_row, created) = Post::objects()
        .update_or_create(
            Predicate::col_eq("title", "uoc"),
            Post {
                id: 0,
                title: "uoc".into(),
                published: false,
            },
        )
        .await
        .expect("update_or_create insert");
    assert!(created, "first call inserts");
    assert_eq!(
        captured.lock().unwrap().len(),
        1,
        "post_save fires on the CREATE branch"
    );

    // Second call: the row now exists (title still 'uoc') → UPDATE branch.
    let (_row2, created2) = Post::objects()
        .update_or_create(
            Predicate::col_eq("title", "uoc"),
            Post {
                id: 0,
                title: "uoc".into(),
                published: true,
            },
        )
        .await
        .expect("update_or_create update");
    assert!(!created2, "second call updates the existing row");
    assert_eq!(
        captured.lock().unwrap().len(),
        2,
        "post_save ALSO fires on the UPDATE branch (gaps3 #14)"
    );
}

#[tokio::test]
async fn post_update_fires_on_update_with_previous_and_instance() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    clear_for_tests();

    let captured = capture("post_update:updpost");

    // INSERT first (this must NOT fire post_update).
    let created = Post::objects()
        .save(Post {
            id: 0,
            title: "before".into(),
            published: false,
        })
        .await
        .expect("insert");
    assert!(
        captured.lock().unwrap().is_empty(),
        "post_update must not fire on INSERT"
    );

    // UPDATE — change the title.
    let mut updated = created.clone();
    updated.title = "after".into();
    updated.published = true;
    let _ = Post::objects().save(updated).await.expect("update");

    let events = captured.lock().unwrap();
    assert_eq!(events.len(), 1, "exactly one post_update on the UPDATE");
    let p = &events[0];
    assert_eq!(p["previous"]["title"], "before", "old value in `previous`");
    assert_eq!(p["previous"]["published"], false);
    assert_eq!(p["instance"]["title"], "after", "new value in `instance`");
    assert_eq!(p["instance"]["published"], true);
    assert_eq!(p["previous"]["id"], created.id, "same PK both sides");
    assert_eq!(p["instance"]["id"], created.id);
    assert!(p.get("actor").is_some(), "actor envelope present");
}

#[tokio::test]
async fn pre_update_fires_before_update() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    clear_for_tests();

    let captured = capture("pre_update:updpost");

    let created = Post::objects()
        .save(Post {
            id: 0,
            title: "v1".into(),
            published: false,
        })
        .await
        .expect("insert");
    assert!(
        captured.lock().unwrap().is_empty(),
        "pre_update must not fire on INSERT"
    );

    let mut updated = created.clone();
    updated.title = "v2".into();
    let _ = Post::objects().save(updated).await.expect("update");

    let events = captured.lock().unwrap();
    assert_eq!(events.len(), 1);
    // pre_update carries the OLD row in `previous` and the about-to-write
    // value in `instance`.
    assert_eq!(events[0]["previous"]["title"], "v1");
    assert_eq!(events[0]["instance"]["title"], "v2");
}

#[tokio::test]
async fn has_subscribers_flips_false_to_true_around_subscribe() {
    let _guard = SERIALISE.lock().await;
    clear_for_tests();
    assert!(!has_subscribers("post_update:hs_demo"), "no subscriber yet");
    subscribe("post_update:hs_demo", |_| {});
    assert!(
        has_subscribers("post_update:hs_demo"),
        "subscriber registered"
    );
    clear_for_tests();
    assert!(!has_subscribers("post_update:hs_demo"), "cleared again");
}

#[tokio::test]
async fn update_with_no_update_subscriber_still_works_and_emits_nothing() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    clear_for_tests();

    // Capture the *_update signals but DON'T subscribe to them — capture()
    // would subscribe, so instead we assert via the post_save signal that
    // the UPDATE happened while no *_update event was produced. We confirm
    // the gating by checking has_subscribers is false for both names.
    assert!(!has_subscribers("pre_update:updpost"));
    assert!(!has_subscribers("post_update:updpost"));

    // A post_save subscriber proves the UPDATE path ran end-to-end.
    let post_saves = capture("post_save:updpost");

    let created = Post::objects()
        .save(Post {
            id: 0,
            title: "x".into(),
            published: false,
        })
        .await
        .expect("insert");
    let mut updated = created.clone();
    updated.title = "y".into();
    let out = Post::objects().save(updated).await.expect("update");
    assert_eq!(out.title, "y", "UPDATE applied with no *_update subscriber");

    // post_save fired twice (INSERT + UPDATE); no *_update subscriber exists
    // so the gated old-row read was skipped entirely.
    let events = post_saves.lock().unwrap();
    assert_eq!(events.len(), 2, "post_save fires on both INSERT and UPDATE");
    assert_eq!(events[0]["created"], true);
    assert_eq!(events[1]["created"], false);
}

#[tokio::test]
async fn create_does_not_fire_update_signals() {
    let _guard = SERIALISE.lock().await;
    boot().await;
    truncate().await;
    clear_for_tests();

    let pre = capture("pre_update:updpost");
    let post = capture("post_update:updpost");

    // `create` is the INSERT-only manager terminal — it must never fire
    // pre_update / post_update.
    let _ = Post::objects()
        .create(Post {
            id: 0,
            title: "fresh".into(),
            published: false,
        })
        .await
        .expect("create");

    assert!(pre.lock().unwrap().is_empty(), "create -> no pre_update");
    assert!(post.lock().unwrap().is_empty(), "create -> no post_update");
}
