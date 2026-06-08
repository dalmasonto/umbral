//! Gap #44 end-to-end — `prefetch_related("comment_set")` on a
//! parent with a `ReverseSet<Comment>` field loads every comment
//! pointing back at each post in one batched query.
//!
//! Pins: macro recognizes `#[umbra(reverse_fk = "...")]`, the
//! parent's `set_m2m_parent_ids` (renamed concept — now covers both
//! M2M and ReverseSet) wires `parent_id` + `fk_column`, the prefetch
//! dispatch finds the spec in `REVERSE_FK_RELATIONS`, runs one
//! batched IN, and the per-field arm in `set_reverse_fk_resolved_json`
//! populates each parent's `ReverseSet`.
//!
//! Query budget: 1 (parents) + 1 (children) — no N+1.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::{ForeignKey, ReverseSet};
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "rfk_comment")]
pub struct Comment {
    pub id: i64,
    pub body: String,
    pub post: ForeignKey<Post>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "rfk_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    /// The macro recognises this as a `ReverseSet<Comment>` field
    /// and skips it from the FromRow column list (hence
    /// `#[sqlx(skip)]`) + the Serialize-by-default shape (hence
    /// `#[serde(skip)]`). The `reverse_fk = "post"` attribute names
    /// the FK column on `Comment` that points back.
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbra(reverse_fk = "post")]
    pub comment_set: ReverseSet<Comment>,
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
            .model::<Post>()
            .model::<Comment>()
            .build()
            .expect("App::build");

        sqlx::query(
            "CREATE TABLE rfk_post (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE rfk_post");
        sqlx::query(
            "CREATE TABLE rfk_comment (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                body TEXT NOT NULL,
                post INTEGER NOT NULL REFERENCES rfk_post(id)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE rfk_comment");

        // alpha (1): 2 comments
        // beta  (2): 1 comment
        // gamma (3): 0 comments
        for title in &["alpha", "beta", "gamma"] {
            sqlx::query("INSERT INTO rfk_post (title) VALUES (?)")
                .bind(*title)
                .execute(&pool)
                .await
                .expect("seed post");
        }
        for (body, post) in &[
            ("first on alpha", 1_i64),
            ("second on alpha", 1),
            ("first on beta", 2),
        ] {
            sqlx::query("INSERT INTO rfk_comment (body, post) VALUES (?, ?)")
                .bind(*body)
                .bind(*post)
                .execute(&pool)
                .await
                .expect("seed comment");
        }
    })
    .await;
}

#[tokio::test]
async fn prefetch_related_populates_reverse_set_for_each_parent() {
    boot().await;
    let posts = Post::objects()
        .prefetch_related("comment_set")
        .fetch()
        .await
        .expect("fetch");

    // Index by title so test parallelism (the boot is shared) doesn't
    // collapse the assertions if other tests add posts.
    let by_title: std::collections::HashMap<&str, &Post> =
        posts.iter().map(|p| (p.title.as_str(), p)).collect();

    let alpha = by_title.get("alpha").expect("alpha present");
    let alpha_comments = alpha
        .comment_set
        .resolved()
        .expect("ReverseSet hydrated post-prefetch");
    assert_eq!(alpha_comments.len(), 2, "alpha has 2 comments");
    let bodies: Vec<&str> = alpha_comments.iter().map(|c| c.body.as_str()).collect();
    assert!(bodies.contains(&"first on alpha"));
    assert!(bodies.contains(&"second on alpha"));

    let beta = by_title.get("beta").expect("beta present");
    let beta_comments = beta.comment_set.resolved().expect("hydrated");
    assert_eq!(beta_comments.len(), 1);
    assert_eq!(beta_comments[0].body, "first on beta");

    let gamma = by_title.get("gamma").expect("gamma present");
    let gamma_comments = gamma.comment_set.resolved().expect("hydrated (empty)");
    assert!(
        gamma_comments.is_empty(),
        "gamma has no children → resolved is Some(&[])"
    );
}

#[tokio::test]
async fn without_prefetch_reverse_set_resolved_is_none() {
    boot().await;
    let posts = Post::objects().fetch().await.expect("fetch");
    for p in &posts {
        // Without .prefetch_related("comment_set"), every post's
        // ReverseSet stays unloaded.
        assert!(
            p.comment_set.resolved().is_none(),
            "unloaded ReverseSet must read as None"
        );
    }
}

#[tokio::test]
async fn loud_error_on_unknown_prefetch_field_naming_reverse_set() {
    boot().await;
    let err = Post::objects()
        .prefetch_related("no_such_field")
        .fetch()
        .await
        .expect_err("unknown field must error");
    let msg = err.to_string();
    assert!(
        msg.contains("no_such_field"),
        "error names the bad field: {msg}"
    );
}
