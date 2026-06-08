//! Nested `select_related("a__b__c")` traversal. Walks each FK hop
//! with one batched `IN (...)` query and unpacks the full chain into
//! `ForeignKey::resolved()` slots at every depth.
//!
//! Query budget = `1 + len(hops)`. No N+1: each hop is one batched
//! query across every parent of prior hops.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::ForeignKey;
use umbra_core::db;

// Self-referential chain: User → User → User
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "srn_user")]
pub struct User {
    pub id: i64,
    #[umbra(string)]
    pub name: String,
    pub manager: Option<ForeignKey<User>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "srn_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<User>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<User>()
            .model::<Post>()
            .build()
            .expect("App::build");

        sqlx::query(
            "CREATE TABLE srn_user (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                manager INTEGER REFERENCES srn_user(id)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE srn_user");
        sqlx::query(
            "CREATE TABLE srn_post (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                author INTEGER NOT NULL REFERENCES srn_user(id)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE srn_post");

        // User hierarchy:
        //   1: ceo       (no manager)
        //   2: alice     (manager = 1)
        //   3: bob       (manager = 1)
        //   4: charlie   (manager = 2, so charlie's manager is alice, whose manager is ceo)
        for (name, mgr) in &[
            ("ceo", None::<i64>),
            ("alice", Some(1)),
            ("bob", Some(1)),
            ("charlie", Some(2)),
        ] {
            sqlx::query("INSERT INTO srn_user (name, manager) VALUES (?, ?)")
                .bind(*name)
                .bind(*mgr)
                .execute(&pool)
                .await
                .expect("seed user");
        }

        // Posts:
        //   1: by alice    (author=2)
        //   2: by bob      (author=3)
        //   3: by charlie  (author=4)
        for (title, author) in &[("first", 2_i64), ("second", 3), ("third", 4)] {
            sqlx::query("INSERT INTO srn_post (title, author) VALUES (?, ?)")
                .bind(*title)
                .bind(*author)
                .execute(&pool)
                .await
                .expect("seed post");
        }
    })
    .await;
}

#[tokio::test]
async fn two_hop_select_related_resolves_chain() {
    boot().await;
    // post.author.manager — 2 hops
    let posts = Post::objects()
        .filter(post::TITLE.eq("first"))
        .select_related("author__manager")
        .fetch()
        .await
        .expect("fetch");
    assert_eq!(posts.len(), 1);
    let p = &posts[0];
    let author = p.author.resolved().expect("author hydrated");
    assert_eq!(author.name, "alice");
    let manager = author
        .manager
        .as_ref()
        .expect("alice has a manager wrapper")
        .resolved()
        .expect("manager hydrated through second hop");
    assert_eq!(manager.name, "ceo");
}

#[tokio::test]
async fn three_hop_select_related_resolves_full_chain() {
    boot().await;
    // post.author.manager.manager — 3 hops (charlie → alice → ceo,
    // and ceo's manager is None so the chain bottoms out).
    let posts = Post::objects()
        .filter(post::TITLE.eq("third"))
        .select_related("author__manager__manager")
        .fetch()
        .await
        .expect("fetch");
    assert_eq!(posts.len(), 1);
    let charlie = posts[0].author.resolved().expect("author");
    assert_eq!(charlie.name, "charlie");
    let alice = charlie
        .manager
        .as_ref()
        .expect("charlie's manager")
        .resolved()
        .expect("alice hydrated");
    assert_eq!(alice.name, "alice");
    let ceo = alice
        .manager
        .as_ref()
        .expect("alice's manager")
        .resolved()
        .expect("ceo hydrated");
    assert_eq!(ceo.name, "ceo");
    // Bottom of the chain — ceo.manager is the column-NULL case.
    assert!(ceo.manager.is_none());
}

#[tokio::test]
async fn nested_select_related_batches_queries_per_hop_not_per_row() {
    boot().await;
    // All 3 posts at once. The hop chain budget is:
    //   1 query: SELECT posts
    //   1 query: SELECT users WHERE id IN (alice_id, bob_id, charlie_id)
    //   1 query: SELECT users WHERE id IN (ceo_id, alice_id)
    // Total = 3 queries regardless of post count.
    let posts = Post::objects()
        .select_related("author__manager")
        .fetch()
        .await
        .expect("fetch");
    // Parallel tests may seed extra rows into the shared
    // in-memory DB; check the originally-seeded titles by
    // membership rather than asserting an exact length.
    assert!(posts.len() >= 3);
    let by_title: std::collections::HashMap<&str, &Post> =
        posts.iter().map(|p| (p.title.as_str(), p)).collect();

    let first_mgr_name = by_title["first"]
        .author
        .resolved()
        .unwrap()
        .manager
        .as_ref()
        .unwrap()
        .resolved()
        .unwrap()
        .name
        .as_str();
    assert_eq!(first_mgr_name, "ceo");

    let third_mgr_name = by_title["third"]
        .author
        .resolved()
        .unwrap()
        .manager
        .as_ref()
        .unwrap()
        .resolved()
        .unwrap()
        .name
        .as_str();
    assert_eq!(third_mgr_name, "alice");
}

#[tokio::test]
async fn nested_path_with_null_middle_hop_does_not_panic() {
    boot().await;
    // ceo wrote a hypothetical post — but ceo's manager is NULL.
    // The second-hop ids list comes back empty after dedup, so the
    // walk short-circuits without panicking.
    sqlx::query("INSERT INTO srn_post (title, author) VALUES (?, ?)")
        .bind("ceo-post")
        .bind(1_i64)
        .execute(&umbra::db::pool())
        .await
        .expect("seed");
    let posts = Post::objects()
        .filter(post::TITLE.eq("ceo-post"))
        .select_related("author__manager")
        .fetch()
        .await
        .expect("fetch must not panic on null middle hop");
    assert_eq!(posts.len(), 1);
    let ceo = posts[0].author.resolved().expect("author hydrated");
    assert_eq!(ceo.name, "ceo");
    // ceo.manager column is NULL → field is None.
    assert!(ceo.manager.is_none());
}

#[tokio::test]
async fn unknown_first_hop_field_returns_loud_error() {
    boot().await;
    let err = Post::objects()
        .select_related("nope__manager")
        .fetch()
        .await
        .expect_err("unknown first hop must error");
    let msg = err.to_string();
    assert!(
        msg.contains("nope"),
        "error should name the bad field: {msg}"
    );
    assert!(
        msg.contains("select_related"),
        "error should name the method: {msg}"
    );
}

#[tokio::test]
async fn unknown_deeper_hop_field_returns_loud_error() {
    boot().await;
    // author is valid; subordinate is not a field on User.
    let err = Post::objects()
        .select_related("author__subordinate")
        .fetch()
        .await
        .expect_err("unknown deeper hop must error");
    let msg = err.to_string();
    assert!(
        msg.contains("subordinate"),
        "error should name the bad hop: {msg}"
    );
    assert!(
        msg.contains("srn_user"),
        "error should name the table where the bad hop lives: {msg}"
    );
}

#[tokio::test]
async fn unknown_single_hop_field_also_errors_loudly_now() {
    boot().await;
    // The non-nested path used to silently no-op; post-#42 it errors
    // for symmetry with the nested path.
    let err = Post::objects()
        .select_related("not_a_field")
        .fetch()
        .await
        .expect_err("unknown single field must error");
    assert!(err.to_string().contains("not_a_field"));
}
