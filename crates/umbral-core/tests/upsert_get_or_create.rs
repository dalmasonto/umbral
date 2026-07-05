//! End-to-end coverage for `Manager::get_or_create` and `Manager::upsert`.
//!
//! Both terminals are the ORM's answer to common write patterns
//! (`get_or_create`) and the SQLite-native `INSERT ... ON CONFLICT DO
//! UPDATE` (upsert). The tests pin the round-trip and the
//! `(row, created)` flag for `get_or_create`.

#![allow(dead_code)]

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Barrier, Mutex, OnceCell};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "goc_widget")]
pub struct Widget {
    pub id: i64,
    pub slug: String,
    pub label: String,
    pub stock: i64,
}

/// One mutex serialises every test so they don't race on the shared
/// `goc_widget` table after boot.
static SERIALISE: Mutex<()> = Mutex::const_new(());
static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("upsert_goc.sqlite");
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

        let _app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Widget>()
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE goc_widget (\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\
                 slug TEXT NOT NULL UNIQUE,\
                 label TEXT NOT NULL,\
                 stock INTEGER NOT NULL DEFAULT 0\
             )",
        )
        .execute(&pool)
        .await
        .expect("create goc_widget");
    })
    .await;
}

async fn clear() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM goc_widget")
        .execute(&pool)
        .await
        .expect("clear");
}

#[tokio::test]
async fn get_or_create_inserts_when_predicate_misses() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let (widget, created) = Widget::objects()
        .get_or_create(
            widget::SLUG.eq("alpha"),
            Widget {
                id: 0,
                slug: "alpha".to_string(),
                label: "Alpha".to_string(),
                stock: 5,
            },
        )
        .await
        .expect("get_or_create");
    assert!(created);
    assert_eq!(widget.slug, "alpha");
    assert_eq!(widget.stock, 5);
    assert!(widget.id > 0);
}

#[tokio::test]
async fn get_or_create_returns_existing_row_on_hit() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let seeded = Widget::objects()
        .create(Widget {
            id: 0,
            slug: "beta".to_string(),
            label: "Beta original".to_string(),
            stock: 7,
        })
        .await
        .expect("seed");

    let (widget, created) = Widget::objects()
        .get_or_create(
            widget::SLUG.eq("beta"),
            Widget {
                id: 0,
                slug: "beta".to_string(),
                label: "Beta should NOT appear".to_string(),
                stock: 999,
            },
        )
        .await
        .expect("get_or_create");
    assert!(!created);
    assert_eq!(widget.id, seeded.id);
    assert_eq!(widget.label, "Beta original");
    assert_eq!(widget.stock, 7);
}

#[tokio::test]
async fn upsert_inserts_when_no_conflict() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let row = Widget::objects()
        .upsert(Widget {
            id: 0,
            slug: "gamma".to_string(),
            label: "Gamma".to_string(),
            stock: 11,
        })
        .await
        .expect("upsert");
    assert!(row.id > 0);
    assert_eq!(row.slug, "gamma");
    assert_eq!(row.stock, 11);
}

#[tokio::test]
async fn upsert_updates_when_pk_conflicts() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let seeded = Widget::objects()
        .create(Widget {
            id: 0,
            slug: "delta".to_string(),
            label: "Delta v1".to_string(),
            stock: 3,
        })
        .await
        .expect("seed");

    let row = Widget::objects()
        .upsert(Widget {
            id: seeded.id,
            slug: "delta".to_string(),
            label: "Delta v2".to_string(),
            stock: 99,
        })
        .await
        .expect("upsert");

    assert_eq!(row.id, seeded.id);
    assert_eq!(row.label, "Delta v2");
    assert_eq!(row.stock, 99);

    let count = Widget::objects().count().await.expect("count");
    assert_eq!(count, 1);
}

/// Convergence test: two concurrent `get_or_create` callers race on the same
/// slug. Without the UniqueViolation-convergence fix one of them would surface
/// a `UniqueViolation` error to the caller. With the fix both converge: one
/// gets `created=true`, the other gets `created=false`, and only one row exists.
///
/// Approach: a `Barrier` forces both tasks to complete their SELECT before
/// either proceeds to the INSERT, maximising the chance that both see an empty
/// table and race on the same UNIQUE slug. This exercises the
/// UniqueViolation→re-fetch path in `get_or_create`.
///
/// We still hold `SERIALISE` so other tests in this binary cannot truncate
/// the table while the two internal tasks are racing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn get_or_create_converges_under_concurrent_insert() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let barrier = std::sync::Arc::new(Barrier::new(2));

    let b1 = barrier.clone();
    let t1 = tokio::spawn(async move {
        // Wait for both tasks to be ready before issuing the SELECT.
        b1.wait().await;
        Widget::objects()
            .get_or_create(
                widget::SLUG.eq("race"),
                Widget {
                    id: 0,
                    slug: "race".to_string(),
                    label: "Race Winner".to_string(),
                    stock: 1,
                },
            )
            .await
    });

    let b2 = barrier.clone();
    let t2 = tokio::spawn(async move {
        b2.wait().await;
        Widget::objects()
            .get_or_create(
                widget::SLUG.eq("race"),
                Widget {
                    id: 0,
                    slug: "race".to_string(),
                    label: "Race Loser".to_string(),
                    stock: 2,
                },
            )
            .await
    });

    let r1 = t1
        .await
        .expect("task1 panicked")
        .expect("task1 get_or_create");
    let r2 = t2
        .await
        .expect("task2 panicked")
        .expect("task2 get_or_create");

    let (w1, c1) = r1;
    let (w2, c2) = r2;

    // Exactly one caller created the row.
    assert_eq!(
        c1 as u8 + c2 as u8,
        1,
        "exactly one task should have created=true"
    );
    // Both callers see the same row id and slug.
    assert_eq!(w1.id, w2.id, "both callers must see the same row");
    assert_eq!(w1.slug, "race");
    assert_eq!(w2.slug, "race");

    // Only one row was inserted despite the race.
    let count = Widget::objects().count().await.expect("count");
    assert_eq!(
        count, 1,
        "only one row must exist after concurrent get_or_create"
    );
}
