//! Gap 19 — `QuerySet::prefetch_related("field_name")` for M2M fields.
//!
//! After the main query returns N parent rows, prefetch_related issues
//! ONE batched JOIN query against the junction + child table for ALL
//! parents, then attaches the matching children to each parent's
//! `M2M.resolved` slot. This is the M2M counterpart of `select_related`
//! for FKs — both eliminate the N+1 footgun.
//!
//! v1 scope: M2M only, i64 parent PK. Reverse-FK collection prefetch
//! (`prefetch_related("comment_set")`) is deferred until a Vec-on-
//! parent slot lands; see gap #19 follow-ups.

#![allow(dead_code, private_interfaces)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral::orm::M2M;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(plugin = "pref")]
pub struct PrefGroup {
    pub id: i64,
    pub name: String,
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: M2M<PrefTag>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(plugin = "pref")]
pub struct PrefTag {
    pub id: i64,
    pub label: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("prefetch_related.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<PrefGroup>()
            .model::<PrefTag>()
            .build()
            .expect("App::build");
        let migration_tmp = tempfile::tempdir().expect("migration tempdir");
        let migration_path = migration_tmp.path().to_path_buf();
        std::mem::forget(migration_tmp);
        umbral::migrate::make_in(&migration_path)
            .await
            .expect("make_in");
        umbral::migrate::run_in(&migration_path)
            .await
            .expect("run_in");
    })
    .await;
}

async fn fresh_group(name: &str) -> PrefGroup {
    PrefGroup::objects()
        .create(PrefGroup {
            id: 0,
            name: name.into(),
            tags: M2M::empty(),
        })
        .await
        .expect("create group")
}

async fn fresh_tag(label: &str) -> PrefTag {
    PrefTag::objects()
        .create(PrefTag {
            id: 0,
            label: label.into(),
        })
        .await
        .expect("create tag")
}

#[tokio::test]
async fn prefetch_related_populates_resolved_on_every_parent() {
    boot().await;
    let g1 = fresh_group("g1-prefetch").await;
    let g2 = fresh_group("g2-prefetch").await;
    let t1 = fresh_tag("t1").await;
    let t2 = fresh_tag("t2").await;
    let t3 = fresh_tag("t3").await;
    g1.tags.add(&t1).await.expect("g1+t1");
    g1.tags.add(&t2).await.expect("g1+t2");
    g2.tags.add(&t3).await.expect("g2+t3");

    let groups = PrefGroup::objects()
        .filter(pref_group::ID.gte(g1.id))
        .filter(pref_group::ID.lte(g2.id))
        .prefetch_related("tags")
        .fetch()
        .await
        .expect("prefetch fetch");

    // Both groups should come back with their tags populated.
    let g1_loaded = groups
        .iter()
        .find(|g| g.id == g1.id)
        .expect("g1 in results");
    let g1_tags = g1_loaded
        .tags
        .resolved()
        .expect("g1 tags resolved by prefetch");
    let mut g1_labels: Vec<&str> = g1_tags.iter().map(|t| t.label.as_str()).collect();
    g1_labels.sort();
    assert_eq!(g1_labels, vec!["t1", "t2"]);

    let g2_loaded = groups
        .iter()
        .find(|g| g.id == g2.id)
        .expect("g2 in results");
    let g2_tags = g2_loaded
        .tags
        .resolved()
        .expect("g2 tags resolved by prefetch");
    let g2_labels: Vec<&str> = g2_tags.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(g2_labels, vec!["t3"]);
}

#[tokio::test]
async fn prefetch_related_without_call_leaves_resolved_empty() {
    boot().await;
    let g = fresh_group("g-no-prefetch").await;
    let t = fresh_tag("plain").await;
    g.tags.add(&t).await.expect("add");

    let groups = PrefGroup::objects()
        .filter(pref_group::ID.eq(g.id))
        .fetch()
        .await
        .expect("plain fetch");
    let loaded = &groups[0];
    assert!(
        loaded.tags.resolved().is_none(),
        "without prefetch_related, M2M slot stays unresolved"
    );
}

#[tokio::test]
async fn prefetch_related_with_no_matching_children_yields_empty_vec() {
    boot().await;
    let g = fresh_group("g-no-children-pref").await;

    let groups = PrefGroup::objects()
        .filter(pref_group::ID.eq(g.id))
        .prefetch_related("tags")
        .fetch()
        .await
        .expect("prefetch fetch");
    let loaded = &groups[0];
    let tags = loaded.tags.resolved().expect("resolved even when empty");
    assert!(tags.is_empty());
}
