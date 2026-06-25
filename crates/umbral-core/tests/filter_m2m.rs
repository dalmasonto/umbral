//! Feature 55 follow-up — `DynQuerySet::filter_m2m_contains_any`.
//!
//! The admin's multi-select filter dialog supports M2M fields ("show
//! products tagged with any of these tags"). This test pins the
//! emitted IN-subquery against an actual SQLite database to verify
//! the junction-table name convention and the OR-on-any semantics
//! (a row matches if it has at least one of the listed child ids).

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::DynQuerySet;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fm2m_tag")]
pub struct Tag {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fm2m_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    /// M2M to Tag. The derive emits the junction table name
    /// `fm2m_post_tags` (parent_table + "_" + field_name).
    #[umbral(m2m = "fm2m_tag")]
    pub tags: umbral::orm::M2M<Tag>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Tag>()
            .model::<Post>()
            .build()
            .expect("App::build");

        sqlx::query(
            "CREATE TABLE fm2m_tag (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE tag");
        sqlx::query(
            "CREATE TABLE fm2m_post (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE post");
        sqlx::query(
            "CREATE TABLE fm2m_post_tags (
                parent_id INTEGER NOT NULL REFERENCES fm2m_post(id) ON DELETE CASCADE,
                child_id  INTEGER NOT NULL REFERENCES fm2m_tag(id)  ON DELETE CASCADE,
                PRIMARY KEY (parent_id, child_id)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE junction");

        for name in &["rust", "web", "framework"] {
            sqlx::query("INSERT INTO fm2m_tag (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed tag");
        }
        for title in &["alpha", "beta", "gamma"] {
            sqlx::query("INSERT INTO fm2m_post (title) VALUES (?)")
                .bind(*title)
                .execute(&pool)
                .await
                .expect("seed post");
        }
        // alpha → rust, web
        // beta  → web, framework
        // gamma → framework
        for (parent, child) in &[(1, 1), (1, 2), (2, 2), (2, 3), (3, 3)] {
            sqlx::query("INSERT INTO fm2m_post_tags (parent_id, child_id) VALUES (?, ?)")
                .bind(*parent as i64)
                .bind(*child as i64)
                .execute(&pool)
                .await
                .expect("seed junction");
        }
    })
    .await;
}

fn meta_for(table: &str) -> umbral::migrate::ModelMeta {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == table)
        .expect("registered")
}

#[tokio::test]
async fn filter_m2m_contains_any_returns_parents_tagged_with_any_child() {
    boot().await;
    let meta = meta_for("fm2m_post");
    // tag=1 (rust) → alpha; tag=2 (web) → alpha + beta. Union = 2 distinct posts.
    let n = DynQuerySet::for_meta(&meta)
        .filter_m2m_contains_any("tags", &["1".to_string(), "2".to_string()])
        .count()
        .await
        .expect("count");
    assert_eq!(
        n, 2,
        "alpha + beta both have at least one of {{rust, web}} tags"
    );
}

#[tokio::test]
async fn filter_m2m_contains_any_single_value_matches_only_linked_parents() {
    boot().await;
    let meta = meta_for("fm2m_post");
    let n = DynQuerySet::for_meta(&meta)
        .filter_m2m_contains_any("tags", &["3".to_string()])
        .count()
        .await
        .expect("count");
    assert_eq!(n, 2, "tag=3 (framework) is on beta + gamma");
}

#[tokio::test]
async fn filter_m2m_contains_any_unknown_field_is_noop() {
    boot().await;
    let meta = meta_for("fm2m_post");
    let n = DynQuerySet::for_meta(&meta)
        .filter_m2m_contains_any("nonexistent", &["1".to_string()])
        .count()
        .await
        .expect("count");
    assert_eq!(
        n, 3,
        "unknown field name shouldn't constrain the result set"
    );
}

#[tokio::test]
async fn filter_m2m_contains_any_empty_input_is_noop() {
    boot().await;
    let meta = meta_for("fm2m_post");
    let n = DynQuerySet::for_meta(&meta)
        .filter_m2m_contains_any("tags", &[])
        .count()
        .await
        .expect("count");
    assert_eq!(n, 3, "empty child list shouldn't filter anything out");
}

#[tokio::test]
async fn filter_m2m_contains_any_unparseable_ids_are_dropped() {
    boot().await;
    let meta = meta_for("fm2m_post");
    // "garbage" drops; "3" survives — should match beta + gamma.
    let n = DynQuerySet::for_meta(&meta)
        .filter_m2m_contains_any("tags", &["garbage".to_string(), "3".to_string()])
        .count()
        .await
        .expect("count");
    assert_eq!(n, 2, "only parseable child id 3 contributes");
}
