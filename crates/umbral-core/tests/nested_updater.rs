//! gaps4 #91 — the reusable ORM-layer nested-tree UPDATER
//! (`umbral::orm::nested::update_nested_tree`) is callable with NO REST plugin /
//! HTTP request, symmetric with the #77 writer.
//!
//! These tests exercise the public updater the way a CLI seeder / AI-agent
//! object-graph reconciler would: hand ONE nested JSON document to ONE call and
//! get the parent updated, each child carrying its pk UPDATED in place (scoped
//! to the parent via its FK), each child WITHOUT a pk CREATED with its FK set,
//! and children absent from the payload left untouched — all on ONE
//! transaction, rolled back whole on any error (including the cross-parent
//! ownership guard).

#![allow(dead_code)]

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::{Mutex as TokioMutex, OnceCell};

use umbral::migrate::{ModelMeta, registered_models};
use umbral::orm::ForeignKey;
use umbral::orm::nested::{NestedError, NestedSpec, update_nested_tree, write_nested_tree};
use umbral_core::db;

/// Serialise the tests: they share one file-backed DB and its tables.
fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

// ── models ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nu_author")]
pub struct Author {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nu_post")]
pub struct Post {
    pub id: i64,
    #[umbral(string)]
    pub title: String,
    /// Reverse-FK link to the parent Author — auto-filled by the writer on a
    /// CREATE, and the ownership anchor on an UPDATE.
    pub author: ForeignKey<Author>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let dir = std::env::temp_dir();
        let path = dir.join(format!("umbral_nested_updater_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Author>()
            .model::<Post>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

fn meta(table: &str) -> ModelMeta {
    registered_models()
        .into_iter()
        .find(|m| m.table == table)
        .unwrap_or_else(|| panic!("no registered model for {table}"))
}

/// On `nu_author`, the `posts` json array maps to reverse-FK children in
/// `nu_post`.
fn spec() -> NestedSpec {
    let mut s = NestedSpec::new();
    s.insert("nu_author".into(), vec![("posts".into(), "nu_post".into())]);
    s
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

/// Seed one author + its posts via the (already-tested) writer, returning the
/// committed author id and the ids of the posts in declaration order.
async fn seed_author(name: &str, titles: &[&str]) -> (i64, Vec<i64>) {
    let posts: Vec<Value> = titles.iter().map(|t| json!({ "title": t })).collect();
    let mut body = obj(json!({ "name": name, "posts": posts }));
    let mut tx = db::begin().await.expect("begin");
    let out = write_nested_tree(&spec(), &meta("nu_author"), &mut body, &mut tx)
        .await
        .expect("seed write");
    tx.commit().await.expect("commit");
    let author_id = out.get("id").and_then(Value::as_i64).expect("author id");
    let post_ids = out
        .get("posts")
        .and_then(Value::as_array)
        .expect("posts")
        .iter()
        .map(|p| p.get("id").and_then(Value::as_i64).expect("post id"))
        .collect();
    (author_id, post_ids)
}

// ── tests ────────────────────────────────────────────────────────────────

/// The public updater, with NO RestPlugin / HTTP request, updates the parent,
/// UPDATES a child that carries its pk, CREATEs a child that doesn't (FK
/// auto-filled), and leaves an unmentioned child untouched — all on one
/// committed transaction (upsert, no implicit deletes).
#[tokio::test]
async fn updates_parent_upserts_existing_and_creates_new_child() {
    let _g = test_lock().lock().await;
    boot().await;

    let (author_id, post_ids) = seed_author("Ada", &["First", "Second"]).await;
    let first_id = post_ids[0];
    let second_id = post_ids[1];
    let posts_before = Post::objects().count().await.unwrap();

    // Update the parent name, edit "First" in place, add a brand-new "Third",
    // and say nothing about "Second".
    let mut body = obj(json!({
        "name": "Ada Lovelace",
        "posts": [
            { "id": first_id, "title": "First (edited)" },
            { "title": "Third" }
        ]
    }));

    let mut tx = db::begin().await.expect("begin");
    let out = update_nested_tree(
        &spec(),
        &meta("nu_author"),
        "id",
        &author_id.to_string(),
        &mut body,
        &mut tx,
    )
    .await
    .expect("nested update succeeds");
    tx.commit().await.expect("commit");

    // Parent scalar column updated.
    let author = Author::objects()
        .filter(author::ID.eq(author_id))
        .first()
        .await
        .unwrap()
        .expect("author");
    assert_eq!(author.name, "Ada Lovelace", "parent scalar updated");

    // Exactly one NEW post created (Third); First edited in place; Second kept.
    assert_eq!(
        Post::objects().count().await.unwrap(),
        posts_before + 1,
        "one new child created, none deleted"
    );
    let first = Post::objects()
        .filter(post::ID.eq(first_id))
        .first()
        .await
        .unwrap()
        .expect("first post");
    assert_eq!(
        first.title, "First (edited)",
        "existing child updated in place"
    );

    let second = Post::objects()
        .filter(post::ID.eq(second_id))
        .first()
        .await
        .unwrap()
        .expect("second post");
    assert_eq!(
        second.title, "Second",
        "an unmentioned child is left untouched (no implicit delete)"
    );

    // The returned graph carries the upserted children; the created one has its
    // FK auto-filled from the parent.
    let posts = out
        .get("posts")
        .and_then(Value::as_array)
        .expect("posts array");
    assert_eq!(posts.len(), 2, "response hydrates the payload's children");
    let third = posts
        .iter()
        .find(|p| p.get("title").and_then(Value::as_str) == Some("Third"))
        .expect("created child in response");
    assert_eq!(
        third.get("author").and_then(Value::as_i64),
        Some(author_id),
        "created child's FK auto-filled from the parent PK"
    );
}

/// A child pk that belongs to a DIFFERENT parent is a `NestedError::NotFound`
/// (REST maps this to 404), and the WHOLE update rolls back: the other parent's
/// child is unchanged AND this parent's own scalar change is reverted.
#[tokio::test]
async fn cross_parent_child_pk_is_not_found_and_rolls_back() {
    let _g = test_lock().lock().await;
    boot().await;

    let (a_id, _a_posts) = seed_author("Grace", &["A-post"]).await;
    let (_b_id, b_posts) = seed_author("Katherine", &["B-post"]).await;
    let b_post_id = b_posts[0];

    // PATCH author A, but point a nested item at author B's post id.
    let mut body = obj(json!({
        "name": "should-not-persist",
        "posts": [ { "id": b_post_id, "title": "hijack" } ]
    }));

    let mut tx = db::begin().await.expect("begin");
    let res = update_nested_tree(
        &spec(),
        &meta("nu_author"),
        "id",
        &a_id.to_string(),
        &mut body,
        &mut tx,
    )
    .await;
    assert!(
        matches!(res, Err(NestedError::NotFound(_))),
        "a child pk from another parent is NotFound, got {res:?}"
    );
    // Drop the tx WITHOUT committing — sqlx rolls the whole update back.
    drop(tx);

    // B's post is unchanged...
    let b_post = Post::objects()
        .filter(post::ID.eq(b_post_id))
        .first()
        .await
        .unwrap()
        .expect("B post");
    assert_eq!(
        b_post.title, "B-post",
        "another parent's child was not mutated"
    );

    // ...and A's scalar change from the failed update rolled back.
    let a = Author::objects()
        .filter(author::ID.eq(a_id))
        .first()
        .await
        .unwrap()
        .expect("author A");
    assert_eq!(
        a.name, "Grace",
        "the failed nested update rolled the parent scalar change back too"
    );
}
