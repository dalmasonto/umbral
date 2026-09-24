//! gaps6 #7 — Task 6: string-pk target coverage.
//!
//! Every model in `materialized_fields.rs` has an `i64` primary key. The
//! write-back in `materialized.rs` goes through `DynQuerySet::filter_pk_eq`,
//! which coerces the target's PK from a `serde_json::Value` regardless of
//! its declared type — but nothing over there actually proves that for a
//! non-i64 PK. `MfClub` (target, `String` primary key `slug`) refreshed from
//! `MfMember` (source, keyed by `club_slug: String`) is that integration
//! proof: if `filter_pk_eq` (or anything upstream of it) silently assumed
//! `i64`, this test fails on the final read-back rather than on a type
//! error, since JSON carries both shapes fine — the assertion is the only
//! thing that would catch a wrong-type regression.
//!
//! This lives in its OWN file (not appended to `materialized_fields.rs`)
//! because `App::builder()...build()` publishes process-global ambient
//! state (`umbral::settings::init` panics if called twice in one process).
//! Every `tests/*.rs` file compiles to its own test binary / process (the
//! same reason `pk_string_backup.rs`, `pk_string_m2m.rs`, etc. each own a
//! private `boot()`), so a second, independently-configured `App` needs a
//! second file, not a second boot helper sharing this process with the
//! `i64`-keyed `boot()` in `materialized_fields.rs`.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::orm::Materialized;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mf_club")]
pub struct MfClub {
    #[umbral(primary_key)]
    pub slug: String,
    pub member_count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "mf_member")]
pub struct MfMember {
    pub id: i64,
    pub club_slug: String,
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot_str() -> sqlx::SqlitePool {
    static ONCE: tokio::sync::OnceCell<sqlx::SqlitePool> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mf_str.sqlite");
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
            .unwrap();
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<MfClub>()
            .model::<MfMember>()
            .materialize(
                Materialized::<MfClub>::field(mf_club::MEMBER_COUNT)
                    .from::<MfMember, _, _>(|m: &MfMember| Some(m.club_slug.clone()))
                    .recompute_typed(|slug: String| async move {
                        MfMember::objects()
                            .filter(mf_member::CLUB_SLUG.eq(&slug))
                            .count()
                            .await
                            .unwrap_or(0)
                    }),
            )
            .build()
            .unwrap();
        umbral_core::migrate::create_tables_for_tests()
            .await
            .unwrap();
        pool
    })
    .await
    .clone()
}

#[tokio::test]
async fn refresh_works_for_a_string_pk_target() {
    let _g = lock().lock().await;
    let _pool = boot_str().await;

    MfClub::objects()
        .create(MfClub {
            slug: "acme".to_string(),
            member_count: 0,
        })
        .await
        .unwrap();

    MfMember::objects()
        .create(MfMember {
            id: 0,
            club_slug: "acme".to_string(),
        })
        .await
        .unwrap();
    MfMember::objects()
        .create(MfMember {
            id: 0,
            club_slug: "acme".to_string(),
        })
        .await
        .unwrap();

    let club = MfClub::objects()
        .filter(mf_club::SLUG.eq("acme"))
        .get()
        .await
        .unwrap();
    assert_eq!(
        club.member_count, 2,
        "member_count tracks the source count with no manual recompute, \
         proving the write-back handles a String primary key via filter_pk_eq"
    );
}
