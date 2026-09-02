//! gap69: `get_or_create` / `update_or_create` must give a CLEAR error when the
//! INSERT trips a UNIQUE constraint on a column the caller's predicate does not
//! cover.
//!
//! The race-safe upsert catches a `UniqueViolation`, then re-fetches by the
//! caller's predicate. It silently assumed the violated constraint IS the
//! predicate's columns. When a DIFFERENT unique column collides, the re-fetch
//! by predicate finds nothing and the old code errored "row vanished after
//! UniqueViolation re-fetch" — naming neither the real constraint nor the
//! mismatch.
//!
//! Real case: `Timezone.slug` is `unique` and derived non-injectively from
//! `name` (`Etc/GMT+9` and `Etc/GMT-9` both slugify to `etc-gmt-9`). The upsert
//! predicated on `name`, the INSERT hit a `UniqueViolation` on `slug`, and the
//! re-fetch by `name` found nothing → "vanished".
//!
//! The fix: name the violated constraint AND the predicate, and distinguish a
//! cross-constraint collision (a different row owns the colliding value) from a
//! genuine concurrent delete.

#![allow(dead_code)]

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use umbral::orm::write::WriteError;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "gap69_timezone")]
pub struct Timezone {
    pub id: i64,
    /// Predicate column in the tests.
    #[umbral(unique)]
    pub name: String,
    /// The DIFFERENT unique column that collides (a non-injective slug).
    #[umbral(unique)]
    pub slug: String,
}

static SERIALISE: Mutex<()> = Mutex::const_new(());
static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("gap69.sqlite");
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
            .model::<Timezone>()
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
    sqlx::query("DELETE FROM gap69_timezone")
        .execute(&pool)
        .await
        .expect("clear");
}

fn message_of(err: &WriteError) -> String {
    err.to_string()
}

/// `get_or_create` predicated on `name` (a miss), whose defaults collide on the
/// separate `slug` UNIQUE against an existing row, returns an error that NAMES
/// the violated constraint (`slug`) and the predicate (`name`) — not "vanished".
#[tokio::test]
async fn get_or_create_cross_constraint_names_both_columns() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    // Existing row owns slug "etc-gmt-9".
    Timezone::objects()
        .create(Timezone {
            id: 0,
            name: "Etc/GMT+9".to_string(),
            slug: "etc-gmt-9".to_string(),
        })
        .await
        .expect("seed row A");

    // Predicate misses (no row named "Etc/GMT-9"), but the defaults' slug
    // collides with row A's slug on the separate UNIQUE.
    let err = Timezone::objects()
        .get_or_create(
            timezone::NAME.eq("Etc/GMT-9"),
            Timezone {
                id: 0,
                name: "Etc/GMT-9".to_string(),
                slug: "etc-gmt-9".to_string(),
            },
        )
        .await
        .expect_err("cross-constraint collision must error");

    let msg = message_of(&err);
    assert!(
        msg.contains("slug"),
        "error must name the violated constraint `slug`: {msg}"
    );
    assert!(
        msg.contains("name"),
        "error must name the predicate column `name`: {msg}"
    );
    assert!(
        !msg.contains("vanished"),
        "cross-constraint error must NOT read as the old `vanished` mystery: {msg}"
    );
}

/// Same disambiguation for `update_or_create`.
#[tokio::test]
async fn update_or_create_cross_constraint_names_both_columns() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    Timezone::objects()
        .create(Timezone {
            id: 0,
            name: "Etc/GMT+9".to_string(),
            slug: "etc-gmt-9".to_string(),
        })
        .await
        .expect("seed row A");

    let err = Timezone::objects()
        .update_or_create(
            timezone::NAME.eq("Etc/GMT-9"),
            Timezone {
                id: 0,
                name: "Etc/GMT-9".to_string(),
                slug: "etc-gmt-9".to_string(),
            },
        )
        .await
        .expect_err("cross-constraint collision must error");

    let msg = message_of(&err);
    assert!(msg.contains("slug"), "must name `slug`: {msg}");
    assert!(msg.contains("name"), "must name predicate `name`: {msg}");
    assert!(
        !msg.contains("vanished"),
        "must not read as vanished: {msg}"
    );
}

/// No regression: when the predicate DOES correspond to the colliding
/// constraint, `get_or_create` still converges — insert on miss, return the
/// existing row on a second call.
#[tokio::test]
async fn get_or_create_same_constraint_still_upserts() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let (first, created) = Timezone::objects()
        .get_or_create(
            timezone::NAME.eq("Etc/UTC"),
            Timezone {
                id: 0,
                name: "Etc/UTC".to_string(),
                slug: "etc-utc".to_string(),
            },
        )
        .await
        .expect("first get_or_create");
    assert!(created, "first call should insert");
    assert_eq!(first.name, "Etc/UTC");

    let (second, created2) = Timezone::objects()
        .get_or_create(
            timezone::NAME.eq("Etc/UTC"),
            Timezone {
                id: 0,
                name: "Etc/UTC".to_string(),
                slug: "etc-utc".to_string(),
            },
        )
        .await
        .expect("second get_or_create");
    assert!(!created2, "second call should hit the existing row");
    assert_eq!(second.id, first.id, "converged on the same row");
}

/// No regression on the update side either.
#[tokio::test]
async fn update_or_create_same_constraint_still_upserts() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let (first, created) = Timezone::objects()
        .update_or_create(
            timezone::NAME.eq("Africa/Nairobi"),
            Timezone {
                id: 0,
                name: "Africa/Nairobi".to_string(),
                slug: "africa-nairobi".to_string(),
            },
        )
        .await
        .expect("insert path");
    assert!(created);

    // Second call hits the existing row and updates it (slug stays valid).
    let (second, created2) = Timezone::objects()
        .update_or_create(
            timezone::NAME.eq("Africa/Nairobi"),
            Timezone {
                id: 0,
                name: "Africa/Nairobi".to_string(),
                slug: "africa-nairobi".to_string(),
            },
        )
        .await
        .expect("update path");
    assert!(!created2);
    assert_eq!(second.id, first.id);
}
