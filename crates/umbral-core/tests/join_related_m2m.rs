//! Gap #113 — `.join_related("<m2m_field>")` emits a double LEFT
//! JOIN through the junction table and dedups parents in the
//! fetch path, populating each parent's `M2M` slot with all the
//! matching children in ONE round-trip.
//!
//! Trade-off: M2M JOINs multiply parent rows by avg cardinality
//! (Cartesian risk). For most apps `prefetch_related` is still
//! the right default — one extra round-trip vs much wider rows
//! and parent dedup work in the client. M2M-via-JOIN is the
//! alternative for hot list pages over SMALL M2Ms where the
//! second round-trip dominates.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{ForeignKey, M2M};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "jrm2m_tag")]
pub struct Tag {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "jrm2m_category")]
pub struct Category {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "jrm2m_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub category: ForeignKey<Category>,
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(m2m = "jrm2m_tag")]
    pub tags: M2M<Tag>,
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
            .model::<Category>()
            .model::<Post>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        // Categories: 1 = news, 2 = tech
        for name in &["news", "tech"] {
            sqlx::query("INSERT INTO jrm2m_category (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed category");
        }
        // Tags: 1 rust, 2 web, 3 db
        for name in &["rust", "web", "db"] {
            sqlx::query("INSERT INTO jrm2m_tag (name) VALUES (?)")
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed tag");
        }
        // Posts:
        //   alpha (1): category=tech(2), tags=[rust, web, db]   ← 3 tags exercises dedup
        //   beta  (2): category=news(1), tags=[rust]
        //   gamma (3): category=tech(2), tags=[]                ← LEFT JOIN miss
        for (title, cat) in &[("alpha", 2_i64), ("beta", 1), ("gamma", 2)] {
            sqlx::query("INSERT INTO jrm2m_post (title, category) VALUES (?, ?)")
                .bind(*title)
                .bind(*cat)
                .execute(&pool)
                .await
                .expect("seed post");
        }
        for (post, tag) in &[(1_i64, 1_i64), (1, 2), (1, 3), (2, 1)] {
            sqlx::query("INSERT INTO jrm2m_post_tags (parent_id, child_id) VALUES (?, ?)")
                .bind(*post)
                .bind(*tag)
                .execute(&pool)
                .await
                .expect("seed junction");
        }
    })
    .await;
}

fn by_title(posts: &[Post]) -> std::collections::HashMap<&str, &Post> {
    posts.iter().map(|p| (p.title.as_str(), p)).collect()
}

#[tokio::test]
async fn to_sql_emits_double_left_join_through_junction() {
    boot().await;
    let sql = Post::objects().join_related("tags").to_sql();
    // Two LEFT JOINs — one for the junction, one for the child table.
    let join_count = sql.matches("LEFT JOIN").count();
    assert_eq!(
        join_count, 2,
        "M2M JOIN must emit exactly TWO LEFT JOINs (junction + child): {sql}"
    );
    // Junction alias + child alias both present.
    assert!(
        sql.contains("\"jrm2m_post_tags\""),
        "junction table name must appear: {sql}"
    );
    assert!(
        sql.contains("\"__jm_tags\"") && sql.contains("\"__j_tags\""),
        "both junction (__jm_) and child (__j_) aliases must appear: {sql}"
    );
    // Aliased child cols project under the same `<field>__<col>`
    // shape the FK branch uses (so the decode helper reuses).
    assert!(
        sql.contains("\"tags__id\"") && sql.contains("\"tags__name\""),
        "aliased child cols must be present: {sql}"
    );
}

#[tokio::test]
async fn parent_with_three_tags_dedups_to_one_instance_with_three_tags() {
    boot().await;
    let posts = Post::objects()
        .join_related("tags")
        .fetch()
        .await
        .expect("fetch");
    let by = by_title(&posts);
    let alpha = by.get("alpha").expect("alpha present");

    // CRITICAL: the parent appears ONCE despite 3 matching tags.
    let alpha_count = posts.iter().filter(|p| p.title == "alpha").count();
    assert_eq!(
        alpha_count, 1,
        "M2M JOIN must dedup parents — alpha has 3 tags but should appear once, got {alpha_count}"
    );

    let tags = alpha.tags.resolved().expect("M2M slot hydrated");
    let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        tags.len(),
        3,
        "all three tags must be in the slot: {names:?}"
    );
    assert!(names.contains(&"rust"));
    assert!(names.contains(&"web"));
    assert!(names.contains(&"db"));
}

#[tokio::test]
async fn left_join_miss_yields_empty_m2m_slot() {
    boot().await;
    let posts = Post::objects()
        .join_related("tags")
        .fetch()
        .await
        .expect("fetch");
    let by = by_title(&posts);
    let gamma = by.get("gamma").expect("gamma present");

    // gamma has no tags → LEFT JOIN miss → the row still surfaces
    // (we want gamma in the result set) but the M2M slot is an
    // empty Vec rather than None.
    let tags = gamma
        .tags
        .resolved()
        .expect("M2M slot must be initialised even on LEFT JOIN miss");
    assert!(
        tags.is_empty(),
        "gamma has no tags → resolved() = Some(&[]), got {} tags",
        tags.len()
    );
}

#[tokio::test]
async fn m2m_join_composes_with_fk_join() {
    boot().await;
    // FK join on `category` + M2M join on `tags` in one query.
    let posts = Post::objects()
        .join_related("category")
        .join_related("tags")
        .fetch()
        .await
        .expect("fetch");

    // Parents still dedup.
    let alpha_count = posts.iter().filter(|p| p.title == "alpha").count();
    assert_eq!(
        alpha_count, 1,
        "parent still dedups when mixing FK + M2M JOIN"
    );

    let by = by_title(&posts);
    let alpha = by.get("alpha").expect("alpha");
    // FK join hydrated.
    let cat = alpha.category.resolved().expect("FK hydrated");
    assert_eq!(cat.name, "tech");
    // M2M slot populated.
    assert_eq!(alpha.tags.resolved().expect("M2M hydrated").len(), 3);
}

#[tokio::test]
async fn each_parent_gets_only_its_own_tags() {
    boot().await;
    let posts = Post::objects()
        .join_related("tags")
        .fetch()
        .await
        .expect("fetch");
    let by = by_title(&posts);

    let alpha_tags: Vec<&str> = by["alpha"]
        .tags
        .resolved()
        .unwrap()
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    let beta_tags: Vec<&str> = by["beta"]
        .tags
        .resolved()
        .unwrap()
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    let gamma_tags: Vec<&str> = by["gamma"]
        .tags
        .resolved()
        .unwrap()
        .iter()
        .map(|t| t.name.as_str())
        .collect();

    assert_eq!(alpha_tags.len(), 3);
    assert_eq!(beta_tags, vec!["rust"]);
    assert_eq!(gamma_tags.len(), 0);

    // Strict isolation — beta has only `rust`, not the other tags
    // that alpha shares with it via the junction.
    assert!(!beta_tags.contains(&"web"));
    assert!(!beta_tags.contains(&"db"));
}

#[tokio::test]
async fn inner_join_related_m2m_drops_a_tagless_parent() {
    boot().await;
    // The LEFT-join counterpart (`join_related`) KEEPS gamma with an empty
    // M2M slot (see `left_join_miss_yields_empty_m2m_slot`). INNER join
    // through the junction must instead DROP gamma entirely — a parent with
    // no junction row has nothing to inner-join against, so it falls out of
    // the result set. This is the row-set proof of the M2M INNER drop path.
    let posts = Post::objects()
        .inner_join_related("tags")
        .fetch()
        .await
        .expect("fetch");
    let by = by_title(&posts);

    // gamma (no tags) is dropped; alpha + beta (which have tags) survive.
    assert!(
        !by.contains_key("gamma"),
        "INNER join through the junction must DROP the tag-less parent, but gamma survived: {:?}",
        posts.iter().map(|p| p.title.as_str()).collect::<Vec<_>>()
    );
    assert!(by.contains_key("alpha"), "alpha has tags → kept");
    assert!(by.contains_key("beta"), "beta has tags → kept");

    // Parents still dedup: alpha has 3 tags but appears once with all three.
    let alpha_count = posts.iter().filter(|p| p.title == "alpha").count();
    assert_eq!(alpha_count, 1, "alpha still dedups to one row under INNER");
    assert_eq!(
        by["alpha"].tags.resolved().expect("M2M hydrated").len(),
        3,
        "the surviving parent still carries all its tags"
    );
}

#[tokio::test]
async fn empty_join_related_keeps_pre_fix_path_unchanged() {
    boot().await;
    // Sanity: no join_related → no JOIN emitted, no dedup. Three
    // posts come back, one instance each. Verifies the new dedup
    // path doesn't affect the byte-for-byte unchanged FK-free path.
    let posts = Post::objects().fetch().await.expect("fetch");
    let our: Vec<&Post> = posts
        .iter()
        .filter(|p| matches!(p.title.as_str(), "alpha" | "beta" | "gamma"))
        .collect();
    assert_eq!(our.len(), 3);
    // M2M slots are None because nothing was hydrated.
    for p in our {
        assert!(p.tags.resolved().is_none(), "no prefetch → unhydrated");
    }
}
