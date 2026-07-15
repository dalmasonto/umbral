//! gaps4 #13 — the reverse-FK child list is windowed PER PARENT, not globally.
//!
//! The bug: the batched `WHERE fk IN (parent_ids)` read carried one global
//! `LIMIT MAX_LIMIT`. Two parents whose children together exceed MAX_LIMIT would
//! share that one budget — the first prolific parent consumed it and later
//! parents came back truncated (or empty), even though each parent's own list
//! was well within the limit.
//!
//! This drives the real batched loader path: a LIST of parents, each traversing
//! its children in ONE resolution pass, so `ChildLoader` coalesces them into a
//! single batch. Two authors with 120 posts EACH (240 > MAX_LIMIT of 200): the
//! old global cap returns 200 rows total and starves one author; the per-parent
//! window returns each author's full 120.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, Model};
use umbral_graphql::GraphqlPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(table = "cw_author")]
pub struct CwAuthor {
    pub id: i64,
    pub username: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(table = "cw_post")]
pub struct CwPost {
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<CwAuthor>,
}

const PER_AUTHOR: usize = 120; // 2 authors * 120 = 240 > MAX_LIMIT (200)

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> axum::Router {
    BOOT.get_or_init(|| async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("cw.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let app = umbral::App::builder()
            .settings(umbral::Settings::from_env().expect("settings"))
            .database("default", pool)
            .model::<CwAuthor>()
            .model::<CwPost>()
            .plugin(
                GraphqlPlugin::new()
                    .expose("cw_author")
                    .expose("cw_post"),
            )
            .build()
            .expect("build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let p = umbral::db::pool();
        sqlx::query("INSERT INTO cw_author (id, username) VALUES (1, 'ada'), (2, 'grace')")
            .execute(&p)
            .await
            .expect("seed authors");

        // 120 posts for each author, in one multi-row insert per author.
        for author in [1_i64, 2] {
            let values: Vec<String> = (0..PER_AUTHOR)
                .map(|i| format!("('post {author}-{i}', {author})"))
                .collect();
            let sql = format!(
                "INSERT INTO cw_post (title, author) VALUES {}",
                values.join(", ")
            );
            sqlx::query(&sql).execute(&p).await.expect("seed posts");
        }
        app.into_router()
    })
    .await
    .clone()
}

async fn gql(query: &str) -> serde_json::Value {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let router = boot().await;
    let body = serde_json::json!({ "query": query }).to_string();
    let res = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/graphql")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Each parent gets its OWN full child window — the prolific sibling can't starve
/// the other.
#[tokio::test]
async fn each_parent_gets_its_own_child_window() {
    let _ = boot().await;
    let out = gql(r#"{ cw_authors(limit: 10) { id cw_posts { id } } }"#).await;
    assert!(out.get("errors").is_none(), "{out}");

    let authors = out["data"]["cw_authors"].as_array().expect("authors list");
    assert_eq!(authors.len(), 2, "both authors returned: {out}");

    for a in authors {
        let posts = a["cw_posts"].as_array().expect("posts list");
        assert_eq!(
            posts.len(),
            PER_AUTHOR,
            "author {} must get all {PER_AUTHOR} of its OWN posts, not a slice of a \
             batch-wide limit: {out}",
            a["id"]
        );
    }
}
