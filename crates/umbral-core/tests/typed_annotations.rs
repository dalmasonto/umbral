//! Typed annotations + ordering by them (gaps3 #29, finding 3).
//!
//! The aggregation engine was already built — `annotate`, `annotate_count`,
//! `aggregate` all existed. It just wasn't *reachable* from a handler:
//!
//! - `annotate` returned `Vec<serde_json::Value>` and `fetch_annotated` returned
//!   `(T, Map<String, Value>)`, so a flat row meant hand-poking JSON per field;
//! - and you **could not `ORDER BY` an annotation at all**, so "the top N authors
//!   *by post count*" — the single most common aggregate query there is — was
//!   impossible to express.
//!
//! A live consumer's leaderboard consequently pulled whole tables into `HashMap`s
//! and sorted in Rust. This is that leaderboard, written the way it should be.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ta_author")]
pub struct TaAuthor {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ta_post")]
pub struct TaPost {
    pub id: i64,
    pub author: umbral::orm::ForeignKey<TaAuthor>,
    pub title: String,
}

/// The flat row a handler actually wants — model columns AND the computed count,
/// in one struct.
#[derive(Debug, Deserialize)]
struct Leader {
    name: String,
    ta_post_set_count: i64,
}

async fn boot() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ta.sqlite");
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
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<TaAuthor>()
            .model::<TaPost>()
            .build()
            .expect("App::build");
        let pool = umbral::db::pool();
        for ddl in [
            "CREATE TABLE ta_author (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
            "CREATE TABLE ta_post (id INTEGER PRIMARY KEY AUTOINCREMENT, author INTEGER NOT NULL, title TEXT NOT NULL)",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }
        // ada: 3 posts, bob: 1, cleo: 0 — a deliberate tie-free ordering.
        sqlx::query("INSERT INTO ta_author (name) VALUES ('ada'),('bob'),('cleo')")
            .execute(&pool)
            .await
            .expect("authors");
        sqlx::query("INSERT INTO ta_post (author, title) VALUES (1,'a'),(1,'b'),(1,'c'),(2,'d')")
            .execute(&pool)
            .await
            .expect("posts");
    })
    .await;
}

/// **The query that was impossible.** Top authors *by post count*, typed.
#[tokio::test]
async fn order_by_an_annotation_and_get_typed_rows() {
    boot().await;

    let top: Vec<Leader> = TaAuthor::objects()
        .annotate_count("ta_post_set")
        .order_by_annotation("ta_post_set_count", true) // desc
        .fetch_annotated_as::<Leader>()
        .await
        .expect("leaderboard");

    let names: Vec<&str> = top.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["ada", "bob", "cleo"],
        "rows must come back ordered BY THE COUNT, descending — this is the query \
         the ORM could not express, and why a consumer sorted in Rust instead",
    );
    assert_eq!(top[0].ta_post_set_count, 3, "ada wrote 3");
    assert_eq!(
        top[2].ta_post_set_count, 0,
        "cleo wrote none, and still appears"
    );
}

/// Ascending works too — and the count is a real SQL sort, not a Rust one, so it
/// composes with `limit`.
#[tokio::test]
async fn ordering_ascending_composes_with_limit() {
    boot().await;

    let bottom: Vec<Leader> = TaAuthor::objects()
        .annotate_count("ta_post_set")
        .order_by_annotation("ta_post_set_count", false)
        .limit(1)
        .fetch_annotated_as::<Leader>()
        .await
        .expect("query");

    assert_eq!(bottom.len(), 1, "LIMIT applied in SQL, after the sort");
    assert_eq!(bottom[0].name, "cleo", "fewest posts first");
}

/// A typo'd alias must fail loudly. Silently emitting `ORDER BY post_count` when
/// no such column exists — or worse, matching a real column and returning
/// confidently wrong rows — is the failure mode worth preventing.
#[tokio::test]
async fn ordering_by_an_unknown_annotation_is_a_loud_error() {
    boot().await;

    let err = TaAuthor::objects()
        .annotate_count("ta_post_set")
        .order_by_annotation("ta_post_set_kount", true) // typo
        .fetch_annotated_as::<Leader>()
        .await
        .expect_err("a typo'd alias must not silently produce wrong SQL");

    let msg = err.to_string();
    assert!(
        msg.contains("ta_post_set_kount") && msg.contains("ta_post_set_count"),
        "the error must name the bad alias AND the ones that exist; got: {msg}",
    );
}

/// The GROUP BY rollup, typed — `annotate_as` instead of decoding `Vec<Value>`.
#[tokio::test]
async fn annotate_as_returns_typed_rollup_rows() {
    boot().await;

    #[derive(Debug, Deserialize)]
    struct ByAuthor {
        author: i64,
        posts: i64,
    }

    let mut rows: Vec<ByAuthor> = TaPost::objects()
        .annotate_as::<ByAuthor>(&["author"], &[("posts", umbral::orm::Aggregate::count())])
        .await
        .expect("rollup");
    rows.sort_by_key(|r| r.author);

    assert_eq!(rows.len(), 2, "two authors have posts");
    assert_eq!((rows[0].author, rows[0].posts), (1, 3));
    assert_eq!((rows[1].author, rows[1].posts), (2, 1));
}
