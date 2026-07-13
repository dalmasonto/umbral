//! PK refactor — OneToOne on a String-PK model, both directions.
//!
//!   1. PARENT side: a `String`-slug-PK `Account` carries a parent-side
//!      `OneToOne<Profile>` back-link and hydrates it via prefetch.
//!   2. CHILD side: a `OneToOne<Account>` FK column stores the target's
//!      `String` PK and round-trips (decode + `.id()` give back the slug).
//!
//! Before the lift `OneToOne<C>` stored a shared `Option<i64>`, so neither
//! direction worked for a non-i64 PK.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, OneToOne};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "oto_pk_account")]
pub struct Account {
    /// Slug primary key — a `String`.
    #[umbral(primary_key)]
    pub handle: String,
    pub name: String,
    /// Parent-side reverse OneToOne — populated by prefetch.
    #[sqlx(skip)]
    #[serde(skip)]
    pub profile: OneToOne<Profile>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "oto_pk_profile")]
pub struct Profile {
    pub id: i64,
    /// UNIQUE FK back at the String-PK Account — makes the reverse side a
    /// OneToOne. The column holds the account's slug.
    #[umbral(unique)]
    pub account: ForeignKey<Account>,
    pub bio: String,
}

/// Child-side `OneToOne<Account>` (no `#[sqlx(skip)]`): a unique FK whose
/// value is the target Account's `String` PK.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "oto_pk_badge")]
pub struct Badge {
    pub id: i64,
    pub holder: OneToOne<Account>,
    pub label: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Account>()
            .model::<Profile>()
            .model::<Badge>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        for (handle, name) in &[("ada", "Ada"), ("grace", "Grace")] {
            sqlx::query("INSERT INTO oto_pk_account (handle, name) VALUES (?, ?)")
                .bind(*handle)
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed account");
        }
        // ada has a profile; grace does not.
        sqlx::query("INSERT INTO oto_pk_profile (account, bio) VALUES (?, ?)")
            .bind("ada")
            .bind("first programmer")
            .execute(&pool)
            .await
            .expect("seed profile");
        // a badge held by ada (child-side OneToOne FK = the slug "ada").
        sqlx::query("INSERT INTO oto_pk_badge (holder, label) VALUES (?, ?)")
            .bind("ada")
            .bind("pioneer")
            .execute(&pool)
            .await
            .expect("seed badge");
    })
    .await;
}

#[tokio::test]
async fn parent_side_one_to_one_hydrates_on_a_string_pk_parent() {
    boot().await;
    let accounts = Account::objects()
        .prefetch_related("profile")
        .fetch()
        .await
        .expect("fetch");

    let by_handle: std::collections::HashMap<&str, &Account> =
        accounts.iter().map(|a| (a.handle.as_str(), a)).collect();

    let ada = by_handle.get("ada").expect("ada present");
    let profile = ada
        .profile
        .resolved()
        .expect("OneToOne hydrated for a String-PK parent");
    assert_eq!(profile.bio, "first programmer");

    let grace = by_handle.get("grace").expect("grace present");
    assert!(
        grace.profile.resolved().is_none(),
        "grace has no profile → resolved() is None"
    );
    assert!(
        grace.profile.is_loaded(),
        "but the slot was loaded (prefetch ran)"
    );
}

#[tokio::test]
async fn child_side_one_to_one_fk_round_trips_a_string_pk() {
    boot().await;
    let badges = Badge::objects().fetch().await.expect("fetch badges");
    assert_eq!(badges.len(), 1);
    // The child-side OneToOne<Account> decoded the FK column (a slug) and
    // .id() hands it back as the target's PK type — a String, not an i64.
    let holder: String = badges[0].holder.id();
    assert_eq!(holder, "ada");
    assert_eq!(badges[0].label, "pioneer");
}
