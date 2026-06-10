//! `QuerySet::annotate_count` — Django's
//! `Post.objects.filter(...).annotate(n=Count("comments"))` in ONE
//! query: every parent row arrives with its related-row count via a
//! correlated `COUNT(*)` subquery, never a count query per row.
//!
//! Pins: the reverse relation resolves through
//! `Model::REVERSE_FK_RELATIONS`, the count composes with `.filter()`
//! on the parent, zero-children parents report 0 (LEFT-JOIN-miss
//! semantics), and an unknown field name fails loudly.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::{ForeignKey, ReverseSet};
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "anc_comment")]
pub struct Comment {
    pub id: i64,
    pub body: String,
    pub post: ForeignKey<Post>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "anc_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
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
            "CREATE TABLE anc_post (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE anc_post");
        sqlx::query(
            "CREATE TABLE anc_comment (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                body TEXT NOT NULL,
                post INTEGER NOT NULL REFERENCES anc_post(id)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE anc_comment");

        for title in ["alpha", "beta", "gamma"] {
            sqlx::query("INSERT INTO anc_post (title) VALUES (?)")
                .bind(title)
                .execute(&pool)
                .await
                .expect("seed post");
        }
        // alpha (id 1): two comments, beta (id 2): one, gamma: none.
        for (body, post) in [("a1", 1), ("a2", 1), ("b1", 2)] {
            sqlx::query("INSERT INTO anc_comment (body, post) VALUES (?, ?)")
                .bind(body)
                .bind(post)
                .execute(&pool)
                .await
                .expect("seed comment");
        }
    })
    .await;
}

#[tokio::test]
async fn counts_arrive_with_the_rows_in_one_query() {
    boot().await;
    let rows: Vec<(Post, i64)> = Post::objects()
        .annotate_count("comment_set")
        .await
        .expect("annotate_count");
    assert_eq!(rows.len(), 3);
    let by_title: std::collections::HashMap<String, i64> =
        rows.into_iter().map(|(p, n)| (p.title, n)).collect();
    assert_eq!(by_title["alpha"], 2);
    assert_eq!(by_title["beta"], 1);
    assert_eq!(
        by_title["gamma"], 0,
        "no children must mean 0, not a missing row"
    );
}

#[tokio::test]
async fn composes_with_parent_filters() {
    boot().await;
    let rows: Vec<(Post, i64)> = Post::objects()
        .filter(post::TITLE.eq("alpha"))
        .annotate_count("comment_set")
        .await
        .expect("filtered annotate_count");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.title, "alpha");
    assert_eq!(rows[0].1, 2);
}

#[tokio::test]
async fn unknown_relation_fails_loudly() {
    boot().await;
    let err = Post::objects()
        .annotate_count("nope_set")
        .await
        .expect_err("unknown relation must error");
    let msg = err.to_string();
    assert!(
        msg.contains("nope_set") && msg.contains("comment_set"),
        "error names the bad field and the valid ones: {msg}"
    );
}
