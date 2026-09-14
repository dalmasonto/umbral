//! ORM heavy-relations epic, Plan A Task 2 — `RelPath::from_path::<T>(&str)`.
//!
//! `from_path` resolves a bare `__`-separated relation path string into a
//! `RelPath` — the shape a later `select_related` / aggregate / hydration
//! consumer needs, without composing `to_one_hop`/`to_many_hop` calls by
//! hand. This suite proves every segment kind it must resolve: a forward FK
//! chain, an M2M first segment, a reverse-FK chain (two hops deep), and a
//! loud (never silent) error on an unknown segment.
//!
//! Schema:
//! ```text
//! Company  <--FK(company)--  User  <--FK(author)--  Post(table "posts")  <--FK(post)--  Comment(table "comments")
//! Developer --M2M(software_groups)--> SoftwareGroup
//! ```
//! `Post`'s table is deliberately named the bare plural `"posts"` (and
//! `Comment`'s `"comments"`) so a reverse-FK segment can match the
//! CONVENTIONAL bare-table-name form the auto-discovery scan supports,
//! without needing a declared `#[umbral(reverse_fk = "...")]` field.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, M2M, Model};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rfp_company")]
pub struct Company {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rfp_user")]
pub struct User {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    pub company: ForeignKey<Company>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "posts")]
pub struct Post {
    #[umbral(primary_key)]
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<User>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "comments")]
pub struct Comment {
    #[umbral(primary_key)]
    pub id: i64,
    pub body: String,
    pub post: ForeignKey<Post>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rfp_software_group")]
pub struct SoftwareGroup {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "rfp_developer")]
pub struct Developer {
    #[umbral(primary_key)]
    pub id: i64,
    pub name: String,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "rfp_software_group")]
    pub software_groups: M2M<SoftwareGroup>,
}

// =========================================================================
// Harness — models only need to be REGISTERED (via App::builder) for the
// registry-backed deeper-hop / reverse-FK auto-discovery lookups; no rows
// are seeded because `from_path` never touches the database.
// =========================================================================

static BOOT: OnceCell<sqlx::SqlitePool> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Company>()
            .model::<User>()
            .model::<Post>()
            .model::<Comment>()
            .model::<SoftwareGroup>()
            .model::<Developer>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        pool
    })
    .await;
}

// =========================================================================
// Tests
// =========================================================================

/// A two-hop forward FK chain off the typed root: `Post.author -> User`,
/// `User.company -> Company`.
#[tokio::test]
async fn from_path_resolves_two_hop_fk_chain() {
    boot().await;
    use umbral::orm::relation::{HopKind, PathBase, RelPath};

    let p = RelPath::from_path::<Post>("author__company").expect("resolves");
    match p.base {
        PathBase::TableRoot { table } => assert_eq!(table, Post::TABLE),
        _ => panic!("from_path must root at PathBase::TableRoot"),
    }
    assert_eq!(p.hops.len(), 2);
    assert_eq!(p.hops[0].kind, HopKind::Fk);
    assert_eq!(p.hops[0].to_table, User::TABLE);
    assert_eq!(p.hops[1].kind, HopKind::Fk);
    assert_eq!(p.hops[1].to_table, Company::TABLE);
    assert!(
        p.hops.iter().all(|h| h.fk_on_from),
        "both hops are forward FKs"
    );
}

/// A single M2M segment off the typed root resolves to one `M2M` hop
/// carrying its junction descriptor.
#[tokio::test]
async fn from_path_resolves_m2m_first_segment() {
    boot().await;
    use umbral::orm::relation::{HopKind, RelPath};

    let p = RelPath::from_path::<Developer>("software_groups").expect("resolves");
    assert_eq!(p.hops.len(), 1);
    assert_eq!(p.hops[0].kind, HopKind::M2M);
    assert!(
        p.hops[0].junction.is_some(),
        "M2M hop must carry a junction"
    );
    let j = p.hops[0].junction.unwrap();
    assert_eq!(j.table, "rfp_developer_software_groups");
    assert_eq!(j.parent_column, "parent_id");
    assert_eq!(j.target_column, "child_id");
}

/// A two-hop REVERSE-FK chain, auto-discovered by convention (no declared
/// `#[umbral(reverse_fk = "...")]` field on either `User` or `Post`):
/// `User <- Post` (via `Post.author`), then `Post <- Comment` (via
/// `Comment.post`).
#[tokio::test]
async fn from_path_resolves_reverse_fk_two_hop() {
    boot().await;
    use umbral::orm::relation::{HopKind, RelPath};

    let p = RelPath::from_path::<User>("posts__comments").expect("resolves");
    assert_eq!(p.hops.len(), 2);
    assert_eq!(p.hops[0].kind, HopKind::ReverseFk); // User <- Post
    assert!(!p.hops[0].fk_on_from, "the FK lives on the child (Post)");
    assert_eq!(p.hops[0].to_table, Post::TABLE);
    assert_eq!(p.hops[0].fk_column, "author");
    assert_eq!(p.hops[1].kind, HopKind::ReverseFk); // Post <- Comment
    assert_eq!(p.hops[1].to_table, Comment::TABLE);
    assert_eq!(p.hops[1].fk_column, "post");
}

/// An unknown segment errors loudly, naming the bad segment — never a
/// silently-empty / wrong path.
#[tokio::test]
async fn from_path_unknown_segment_errors_loudly() {
    boot().await;
    use umbral::orm::relation::RelPath;

    let err = RelPath::from_path::<Post>("athor").unwrap_err(); // typo of "author"
    assert!(
        err.to_string().contains("athor"),
        "error names the bad segment: {err}"
    );
}
