//! gaps4 #77 — the reusable ORM-layer nested-tree writer
//! (`umbral::orm::nested`) is callable with NO REST plugin / HTTP request.
//!
//! These tests exercise the public writer the way a CLI seeder / AI-agent
//! object-graph seeder would: hand ONE nested JSON document to ONE call and
//! get the parent + its declared reverse-FK children (FKs auto-filled) + any
//! declared M2M links written on ONE transaction — and, on a child failure,
//! the whole tree rolls back so the PARENT row is not persisted.

#![allow(dead_code)]

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::{Mutex as TokioMutex, OnceCell};

use umbral::migrate::{ModelMeta, registered_models};
use umbral::orm::ForeignKey;
use umbral::orm::M2M;
use umbral::orm::nested::{NestedSpec, write_nested_tree};
use umbral_core::db;

/// Serialise the tests: they share one file-backed DB and its tables.
fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

// ── models ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nw_tag")]
pub struct Tag {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nw_author")]
pub struct Author {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nw_post")]
pub struct Post {
    pub id: i64,
    #[umbral(string)]
    pub title: String,
    /// Reverse-FK link to the parent Author. The writer auto-fills this from
    /// the parent's just-inserted PK, so the seed document never repeats it.
    pub author: ForeignKey<Author>,
    /// M2M to Tag — junction `nw_post_tags`. The writer drives this from ids
    /// carried in the same nested document.
    #[sqlx(skip)]
    #[umbral(m2m = "nw_tag")]
    pub tags: M2M<Tag>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let dir = std::env::temp_dir();
        let path = dir.join(format!("umbral_nested_writer_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Tag>()
            .model::<Author>()
            .model::<Post>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        // Two tags so the M2M link targets valid rows.
        for (id, name) in &[(1_i64, "rust"), (2, "orm")] {
            sqlx::query("INSERT INTO nw_tag (id, name) VALUES (?, ?)")
                .bind(id)
                .bind(name)
                .execute(&pool)
                .await
                .expect("seed tag");
        }
    })
    .await;
}

fn meta(table: &str) -> ModelMeta {
    registered_models()
        .into_iter()
        .find(|m| m.table == table)
        .unwrap_or_else(|| panic!("no registered model for {table}"))
}

/// The nested spec a seeder builds: on `nw_author`, the `posts` json array maps
/// to reverse-FK children in `nw_post`.
fn spec() -> NestedSpec {
    let mut s = NestedSpec::new();
    s.insert("nw_author".into(), vec![("posts".into(), "nw_post".into())]);
    s
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

// ── tests ────────────────────────────────────────────────────────────────

/// The public writer, with NO RestPlugin / HTTP request, writes a parent plus
/// its declared reverse-FK children (FKs auto-filled) AND the declared M2M
/// links, all on one committed transaction.
#[tokio::test]
async fn writes_parent_children_and_m2m_in_one_tx() {
    let _g = test_lock().lock().await;
    boot().await;

    let authors_before = Author::objects().count().await.unwrap();
    let posts_before = Post::objects().count().await.unwrap();

    let mut body = obj(json!({
        "name": "Ada",
        "posts": [
            { "title": "First", "tags": [1, 2] },
            { "title": "Second" }
        ]
    }));

    let mut tx = db::begin().await.expect("begin");
    let out = write_nested_tree(&spec(), &meta("nw_author"), &mut body, &mut tx)
        .await
        .expect("nested write succeeds");
    tx.commit().await.expect("commit");

    // Parent + both children landed.
    assert_eq!(Author::objects().count().await.unwrap(), authors_before + 1);
    assert_eq!(Post::objects().count().await.unwrap(), posts_before + 2);

    // The returned graph carries the parent PK and both hydrated posts.
    let author_id = out.get("id").and_then(Value::as_i64).expect("author id");
    let posts = out
        .get("posts")
        .and_then(Value::as_array)
        .expect("posts array");
    assert_eq!(posts.len(), 2);

    // Each child's FK was auto-filled from the parent PK (the seed doc never
    // carried `author`).
    for p in posts {
        assert_eq!(
            p.get("author").and_then(Value::as_i64),
            Some(author_id),
            "child FK auto-filled from parent PK"
        );
    }

    // The first post's declared M2M ids became junction rows (hydrated back
    // onto the returned object).
    let first_tags = posts[0]
        .get("tags")
        .and_then(Value::as_array)
        .expect("hydrated tags");
    let mut ids: Vec<i64> = first_tags.iter().filter_map(Value::as_i64).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2], "declared M2M ids drove junction rows");
}

/// A child failure rolls the WHOLE tree back: after a forced error on a child
/// row (a required field missing), the parent row is NOT persisted.
#[tokio::test]
async fn child_failure_rolls_the_whole_tree_back() {
    let _g = test_lock().lock().await;
    boot().await;

    let authors_before = Author::objects().count().await.unwrap();
    let posts_before = Post::objects().count().await.unwrap();

    // The second child omits the required `title` → its insert fails AFTER the
    // parent row has already been written on the tx.
    let mut body = obj(json!({
        "name": "Grace",
        "posts": [
            { "title": "ok" },
            { "tags": [1] }
        ]
    }));

    let mut tx = db::begin().await.expect("begin");
    let res = write_nested_tree(&spec(), &meta("nw_author"), &mut body, &mut tx).await;
    assert!(res.is_err(), "the missing-title child must fail the write");
    // Drop the tx WITHOUT committing — sqlx rolls it back.
    drop(tx);

    // Nothing durable: not the parent, not the first (valid) child.
    assert_eq!(
        Author::objects().count().await.unwrap(),
        authors_before,
        "the parent row was rolled back with the failed child"
    );
    assert_eq!(
        Post::objects().count().await.unwrap(),
        posts_before,
        "no child row survived the rollback"
    );
}
