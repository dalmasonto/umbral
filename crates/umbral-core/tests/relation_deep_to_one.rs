//! Phase 1, Task 2 — all-to-one **multi-hop** relation resolution.
//!
//! Task 1 resolved a single forward-FK hop via a subquery. This suite proves
//! a *deep* to-one chain — `Post.author` (FK) → `Author.company` (FK) →
//! `Company.owner` (FK) — resolves to the leaf `User` in **one** JOIN query,
//! and that a NULL link anywhere along the chain surfaces as `Ok(None)` (never
//! a wrong row).
//!
//! Behavioral: real rows through the actual public accessor, read the row
//! back. The `to_sql()` statement-count probe rides ALONGSIDE the round-trip,
//! never in place of it.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::ForeignKey;
use umbral::orm::relation::{HopKind, HopSpec, Relation, to_one_hop};
use umbral_core::db;

// =========================================================================
// Model declarations — a 3-hop all-to-one chain.
//   Post ──author──▶ Author ──company(nullable)──▶ Company ──owner──▶ User
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "deep_user")]
pub struct User {
    #[umbral(primary_key)]
    pub id: i64,
    pub email: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "deep_company")]
pub struct Company {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    pub owner: ForeignKey<User>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "deep_author")]
pub struct Author {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    // Nullable middle FK: an author may have no company.
    pub company: Option<ForeignKey<Company>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "deep_post")]
pub struct Post {
    #[umbral(primary_key)]
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<Author>,
}

// =========================================================================
// Harness — one boot, deterministic seed.
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
            .model::<User>()
            .model::<Company>()
            .model::<Author>()
            .model::<Post>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // boss(1) owns Acme(1); ada(1) works at Acme; nemo(2) has no company.
        sqlx::query("INSERT INTO deep_user (email) VALUES (?)")
            .bind("boss@acme.test")
            .execute(&pool)
            .await
            .expect("seed user");
        sqlx::query("INSERT INTO deep_company (name, owner) VALUES (?, ?)")
            .bind("Acme")
            .bind(1_i64)
            .execute(&pool)
            .await
            .expect("seed company");
        sqlx::query("INSERT INTO deep_author (name, company) VALUES (?, ?)")
            .bind("ada")
            .bind(Some(1_i64))
            .execute(&pool)
            .await
            .expect("seed author ada");
        sqlx::query("INSERT INTO deep_author (name, company) VALUES (?, ?)")
            .bind("nemo")
            .bind(Option::<i64>::None)
            .execute(&pool)
            .await
            .expect("seed author nemo");
        // p1 -> ada (full chain), p2 -> nemo (null middle).
        sqlx::query("INSERT INTO deep_post (title, author) VALUES (?, ?)")
            .bind("Hello")
            .bind(1_i64)
            .execute(&pool)
            .await
            .expect("seed post p1");
        sqlx::query("INSERT INTO deep_post (title, author) VALUES (?, ?)")
            .bind("Orphan")
            .bind(2_i64)
            .execute(&pool)
            .await
            .expect("seed post p2");

        pool
    })
    .await
    .clone()
}

fn author_hop() -> HopSpec {
    HopSpec {
        kind: HopKind::Fk,
        from_table: "deep_post",
        to_table: "deep_author",
        fk_column: "author",
        fk_on_from: true,
        required: true,
        junction: None,
    }
}

fn company_hop() -> HopSpec {
    HopSpec {
        kind: HopKind::Fk,
        from_table: "deep_author",
        to_table: "deep_company",
        fk_column: "company",
        fk_on_from: true,
        // Nullable FK => not required.
        required: false,
        junction: None,
    }
}

fn owner_hop() -> HopSpec {
    HopSpec {
        kind: HopKind::Fk,
        from_table: "deep_company",
        to_table: "deep_user",
        fk_column: "owner",
        fk_on_from: true,
        required: true,
        junction: None,
    }
}

// =========================================================================
// Tests
// =========================================================================

/// A three-hop all-to-one chain resolves to the leaf `User` in a SINGLE JOIN
/// query. The `to_sql()` probe rides alongside the real round-trip.
#[tokio::test]
async fn three_hop_to_one_resolves_in_one_join_query() {
    let pool = boot().await;
    let post = Post::objects()
        .filter(post::TITLE.eq("Hello"))
        .on(&pool)
        .get()
        .await
        .expect("get post");

    // Explicit nested build (Task-5 accessors don't exist yet):
    //   to_one_hop(to_one_hop(to_one_hop(&post, author), company), owner)
    let rel: Relation<User> = to_one_hop::<Company, User>(
        to_one_hop::<Author, Company>(
            to_one_hop::<Post, Author>(&post, author_hop()),
            company_hop(),
        ),
        owner_hop(),
    )
    .on(&pool);

    // Probe: exactly ONE SELECT, exactly THREE JOINs (one per hop).
    let sql = rel.to_sql().expect("build to_sql");
    assert_eq!(
        sql.matches("SELECT").count(),
        1,
        "an all-to-one chain must emit ONE flat SELECT, not nested subqueries: {sql}"
    );
    assert_eq!(
        sql.matches("JOIN").count(),
        3,
        "a 3-hop chain must emit exactly three JOINs: {sql}"
    );

    // Round-trip: the real leaf row comes back.
    let owner: User = rel.get().await.expect("resolve owner");
    assert_eq!(owner.email, "boss@acme.test");
    assert_eq!(owner.id, 1);
}

/// A NULL link in the middle of the chain (`author.company IS NULL`) yields
/// `Ok(None)` from `get_opt()` — never a wrong row.
#[tokio::test]
async fn nullable_middle_fk_yields_none() {
    let pool = boot().await;
    let post = Post::objects()
        .filter(post::TITLE.eq("Orphan"))
        .on(&pool)
        .get()
        .await
        .expect("get orphan post");

    let owner: Option<User> = to_one_hop::<Company, User>(
        to_one_hop::<Author, Company>(
            to_one_hop::<Post, Author>(&post, author_hop()),
            company_hop(),
        ),
        owner_hop(),
    )
    .on(&pool)
    .get_opt()
    .await
    .expect("get_opt must not error on a NULL middle link");
    assert!(
        owner.is_none(),
        "a NULL middle FK must resolve to None, not a wrong row"
    );
}

/// The same NULL-middle chain through `get()` (required terminal) is an
/// `Err`, never a silent `None`.
#[tokio::test]
async fn nullable_middle_fk_get_is_error() {
    let pool = boot().await;
    let post = Post::objects()
        .filter(post::TITLE.eq("Orphan"))
        .on(&pool)
        .get()
        .await
        .expect("get orphan post");

    let result: Result<User, _> = to_one_hop::<Company, User>(
        to_one_hop::<Author, Company>(
            to_one_hop::<Post, Author>(&post, author_hop()),
            company_hop(),
        ),
        owner_hop(),
    )
    .on(&pool)
    .get()
    .await;
    assert!(
        result.is_err(),
        "a broken chain through the required `.get()` must surface as Err"
    );
}
