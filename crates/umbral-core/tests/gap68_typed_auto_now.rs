//! gap68: the TYPED write path must honor `#[umbral(auto_now_add)]` /
//! `#[umbral(auto_now)]`, not just the dynamic (REST/admin JSON+form) path.
//!
//! Because a Rust struct literal forces EVERY field, callers of
//! `objects().create(instance)` are compelled to write *some* value into a
//! `created_at` / `updated_at` field — typically a `Default`-derived epoch
//! sentinel (`1970-01-01`). Before this fix the typed path persisted that
//! boilerplate value verbatim, so rows landed dated to the epoch. The contract
//! now matches Django and the dynamic path:
//!
//! - `auto_now_add` = force `now()` on INSERT, frozen forever after (an UPDATE
//!   never rewrites it, even though the struct still carries a value).
//! - `auto_now` = force `now()` on every save (INSERT and UPDATE).

#![allow(dead_code)]

use chrono::{DateTime, Datelike, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "gap68_event")]
pub struct Event {
    pub id: i64,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    pub label: String,
}

static SERIALISE: Mutex<()> = Mutex::const_new(());
static BOOT: OnceCell<()> = OnceCell::const_new();

fn epoch() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).expect("epoch")
}

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("gap68.sqlite");
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
            .model::<Event>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

async fn clear() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM gap68_event")
        .execute(&pool)
        .await
        .expect("clear");
}

/// A typed `create` whose struct carries the epoch sentinel for both timestamp
/// columns must persist `now()`, not `1970-01-01`.
#[tokio::test]
async fn typed_create_stamps_now_over_epoch_sentinel() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let before = Utc::now();
    let row = Event::objects()
        .create(Event {
            id: 0,
            created_at: epoch(), // boilerplate the struct literal forced
            updated_at: epoch(),
            label: "first".to_string(),
        })
        .await
        .expect("create");
    let after = Utc::now();

    assert_ne!(
        row.created_at,
        epoch(),
        "created_at kept the epoch sentinel"
    );
    assert_ne!(
        row.updated_at,
        epoch(),
        "updated_at kept the epoch sentinel"
    );
    assert!(
        row.created_at.year() >= 2020,
        "created_at not recent: {}",
        row.created_at
    );
    // Both stamped values sit inside the [before, after] window around the write
    // (allowing a tiny slack for storage rounding).
    let slack = chrono::Duration::seconds(2);
    assert!(
        row.created_at >= before - slack && row.created_at <= after + slack,
        "created_at {} outside [{before}, {after}]",
        row.created_at
    );
    assert!(
        row.updated_at >= before - slack && row.updated_at <= after + slack,
        "updated_at {} outside [{before}, {after}]",
        row.updated_at
    );
}

/// A typed `save` (UPDATE) refreshes the `auto_now` column to `now()` while the
/// `auto_now_add` column stays frozen at the original insert time — even though
/// the in-memory struct still carries both old values.
#[tokio::test]
async fn typed_update_refreshes_auto_now_but_freezes_auto_now_add() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let created = Event::objects()
        .create(Event {
            id: 0,
            created_at: epoch(),
            updated_at: epoch(),
            label: "v1".to_string(),
        })
        .await
        .expect("create");
    let original_created = created.created_at;
    let original_updated = created.updated_at;

    // Ensure the clock advances measurably before the update.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // Edit an unrelated column and save. The struct still carries the old
    // timestamps — the framework must ignore them.
    let mut edited = created.clone();
    edited.label = "v2".to_string();
    let saved = Event::objects().save(edited).await.expect("save/update");

    assert_eq!(saved.label, "v2");
    assert_eq!(
        saved.created_at, original_created,
        "auto_now_add moved on update (should be frozen)"
    );
    assert!(
        saved.updated_at > original_updated,
        "auto_now did not advance on update: {} !> {}",
        saved.updated_at,
        original_updated
    );

    // Read the row back independently to prove it's the stored state, not just
    // the RETURNING image.
    let fetched = Event::objects()
        .filter(event::ID.eq(saved.id))
        .first()
        .await
        .expect("refetch")
        .expect("row exists");
    assert_eq!(fetched.created_at, original_created);
    assert_eq!(fetched.updated_at, saved.updated_at);
    assert!(fetched.updated_at > original_updated);
}
