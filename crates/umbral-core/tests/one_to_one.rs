//! `OneToOne<C>` — attribute-free reverse OneToOne accessor on a
//! parent model. Mirrors the Django pattern `user.profile.avatar`
//! where every user has at most one profile (enforced by the
//! child's UNIQUE FK).
//!
//! These tests pin:
//!   - The happy path: `.prefetch_related("profile")` populates
//!     `user.profile.resolved()` to `Some(&Profile)`.
//!   - The "loaded but no match" path: parent without a profile
//!     gets `resolved() == None` AND `is_loaded() == true`.
//!   - Auto FK discovery: no `#[umbral(reverse_one = "...")]` etc.
//!     was used; the macro found the back-link via runtime
//!     introspection of Profile's FIELDS.
//!   - Cross-parent isolation (mirrors the ReverseSet
//!     no-contamination test): each user gets only its own
//!     profile.
//!   - Template-shaped JSON: serialised parent emits a nested
//!     profile object (or `null`), so a Jinja template can write
//!     `{{ user.profile.avatar }}` once the parent has been
//!     prefetched.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, OneToOne};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "oto_user")]
pub struct User {
    pub id: i64,
    pub username: String,
    /// Reverse OneToOne — populated by
    /// `.prefetch_related("profile")`. No umbral attribute needed:
    /// the macro discovers Profile's back-pointing UNIQUE FK at
    /// runtime.
    #[sqlx(skip)]
    pub profile: OneToOne<Profile>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "oto_profile")]
pub struct Profile {
    pub id: i64,
    /// `#[umbral(unique)]` is what makes the reverse side OneToOne
    /// (vs OneToMany). The OneToOne macro's runtime FK lookup
    /// requires the UNIQUE attribute — without it the lookup
    /// finds no candidates and surfaces a loud error.
    #[umbral(unique)]
    pub user: ForeignKey<User>,
    pub avatar: String,
    pub bio: String,
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
            .model::<User>()
            .model::<Profile>()
            .build()
            .expect("App::build");

        sqlx::query(
            "CREATE TABLE oto_user (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE user");
        sqlx::query(
            "CREATE TABLE oto_profile (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user INTEGER NOT NULL UNIQUE REFERENCES oto_user(id),
                avatar TEXT NOT NULL,
                bio TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE profile");

        // alice has a profile; bob doesn't. carol exists to test
        // that the loader handles the "no match" case AND populates
        // is_loaded() correctly.
        for username in &["alice", "bob", "carol"] {
            sqlx::query("INSERT INTO oto_user (username) VALUES (?)")
                .bind(*username)
                .execute(&pool)
                .await
                .expect("seed user");
        }
        // alice (id=1) gets a profile; bob (id=2) and carol (id=3) don't.
        sqlx::query("INSERT INTO oto_profile (user, avatar, bio) VALUES (?, ?, ?)")
            .bind(1_i64)
            .bind("alice.png")
            .bind("hello from alice")
            .execute(&pool)
            .await
            .expect("seed profile");
    })
    .await;
}

fn by_username(users: &[User]) -> std::collections::HashMap<&str, &User> {
    users.iter().map(|u| (u.username.as_str(), u)).collect()
}

#[tokio::test]
async fn prefetch_related_populates_one_to_one_for_matching_parent() {
    boot().await;
    let users = User::objects()
        .prefetch_related("profile")
        .fetch()
        .await
        .expect("fetch");
    let by = by_username(&users);
    let alice = by.get("alice").expect("alice");
    let profile = alice
        .profile
        .resolved()
        .expect("alice has a profile, should be resolved");
    assert_eq!(profile.avatar, "alice.png");
    assert_eq!(profile.bio, "hello from alice");
    assert!(alice.profile.is_loaded(), "is_loaded() must flip true");
}

#[tokio::test]
async fn one_to_one_no_match_is_loaded_but_resolved_is_none() {
    boot().await;
    let users = User::objects()
        .prefetch_related("profile")
        .fetch()
        .await
        .expect("fetch");
    let by = by_username(&users);
    let bob = by.get("bob").expect("bob");
    assert!(
        bob.profile.resolved().is_none(),
        "bob has no profile → resolved() = None"
    );
    assert!(
        bob.profile.is_loaded(),
        "but is_loaded() = true so callers can tell \"loaded, no row\" from \
         \"not loaded\""
    );
}

#[tokio::test]
async fn without_prefetch_one_to_one_resolved_is_none_and_not_loaded() {
    boot().await;
    let users = User::objects().fetch().await.expect("fetch");
    for u in &users {
        assert!(u.profile.resolved().is_none());
        assert!(
            !u.profile.is_loaded(),
            "no prefetch → is_loaded() stays false (lets template-rendering \
             code distinguish unhydrated from empty)"
        );
    }
}

#[tokio::test]
async fn one_to_one_does_not_contaminate_across_parents() {
    boot().await;
    let users = User::objects()
        .prefetch_related("profile")
        .fetch()
        .await
        .expect("fetch");
    let by = by_username(&users);
    // Strict membership: alice's profile must NOT show up on bob
    // or carol.
    assert!(by["alice"].profile.resolved().is_some());
    assert!(by["bob"].profile.resolved().is_none());
    assert!(by["carol"].profile.resolved().is_none());

    // And alice's avatar value is hers alone — sanity check the
    // bucket-by-pk grouping picked the right row.
    assert_eq!(by["alice"].profile.resolved().unwrap().avatar, "alice.png");
}

#[tokio::test]
async fn template_shaped_json_emits_nested_profile_or_null() {
    boot().await;
    let users = User::objects()
        .prefetch_related("profile")
        .fetch()
        .await
        .expect("fetch");
    let by = by_username(&users);

    // alice: resolved → nested object reachable via JSON dotted access.
    let alice_json = serde_json::to_value(by["alice"]).expect("serialize");
    let alice_obj = alice_json.as_object().expect("obj");
    let alice_profile = alice_obj
        .get("profile")
        .and_then(|v| v.as_object())
        .expect("alice.profile is a nested object");
    assert_eq!(
        alice_profile.get("avatar").and_then(|v| v.as_str()),
        Some("alice.png"),
        "{{ user.profile.avatar }} resolves cleanly in templates"
    );

    // bob: no profile → key is null OR omitted (skip_serializing_if).
    let bob_json = serde_json::to_value(by["bob"]).expect("serialize");
    let bob_obj = bob_json.as_object().expect("obj");
    let profile_value = bob_obj.get("profile");
    assert!(
        profile_value.is_none() || profile_value.unwrap().is_null(),
        "bob's profile must be null or absent: {profile_value:?}"
    );
}

#[tokio::test]
async fn select_related_through_forward_side_works_alongside_one_to_one() {
    boot().await;
    // The FORWARD side (Profile → User) has always worked via
    // `select_related("user")` on Profile. Confirm both directions
    // still compose: load profiles with their users hydrated.
    let profiles = Profile::objects()
        .select_related("user")
        .fetch()
        .await
        .expect("fetch");
    assert_eq!(profiles.len(), 1, "only alice has a profile");
    let u = profiles[0]
        .user
        .resolved()
        .expect("user hydrated via select_related");
    assert_eq!(u.username, "alice");
}

#[tokio::test]
async fn loud_error_on_unknown_field_still_mentions_one_to_one() {
    boot().await;
    let err = User::objects()
        .prefetch_related("not_a_relation")
        .fetch()
        .await
        .expect_err("typo must error");
    let msg = err.to_string();
    assert!(
        msg.contains("not_a_relation"),
        "error must name the bad field: {msg}"
    );
    assert!(
        msg.contains("OneToOne"),
        "error must enumerate OneToOne as one of the searched relation kinds: {msg}"
    );
}
