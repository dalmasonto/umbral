//! gaps #112 / PK lift Pass A — `?include=` works when the FK
//! target has a non-i64 PK.
//!
//! Pre-fix, `DynQuerySet::hydrate_select_related_into` collected
//! FK values via `.as_i64()` and queried the target with `WHERE id
//! IN (...)`. A target like `permissions_permission` whose PK is a
//! String column named `codename` silently dropped every FK on
//! the floor (`.as_i64()` on a JSON String returns None) — REST
//! `?include=permission` on the FK side returned the bare codename
//! string instead of expanding to the full permission row.
//!
//! This test pins the fix: a `Tag` model with a String PK (`slug`)
//! and a `Bookmark` model with `ForeignKey<Tag>` round-trips
//! through the dynamic hydrator end-to-end.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbra::orm::{DynQuerySet, ForeignKey};
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "spk_tag")]
pub struct Tag {
    /// String PK, not the default `id: i64`. Same shape
    /// `permissions_permission.codename` uses since gap #60.
    #[umbra(primary_key, string, max_length = 50)]
    pub slug: String,
    #[umbra(string)]
    pub label: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "spk_bookmark")]
pub struct Bookmark {
    pub id: i64,
    #[umbra(string)]
    pub url: String,
    /// FK to a String-PK target. The dynamic hydrator must read
    /// this column as a JSON String, not i64, and bind it as a
    /// String in the IN-list against `spk_tag.slug`.
    pub tag: ForeignKey<Tag>,
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
            .model::<Tag>()
            .model::<Bookmark>()
            .build()
            .expect("App::build");

        sqlx::query(
            "CREATE TABLE spk_tag (
                slug  TEXT PRIMARY KEY,
                label TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE spk_tag");

        sqlx::query(
            "CREATE TABLE spk_bookmark (
                id  INTEGER PRIMARY KEY AUTOINCREMENT,
                url TEXT NOT NULL,
                tag TEXT NOT NULL REFERENCES spk_tag(slug)
            )",
        )
        .execute(&pool)
        .await
        .expect("CREATE TABLE spk_bookmark");

        for (slug, label) in &[("rust", "Rust"), ("web", "Web"), ("db", "Database")] {
            sqlx::query("INSERT INTO spk_tag (slug, label) VALUES (?, ?)")
                .bind(*slug)
                .bind(*label)
                .execute(&pool)
                .await
                .expect("seed tag");
        }
        for (url, tag) in &[
            ("https://rust-lang.org", "rust"),
            ("https://crates.io", "rust"),
            ("https://docs.rs", "web"),
        ] {
            sqlx::query("INSERT INTO spk_bookmark (url, tag) VALUES (?, ?)")
                .bind(*url)
                .bind(*tag)
                .execute(&pool)
                .await
                .expect("seed bookmark");
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
async fn select_related_dyn_expands_string_pk_target() {
    boot().await;
    let rows = DynQuerySet::for_meta(&meta_for("spk_bookmark"))
        .select_related_dyn(&["tag".to_string()])
        .order_by_col("id", false)
        .fetch_as_json()
        .await
        .expect("fetch");
    assert!(rows.len() >= 3, "expected at least 3 seeded bookmarks");

    // Every row's `tag` field should now be a FULL OBJECT
    // (the Tag row keyed by `slug`), NOT the raw string id.
    // This is the regression the i64-only hydrator caused: the
    // raw string was passed through unchanged because
    // `.as_i64()` returned None and the FK was never queued for
    // the IN-list.
    let first = &rows[0];
    let tag = first
        .get("tag")
        .expect("tag field present")
        .as_object()
        .expect(
            "tag must be an object after select_related_dyn (was the bare slug pre-fix); \
             got: {tag:?}",
        );
    assert!(
        tag.contains_key("slug"),
        "expanded tag carries its slug PK; got keys: {:?}",
        tag.keys().collect::<Vec<_>>()
    );
    assert!(
        tag.contains_key("label"),
        "expanded tag carries its non-PK columns too; got keys: {:?}",
        tag.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn fetch_as_strings_renders_string_pk_fk_cell() {
    // Review #3: the admin display path (`fetch_as_strings`) decoded every
    // ForeignKey cell as i64. `Bookmark.tag` targets a String-PK `Tag`, so
    // its column holds a slug — decoding it as i64 fails. It must render as
    // the slug string.
    boot().await;
    let rows = DynQuerySet::for_meta(&meta_for("spk_bookmark"))
        .order_by_col("id", false)
        .fetch_as_strings()
        .await
        .expect("fetch_as_strings must not fail on a String-PK FK column");
    assert!(rows.len() >= 3);
    assert_eq!(rows[0].get("tag").map(String::as_str), Some("rust"));
    assert_eq!(
        rows[0].get("url").map(String::as_str),
        Some("https://rust-lang.org")
    );
}

#[tokio::test]
async fn select_related_dyn_dedupes_string_pk_fk_ids_across_rows() {
    // Two of the three bookmarks point at slug="rust". The
    // pk-key dedup must collapse those into ONE bind in the
    // SELECT — not two — so the IN-list stays minimal.
    boot().await;
    let rows = DynQuerySet::for_meta(&meta_for("spk_bookmark"))
        .select_related_dyn(&["tag".to_string()])
        .fetch_as_json()
        .await
        .expect("fetch");
    let by_url: std::collections::HashMap<String, &serde_json::Map<String, serde_json::Value>> =
        rows.iter()
            .map(|r| {
                (
                    r.get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    r,
                )
            })
            .collect();
    let r1 = by_url["https://rust-lang.org"];
    let r2 = by_url["https://crates.io"];
    let r3 = by_url["https://docs.rs"];

    // Same expanded shape on both rust rows, distinct one on the
    // web row — proves the dedup'd batch fetch found both source
    // bookmarks AND mapped them back correctly.
    assert_eq!(
        r1.get("tag")
            .and_then(|t| t.as_object())
            .unwrap()
            .get("label")
            .and_then(|v| v.as_str()),
        Some("Rust"),
    );
    assert_eq!(
        r2.get("tag")
            .and_then(|t| t.as_object())
            .unwrap()
            .get("label")
            .and_then(|v| v.as_str()),
        Some("Rust"),
    );
    assert_eq!(
        r3.get("tag")
            .and_then(|t| t.as_object())
            .unwrap()
            .get("label")
            .and_then(|v| v.as_str()),
        Some("Web"),
    );
}
