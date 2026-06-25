//! Child-side `OneToOne<T>` sugar.
//!
//! `pub user: OneToOne<AuthUser>` (no `#[sqlx(skip)]`) is sugar for
//! `#[umbral(unique)] pub user: ForeignKey<AuthUser>`. The macro
//! rewrites the classification so all downstream code (column spec,
//! hydrate arms, reverse-FK accessor, reverse-O2O accessor) treats
//! it identically to a unique FK. The `OneToOne<T>` type itself
//! gained `new(id) / id() / Decode / Encode` so the field behaves
//! like a `ForeignKey<T>` at the row level.
//!
//! Dispatch:
//!   - With `#[sqlx(skip)]` → PARENT-side back-link (existing).
//!   - Without `#[sqlx(skip)]` → CHILD-side FK + UNIQUE (this file).

#![allow(dead_code, private_interfaces)]

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::OnceCell;
use umbral::orm::OneToOne;
use umbral_auth::{AuthUser, create_user};

/// THE SUGAR — no `ForeignKey`, no `#[umbral(unique)]`, just
/// `OneToOne<AuthUser>`. This is the spelling the user asked for.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct ShopperProfile {
    pub id: i64,
    pub user: OneToOne<AuthUser>,
    pub bio: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<AuthUser>()
            .model::<ShopperProfile>()
            .build()
            .expect("App::build");

        sqlx::query(
            "CREATE TABLE auth_user (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                email TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                is_active BOOLEAN NOT NULL,
                is_staff BOOLEAN NOT NULL,
                is_superuser BOOLEAN NOT NULL,
                date_joined TEXT NOT NULL,
                last_login TEXT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE auth_user");

        // CRITICAL: the column for `user` must be a BIGINT with FK + UNIQUE
        // constraints — the exact shape `#[umbral(unique)] pub user:
        // ForeignKey<AuthUser>` would emit. Hand-create here to keep this
        // test focused on the macro path, not the migration engine.
        sqlx::query(
            "CREATE TABLE shopper_profile (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user INTEGER NOT NULL UNIQUE REFERENCES auth_user(id),
                bio TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE shopper_profile");
    })
    .await;
}

async fn make_user(username: &str) -> AuthUser {
    let now = Utc::now();
    create_user(username, &format!("{username}@example.com"), "pw12345!")
        .await
        .map(|u| AuthUser {
            id: u.id,
            username: u.username,
            email: u.email,
            password_hash: u.password_hash,
            is_active: u.is_active,
            is_staff: u.is_staff,
            is_superuser: u.is_superuser,
            date_joined: now,
            last_login: None,
        })
        .expect("create_user")
}

#[tokio::test]
async fn sugar_field_spec_emits_unique_fk_column() {
    // The whole point of the sugar: the FieldSpec for `user` must
    // be a UNIQUE FK pointing at auth_user — identical to what the
    // longhand `#[umbral(unique)] pub user: ForeignKey<AuthUser>`
    // would produce.
    use umbral::orm::Model;

    let user_field = ShopperProfile::FIELDS
        .iter()
        .find(|f| f.name == "user")
        .expect("user field present in FIELDS");

    assert!(
        user_field.unique,
        "sugar must emit unique=true (it's the U in OneToOne)"
    );
    assert_eq!(
        user_field.fk_target,
        Some("auth_user"),
        "sugar must emit fk_target pointing at auth_user, got {:?}",
        user_field.fk_target
    );
}

#[tokio::test]
async fn create_and_read_round_trip() {
    boot().await;
    let user = make_user("sugar-rt").await;

    // Write: construct OneToOne::new(user.id) the way you would
    // ForeignKey::new(user.id). The Encode impl writes it as i64
    // into the BIGINT column.
    let pool = umbral::db::pool();
    sqlx::query("INSERT INTO shopper_profile (user, bio) VALUES (?, ?)")
        .bind(OneToOne::<AuthUser>::new(user.id))
        .bind("sugar bio")
        .execute(&pool)
        .await
        .expect("insert profile");

    // Read: sqlx::FromRow decodes the BIGINT column into the
    // OneToOne<AuthUser> field via the real Decode impl (no longer
    // the no-op stub). `.id()` returns the FK value.
    // Filter by user.id — the BOOT OnceCell is shared across the
    // whole file's tests so `.first()` would pick whoever inserted
    // earliest in the run.
    let profile = ShopperProfile::objects()
        .filter(shopper_profile::USER.eq(user.id))
        .first()
        .await
        .expect("query")
        .expect("row");
    assert_eq!(profile.user.id(), user.id);
    assert_eq!(profile.bio, "sugar bio");
}

#[tokio::test]
async fn unique_constraint_enforced_on_duplicate_insert() {
    boot().await;
    let user = make_user("sugar-uniq").await;
    let pool = umbral::db::pool();

    sqlx::query("INSERT INTO shopper_profile (user, bio) VALUES (?, ?)")
        .bind(OneToOne::<AuthUser>::new(user.id))
        .bind("first")
        .execute(&pool)
        .await
        .expect("first insert");

    // A second profile for the same user violates the UNIQUE
    // constraint — proves the column was created with UNIQUE.
    let dup_err = sqlx::query("INSERT INTO shopper_profile (user, bio) VALUES (?, ?)")
        .bind(OneToOne::<AuthUser>::new(user.id))
        .bind("second")
        .execute(&pool)
        .await
        .expect_err("second insert must violate UNIQUE");

    let msg = dup_err.to_string();
    assert!(
        msg.contains("UNIQUE") || msg.contains("unique"),
        "expected a UNIQUE constraint error, got: {msg}"
    );
}

#[tokio::test]
async fn sugar_field_also_emits_cross_crate_reverse_accessor() {
    // The other half of the sugar: BECAUSE the rewritten kind goes
    // through the unique-FK path, the reverse-O2O trait emission
    // (`auth_user.shopper_profile().await?`) kicks in automatically.
    // No extra wiring on the user's part.
    boot().await;
    let user = make_user("sugar-rev").await;
    let pool = umbral::db::pool();

    sqlx::query("INSERT INTO shopper_profile (user, bio) VALUES (?, ?)")
        .bind(OneToOne::<AuthUser>::new(user.id))
        .bind("rev bio")
        .execute(&pool)
        .await
        .expect("insert");

    // The reverse accessor (e.g. user.profile.bio).
    let profile = user
        .shopper_profile()
        .await
        .expect("reverse-o2o query")
        .expect("profile exists");
    assert_eq!(profile.bio, "rev bio");
    assert_eq!(profile.user.id(), user.id);
}

/// Critical regression test for the Side discriminator.
/// Without it, the Serialize impl would emit `parent_id` for the
/// PARENT-side back-link slot too, breaking the
/// `template_shaped_json_emits_nested_profile_or_null` shape
/// AND poisoning the create-path validator with the parent's own
/// PK leaking into the field's JSON value.
#[tokio::test]
async fn serialize_emits_fk_id_for_child_side_only() {
    use umbral::orm::OneToOne;

    // Child-side: constructed via `new(id)` → Side::Child.
    // Serialize emits the FK value as a number — that's what the
    // create-path validator reads to satisfy NOT NULL on the column.
    let child: OneToOne<AuthUser> = OneToOne::new(42);
    let j = serde_json::to_value(&child).expect("serialize");
    assert_eq!(
        j,
        serde_json::json!(42),
        "child-side OneToOne::new(42) must serialize as the FK id `42`, got {j}"
    );

    // Parent-side: default constructor → Side::Parent. Even with a
    // parent_id set via set_parent_id() (which the macro emits at
    // row-decode time for prefetch bucketing), the FIELD VALUE must
    // serialize as null — the parent_id is the parent's OWN PK, not
    // the field's data, and leaking it would break JSON output.
    let mut parent: OneToOne<AuthUser> = OneToOne::empty();
    // PK lift: set_parent_id takes the parent's PK as a serde_json::Value.
    parent.set_parent_id(serde_json::json!(99));
    let j = serde_json::to_value(&parent).expect("serialize");
    assert!(
        j.is_null(),
        "parent-side OneToOne must serialize as null (not the parent's own PK), got {j}"
    );
}

#[tokio::test]
async fn id_panics_on_unset_slot() {
    // OneToOne::id() panics when unset — matches ForeignKey::id().
    // The caller constructed the row, so they should have set the
    // FK. An unset OneToOne is a programming error, not a runtime
    // condition to silently paper over.
    let slot: OneToOne<AuthUser> = OneToOne::empty();
    let result = std::panic::catch_unwind(|| slot.id());
    assert!(result.is_err(), "OneToOne::id() on empty slot must panic");
}
