//! gap2 #18 — `DynQuerySet::select_related_dyn` accepts dotted /
//! `__` chains and `hydrate_select_related_into` walks per-hop.
//!
//! Two-hop expansion turns `?include=author.profile` into `author`
//! as a nested object with `author.profile` further nested inside
//! it. Query budget is `1 + len(hops)` per chain regardless of
//! parent count.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::{DynQuerySet, ForeignKey};
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "srn_dyn_profile")]
pub struct Profile {
    pub id: i64,
    #[umbra(string)]
    pub github_url: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "srn_dyn_author")]
pub struct Author {
    pub id: i64,
    #[umbra(string)]
    pub name: String,
    pub profile: ForeignKey<Profile>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "srn_dyn_post")]
pub struct Post {
    pub id: i64,
    #[umbra(string)]
    pub title: String,
    pub author: ForeignKey<Author>,
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
            .model::<Profile>()
            .model::<Author>()
            .model::<Post>()
            .build()
            .expect("App::build");

        for sql in &[
            "CREATE TABLE srn_dyn_profile (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                github_url TEXT NOT NULL
            )",
            "CREATE TABLE srn_dyn_author (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                profile INTEGER NOT NULL REFERENCES srn_dyn_profile(id)
            )",
            "CREATE TABLE srn_dyn_post (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                author INTEGER NOT NULL REFERENCES srn_dyn_author(id)
            )",
        ] {
            sqlx::query(sql).execute(&pool).await.expect("ddl");
        }
        for (id, url) in &[(1_i64, "https://gh/alice"), (2, "https://gh/bob")] {
            sqlx::query("INSERT INTO srn_dyn_profile (id, github_url) VALUES (?, ?)")
                .bind(*id)
                .bind(*url)
                .execute(&pool)
                .await
                .expect("seed profile");
        }
        for (id, name, profile) in &[(1_i64, "alice", 1_i64), (2, "bob", 2)] {
            sqlx::query("INSERT INTO srn_dyn_author (id, name, profile) VALUES (?, ?, ?)")
                .bind(*id)
                .bind(*name)
                .bind(*profile)
                .execute(&pool)
                .await
                .expect("seed author");
        }
        for (id, title, author) in &[(1_i64, "p1", 1_i64), (2, "p2", 2)] {
            sqlx::query("INSERT INTO srn_dyn_post (id, title, author) VALUES (?, ?, ?)")
                .bind(*id)
                .bind(*title)
                .bind(*author)
                .execute(&pool)
                .await
                .expect("seed post");
        }
    })
    .await;
}

fn meta_for(table: &str) -> umbra::migrate::ModelMeta {
    umbra::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == table)
        .expect("registered")
}

#[tokio::test]
async fn select_related_dyn_one_hop_expands_fk() {
    boot().await;
    let meta = meta_for("srn_dyn_post");
    let rows = DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", "p1")
        .select_related_dyn(&["author".to_string()])
        .fetch_as_json()
        .await
        .expect("fetch");
    assert_eq!(rows.len(), 1);
    let author = rows[0].get("author").expect("author present");
    let author_obj = author.as_object().expect("author expanded to object");
    assert_eq!(
        author_obj.get("name").and_then(|v| v.as_str()),
        Some("alice")
    );
    // One-hop only: nested `profile` should remain the raw FK id.
    assert!(
        author_obj.get("profile").and_then(|v| v.as_i64()).is_some(),
        "one-hop include leaves nested profile as integer id"
    );
}

#[tokio::test]
async fn select_related_dyn_two_hop_dotted_expands_chain() {
    boot().await;
    let meta = meta_for("srn_dyn_post");
    let rows = DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", "p1")
        .select_related_dyn(&["author.profile".to_string()])
        .fetch_as_json()
        .await
        .expect("fetch");
    assert_eq!(rows.len(), 1);
    let author = rows[0]
        .get("author")
        .and_then(|v| v.as_object())
        .expect("author expanded");
    let profile = author
        .get("profile")
        .and_then(|v| v.as_object())
        .expect("profile expanded through second hop");
    assert_eq!(
        profile.get("github_url").and_then(|v| v.as_str()),
        Some("https://gh/alice")
    );
}

#[tokio::test]
async fn select_related_dyn_two_hop_double_underscore_normalizes() {
    boot().await;
    let meta = meta_for("srn_dyn_post");
    let rows = DynQuerySet::for_meta(&meta)
        .filter_eq_string("title", "p2")
        .select_related_dyn(&["author__profile".to_string()])
        .fetch_as_json()
        .await
        .expect("fetch");
    assert_eq!(rows.len(), 1);
    let author = rows[0]
        .get("author")
        .and_then(|v| v.as_object())
        .expect("author expanded");
    let profile = author
        .get("profile")
        .and_then(|v| v.as_object())
        .expect("profile expanded through second hop");
    assert_eq!(
        profile.get("github_url").and_then(|v| v.as_str()),
        Some("https://gh/bob")
    );
}

#[tokio::test]
async fn select_related_dyn_drops_unknown_chains_silently() {
    boot().await;
    let meta = meta_for("srn_dyn_post");
    let qs = DynQuerySet::for_meta(&meta).select_related_dyn(&[
        "author".to_string(),
        "author.nonexistent".to_string(),
        "nonexistent.profile".to_string(),
        "title.profile".to_string(), // title is not an FK
    ]);
    // Only the valid single-hop "author" survives. Matches the
    // pre-existing single-hop "silently drop unknown" contract.
    assert_eq!(qs.select_related_fields(), &["author".to_string()]);
}

#[tokio::test]
async fn select_related_dyn_dedups_canonical_form() {
    boot().await;
    let meta = meta_for("srn_dyn_post");
    let qs = DynQuerySet::for_meta(&meta)
        .select_related_dyn(&["author.profile".to_string(), "author__profile".to_string()]);
    // Both normalise to the same canonical "author.profile";
    // the second is a no-op.
    assert_eq!(qs.select_related_fields(), &["author.profile".to_string()]);
}
