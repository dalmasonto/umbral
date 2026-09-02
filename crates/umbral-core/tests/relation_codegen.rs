//! Phase 1, Task 5 — the derive-generated chainable relation accessors.
//!
//! Every assertion here goes through a GENERATED accessor (`<Model>Relations`
//! trait method), never a hand-written `to_one_hop` / `to_many_hop`. That is
//! the whole point of the task: `post.author().await?` and
//! `user.developer().software_groups().software().fetch().await?` compile and
//! round-trip because `#[derive(Model)]` emitted the trait + impls.
//!
//! Behavioral, per the project's testing rule: real rows + real junctions
//! through the actual public accessors, read the object graph back.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, M2M, OneToOne};
use umbral_core::db;

// Task 6 (the prelude glob) is not here yet. No explicit `use` of the generated
// relation traits is needed here, though: `#[derive(Model)]` emits each
// `<M>Relations` trait at the same module scope as the model it is derived on
// (this test module's root), so they are already in scope for the calls below.

// =========================================================================
// Models — one of every forward relation kind.
//   Post   --FK-->            User          (required)
//   Post   --Option<FK>-->    User          (nullable, "reviewer")
//   User   --reverse-O2O-->   Developer     (#[sqlx(skip)] back-link)
//   Developer --unique FK-->  User          (the O2O child side)
//   Developer --M2M-->        SoftwareGroup
//   SoftwareGroup --M2M-->    Software
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rcg_software")]
pub struct Software {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    pub active: bool,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rcg_software_group")]
pub struct SoftwareGroup {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    pub active: bool,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "rcg_software")]
    pub software: M2M<Software>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rcg_user")]
pub struct User {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    /// Parent-side reverse OneToOne — the back-link to the child `Developer`,
    /// carries no DB column. The generated `UserRelations::developer()`
    /// accessor resolves it as a chainable `Relation<Developer>`.
    #[sqlx(skip)]
    #[serde(skip)]
    pub developer: OneToOne<Developer>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rcg_developer")]
pub struct Developer {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    /// Child-side of the OneToOne: a UNIQUE FK pointing back at `User`.
    ///
    /// `#[umbral(no_reverse)]` suppresses the zero-config reverse accessors on
    /// `User` (`user.developer()` returning `Option<Developer>`, and
    /// `user.developer_set()`). `User` declares an explicit parent-side
    /// `developer: OneToOne<Developer>` field, so the chainable Task-5
    /// `UserRelations::developer() -> Relation<Developer>` is the canonical
    /// accessor; without `no_reverse` the two `developer()` methods collide
    /// (ambiguous method call). This is the documented opt-out when you declare
    /// both ends of a OneToOne.
    #[umbral(unique)]
    #[umbral(no_reverse)]
    pub user: ForeignKey<User>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "rcg_software_group")]
    pub software_groups: M2M<SoftwareGroup>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rcg_post")]
pub struct Post {
    #[umbral(primary_key)]
    pub id: i64,
    pub title: String,
    /// Required forward FK.
    pub author: ForeignKey<User>,
    /// Nullable forward FK.
    pub reviewer: Option<ForeignKey<User>>,
}

// =========================================================================
// Harness
// =========================================================================

static BOOT: OnceCell<sqlx::SqlitePool> = OnceCell::const_new();

async fn boot() -> sqlx::SqlitePool {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Software>()
            .model::<SoftwareGroup>()
            .model::<User>()
            .model::<Developer>()
            .model::<Post>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // Users: ada=1, grace=2.
        for name in &["ada", "grace"] {
            sqlx::query("INSERT INTO rcg_user (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed user");
        }
        // Developer for ada (id=1), pointing back at user 1.
        sqlx::query("INSERT INTO rcg_developer (name, user) VALUES (?, ?)")
            .bind("ada-dev")
            .bind(1_i64)
            .execute(&pool)
            .await
            .expect("seed developer");

        // Software groups: core=1, infra=2, archived=3 (inactive).
        for (name, active) in &[("core", true), ("infra", true), ("archived", false)] {
            sqlx::query("INSERT INTO rcg_software_group (name, active) VALUES (?, ?)")
                .bind(*name)
                .bind(*active)
                .execute(&pool)
                .await
                .expect("seed group");
        }
        // Software: editor=1, compiler=2.
        for name in &["editor", "compiler"] {
            sqlx::query("INSERT INTO rcg_software (name, active) VALUES (?, 1)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed software");
        }
        // dev(1) -> core(1), infra(2).
        for child in &[1_i64, 2_i64] {
            sqlx::query(
                "INSERT INTO rcg_developer_software_groups (parent_id, child_id) VALUES (1, ?)",
            )
            .bind(*child)
            .execute(&pool)
            .await
            .expect("seed dev->group");
        }
        // core(1) -> editor(1), compiler(2); infra(2) -> editor(1).
        for (parent, child) in &[(1_i64, 1_i64), (1_i64, 2_i64), (2_i64, 1_i64)] {
            sqlx::query(
                "INSERT INTO rcg_software_group_software (parent_id, child_id) VALUES (?, ?)",
            )
            .bind(*parent)
            .bind(*child)
            .execute(&pool)
            .await
            .expect("seed group->software");
        }
        // Posts: 1 authored by ada, reviewed by grace; 2 authored by grace, no reviewer.
        sqlx::query("INSERT INTO rcg_post (title, author, reviewer) VALUES ('a', 1, 2)")
            .execute(&pool)
            .await
            .expect("seed post 1");
        sqlx::query("INSERT INTO rcg_post (title, author, reviewer) VALUES ('b', 2, NULL)")
            .execute(&pool)
            .await
            .expect("seed post 2");

        pool
    })
    .await
    .clone()
}

async fn fetch_post(title: &str) -> Post {
    Post::objects()
        .filter(post::TITLE.eq(title))
        .get()
        .await
        .expect("fetch post")
}

async fn fetch_user(name: &str) -> User {
    User::objects()
        .filter(user::NAME.eq(name))
        .get()
        .await
        .expect("fetch user")
}

async fn fetch_dev() -> Developer {
    Developer::objects()
        .filter(developer::NAME.eq("ada-dev"))
        .get()
        .await
        .expect("fetch dev")
}

// =========================================================================
// Tests — generated accessors only.
// =========================================================================

/// Object-rooted single hop over a required forward FK.
#[tokio::test]
async fn forward_fk_object_rooted() {
    boot().await;
    let post = fetch_post("a").await;
    let author = post.author().await.expect("author resolves");
    assert_eq!(author.name, "ada");
}

/// A nullable forward FK resolves to `Some` when set and `None` when NULL,
/// via `.get_opt()`.
#[tokio::test]
async fn nullable_fk_returns_option() {
    boot().await;

    let with = fetch_post("a").await;
    let reviewer = with.reviewer().get_opt().await.expect("reviewer query");
    assert_eq!(reviewer.map(|u| u.name), Some("grace".to_string()));

    let without = fetch_post("b").await;
    let none = without
        .reviewer()
        .get_opt()
        .await
        .expect("no-reviewer query");
    assert!(none.is_none(), "post b has a NULL reviewer");
}

/// The forward unique-FK (O2O child side) accessor resolves the parent.
#[tokio::test]
async fn o2o_child_side_resolves_parent() {
    boot().await;
    let dev = fetch_dev().await;
    let user = dev.user().await.expect("dev.user() resolves");
    assert_eq!(user.name, "ada");
}

/// The M2M accessor produces a chainable `QuerySet` — filter + count compose.
#[tokio::test]
async fn m2m_accessor_filters_and_counts() {
    boot().await;
    let dev = fetch_dev().await;

    let active = dev
        .software_groups()
        .filter(software_group::ACTIVE.eq(true))
        .count()
        .await
        .expect("count active groups");
    assert_eq!(
        active, 2,
        "core + infra are active; archived is not attached"
    );

    let names: Vec<String> = dev
        .software_groups()
        .order_by(software_group::NAME.asc())
        .fetch()
        .await
        .expect("fetch groups")
        .into_iter()
        .map(|g| g.name)
        .collect();
    assert_eq!(names, vec!["core", "infra"]);
}

/// The headline: a DEEP MIXED chain of a supported shape —
/// reverse-O2O -> M2M -> M2M — entirely through generated accessors, deduped
/// by leaf PK.
#[tokio::test]
async fn deep_mixed_chain_reverse_o2o_then_m2m_m2m() {
    boot().await;
    let user = fetch_user("ada").await;

    let software = user
        .developer()
        .software_groups()
        .software()
        .order_by(software::NAME.asc())
        .fetch()
        .await
        .expect("user.developer().software_groups().software()");
    let names: Vec<&str> = software.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["compiler", "editor"],
        "editor reachable via two groups appears once by default"
    );
}

/// The reverse-O2O accessor on its own resolves to the child object.
#[tokio::test]
async fn reverse_o2o_object_rooted() {
    boot().await;
    let user = fetch_user("ada").await;
    let dev = user.developer().get_opt().await.expect("developer query");
    assert_eq!(dev.map(|d| d.name), Some("ada-dev".to_string()));
}
