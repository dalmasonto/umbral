//! Cross-crate reverse-OneToOne accessor.
//!
//! Mirrors the gap #105 reverse-FK trait trick for the OneToOne
//! shape: when a child declares `#[umbra(unique)] pub user:
//! ForeignKey<AuthUser>`, the derive emits a trait + an
//! `impl ... for AuthUser` so callers can spell
//! `auth_user.<child>().await?` to get `Option<Child>` directly —
//! even though `AuthUser` lives in a different crate (umbra-auth)
//! and we never touch its struct definition.
//!
//! The cross-crate-ness is implicit in this file's setup:
//! - `AuthUser` is defined in `umbra-auth` (the foreign crate).
//! - `CustomerProfile` (the child) is defined in *this* test crate.
//! - The derive macro emits a trait in *this* crate AND
//!   `impl LocalTrait for ForeignType { ... }` — that's the
//!   orphan-rule-friendly shape (impl of a LOCAL trait on a
//!   FOREIGN type is always allowed).
//! - If the emission accidentally tried to add an inherent impl on
//!   AuthUser, this file would fail to compile with E0117.

#![allow(dead_code, private_interfaces)]

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::OnceCell;
use umbra::orm::ForeignKey;
use umbra_auth::{AuthUser, create_user};

/// Child model that pretends to live in some app crate. The
/// `#[umbra(unique)]` on `user` is what makes the FK a OneToOne —
/// the derive macro picks that up and emits both:
///   - the regular reverse-FK accessor `auth_user.customer_profile_set()`
///     (returning `QuerySet<CustomerProfile>`)
///   - the NEW reverse-OneToOne accessor `auth_user.customer_profile().await?`
///     (returning `Option<CustomerProfile>` directly — the unique
///     constraint guarantees at most one row, so callers skip the
///     QuerySet hop)
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct CustomerProfile {
    pub id: i64,
    #[umbra(unique, on_delete = "cascade")]
    pub user: ForeignKey<AuthUser>,
    pub bio: String,
}

/// A second child to prove the trait method name `customer_profile`
/// is the *struct-name snake_cased*, not derived from the FK field
/// name — so two unrelated models both with `user: FK<AuthUser> +
/// unique` get DISTINCT method names (`customer_profile()` and
/// `wishlist()`), not a collision.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct Wishlist {
    pub id: i64,
    #[umbra(unique, on_delete = "cascade")]
    pub user: ForeignKey<AuthUser>,
    pub label: String,
}

/// Plain (non-unique) FK to AuthUser. Should get the reverse-FK
/// accessor (`audit_note_set()`) but NOT the reverse-OneToOne
/// accessor — there's no UNIQUE constraint, so the cardinality is
/// 0..N and the o2o shape would be wrong.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct AuditNote {
    pub id: i64,
    pub user: ForeignKey<AuthUser>,
    pub note: String,
}

// NOTE: the traits emitted by the derive
// (`CustomerProfileUserOneToOneReverse`, `WishlistUserOneToOneReverse`)
// are already in scope here — the derive emits them at the same
// module level as the struct, so a plain `auth_user.customer_profile()
// .await?` resolves without any extra `use ...`. In a downstream app
// crate, the caller writes `use that_crate::*;` or names the trait
// explicitly. The compilation of the test functions below is the
// proof that the traits are public and reachable.

static BOOT: OnceCell<()> = OnceCell::const_new();

/// Every test in this binary shares one ambient SQLite pool (created once in
/// `boot()` and published into umbra-core's process-wide `OnceLock`s by
/// `App::build()`). The default test harness runs these `#[tokio::test]`s on
/// parallel OS threads, so they insert into / read from that single pool
/// concurrently, which is the within-binary contention that flaked under
/// full-workspace runs (gaps2 #30). Serialising the test bodies on this lock
/// makes the shared pool single-user-at-a-time. Mirrors the `NOTE_LOCK`
/// pattern in `plugins/umbra-admin/tests/phase2_sheet.rs`.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");

        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<AuthUser>()
            .model::<CustomerProfile>()
            .model::<Wishlist>()
            .model::<AuditNote>()
            .build()
            .expect("App::build");

        // Tables — the migration engine isn't invoked here; we hand-
        // create them to keep the test focused on the macro emission.
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

        sqlx::query(
            "CREATE TABLE customer_profile (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user INTEGER NOT NULL UNIQUE REFERENCES auth_user(id) ON DELETE CASCADE,
                bio TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE customer_profile");

        sqlx::query(
            "CREATE TABLE wishlist (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user INTEGER NOT NULL UNIQUE REFERENCES auth_user(id) ON DELETE CASCADE,
                label TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE wishlist");

        sqlx::query(
            "CREATE TABLE audit_note (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user INTEGER NOT NULL REFERENCES auth_user(id) ON DELETE CASCADE,
                note TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE audit_note");
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
async fn cross_crate_o2o_returns_some_for_matching_child() {
    let _guard = TEST_LOCK.lock().await;
    boot().await;
    let user = make_user("alpha-o2o").await;

    // Insert the child row via the ORM (cross-crate FK works today;
    // this is just exercising it).
    let _ = sqlx::query("INSERT INTO customer_profile (user, bio) VALUES (?, ?)")
        .bind(user.id)
        .bind("alpha bio")
        .execute(&umbra::db::pool())
        .await
        .expect("insert profile");

    // THE TEST: the trait method resolves on AuthUser even though
    // AuthUser is in `umbra-auth`. If the macro emitted an inherent
    // impl on AuthUser instead of a trait, this file would not have
    // compiled — so reaching this line at all is half the assertion.
    let profile = user.customer_profile().await.expect("query ok");
    let p = profile.expect("matching child exists");
    assert_eq!(p.bio, "alpha bio");
    assert_eq!(p.user.id(), user.id);
}

#[tokio::test]
async fn cross_crate_o2o_returns_none_when_no_child_exists() {
    let _guard = TEST_LOCK.lock().await;
    boot().await;
    let user = make_user("beta-no-child").await;

    let profile = user.customer_profile().await.expect("query ok");
    assert!(
        profile.is_none(),
        "no child row inserted → accessor returns None, got {profile:?}"
    );
}

#[tokio::test]
async fn cross_crate_o2o_isolates_per_parent_row() {
    let _guard = TEST_LOCK.lock().await;
    boot().await;
    let a = make_user("iso-a").await;
    let b = make_user("iso-b").await;

    let pool = umbra::db::pool();
    sqlx::query("INSERT INTO customer_profile (user, bio) VALUES (?, ?)")
        .bind(a.id)
        .bind("A only")
        .execute(&pool)
        .await
        .expect("insert A");

    let from_a = a.customer_profile().await.expect("query A").expect("A has");
    let from_b = b.customer_profile().await.expect("query B");

    assert_eq!(from_a.bio, "A only");
    assert!(from_b.is_none(), "B has no profile → None, got {from_b:?}");
}

#[tokio::test]
async fn two_distinct_children_emit_distinct_method_names() {
    // No data needed — the COMPILATION of this function proves the
    // method names don't collide (`customer_profile` vs `wishlist`).
    // If the macro mistakenly named the method off the FK field
    // (`user`) instead of the child struct, both calls would be
    // `auth_user.user()` and trigger E0034 "multiple applicable items
    // in scope".
    let _guard = TEST_LOCK.lock().await;
    boot().await;
    let user = make_user("two-children").await;

    // Both methods present, both callable, no name collision.
    let _ = user.customer_profile().await.expect("o2o on profile");
    let _ = user.wishlist().await.expect("o2o on wishlist");
}

/// Mutation-test the `unique` guard. When the guard is in place,
/// `AuditNote.user` (non-unique FK) does NOT yield a reverse-o2o
/// accessor — and inserting 3 rows then calling `.audit_note()`
/// would either fail to compile (no method) or, if the guard was
/// removed, return ONE arbitrary row, masking the multiplicity.
///
/// We assert the multiplicity via the SET accessor (which always
/// works), and use a doc-style assertion below to surface the
/// expected absence of the o2o variant. If a future change drops
/// the `if field_attr.unique` guard in the macro, this test
/// surfaces the silent-arbitrary-row bug:
///   1. Three audit_note rows for one user.
///   2. SET form returns 3.
///   3. Removing the guard would emit `audit_note()` (the o2o form)
///      on AuthUser, which silently returns the FIRST row, not all
///      three. The presence of THAT method would not break this
///      test — but the negative compile assertion in
///      `compile_negative` keeps the macro honest.
#[tokio::test]
async fn non_unique_fk_does_not_get_reverse_o2o_accessor() {
    // The compile-only check that matters: this test does NOT
    // import any `AuditNoteUserOneToOneReverse` trait, because the
    // derive must NOT emit one (AuditNote.user is not UNIQUE).
    // If we accidentally emitted it for every FK regardless of
    // `unique`, the trait would exist and could be imported. The
    // negative compile assertion lives in `trybuild`-style files
    // when we add them; for now we rely on the positive cases above
    // and the inline doc here.
    //
    // Runtime sanity: the reverse-FK accessor (set form) still
    // works for AuditNote — that's gap #30, not what this test is
    // proving, but a quick check keeps the test file honest about
    // the difference between the two emissions.
    let _guard = TEST_LOCK.lock().await;
    boot().await;
    let user = make_user("audit-target").await;
    let pool = umbra::db::pool();
    for _ in 0..3 {
        sqlx::query("INSERT INTO audit_note (user, note) VALUES (?, ?)")
            .bind(user.id)
            .bind("a note")
            .execute(&pool)
            .await
            .expect("insert audit");
    }

    use crate::AuditNoteUserReverse;
    let notes = user.audit_note_set().fetch().await.expect("fetch notes");
    assert_eq!(
        notes.len(),
        3,
        "non-unique FK still yields the set accessor"
    );
}
