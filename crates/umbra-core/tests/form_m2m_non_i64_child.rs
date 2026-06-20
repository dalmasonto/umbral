//! gaps2 #73 — M2M form junction writes with non-i64 child PKs.
//!
//! The form layer calls `validate_multi_fk_exists` which returns
//! `Vec<sea_query::Value>` staged on the M2M field's `pending` slot. The
//! typed `create()` path drains them and calls `set_junction_dynamic`. All
//! of this must work when the *child* model has a String or Uuid primary key
//! — the submitted form ids are strings in both cases, and the junction write
//! must carry them through without dropping.
//!
//! Prior to the fix the code forced a `BigInt`-fallback on the pk_ty lookup
//! (only Uuid and Text were handled correctly; i64 was the implicit default)
//! so String/Uuid child ids that failed `.parse::<i64>()` were silently
//! dropped: the form reported success but wrote zero junction rows.

#![allow(dead_code)]

use std::collections::HashMap;
use tokio::sync::OnceCell;
use umbra::forms::FormValidate;
use umbra_core::db;

// ── models: String-PK child ──────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "fmc_label")]
pub struct Label {
    #[umbra(primary_key)]
    pub code: String,
    pub name: String,
}

#[derive(
    Debug,
    Clone,
    Default,
    sqlx::FromRow,
    serde::Serialize,
    serde::Deserialize,
    umbra::orm::Model,
    umbra::forms::Form,
)]
#[umbra(table = "fmc_post")]
pub struct Post {
    #[umbra(primary_key)]
    pub id: i64,
    #[form(required, length(min = 1, max = 200))]
    pub title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    pub labels: umbra::orm::M2M<Label>,
}

// ── models: Uuid-PK child ────────────────────────────────────────────────────

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "fmc_badge")]
pub struct Badge {
    #[umbra(primary_key)]
    pub id: uuid::Uuid,
    pub name: String,
}

#[derive(
    Debug,
    Clone,
    Default,
    sqlx::FromRow,
    serde::Serialize,
    serde::Deserialize,
    umbra::orm::Model,
    umbra::forms::Form,
)]
#[umbra(table = "fmc_entry")]
pub struct Entry {
    #[umbra(primary_key)]
    pub id: i64,
    #[form(required, length(min = 1, max = 200))]
    pub title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    pub badges: umbra::orm::M2M<Badge>,
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn data_multi_str(title: &str, label_codes: &[&str]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("title".to_string(), title.to_string());
    m.insert("labels".to_string(), label_codes.join(","));
    m
}

fn data_multi_uuid(title: &str, badge_ids: &[uuid::Uuid]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("title".to_string(), title.to_string());
    m.insert(
        "badges".to_string(),
        badge_ids
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(","),
    );
    m
}

// ── boot ──────────────────────────────────────────────────────────────────────

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
            .model::<Label>()
            .model::<Post>()
            .model::<Badge>()
            .model::<Entry>()
            .build()
            .expect("App::build");

        // String-PK child table + junction.
        sqlx::query(
            "CREATE TABLE fmc_label (code TEXT PRIMARY KEY, name TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("create fmc_label");
        sqlx::query(
            "CREATE TABLE fmc_post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("create fmc_post");
        sqlx::query(
            "CREATE TABLE fmc_post_labels (\
                parent_id INTEGER NOT NULL, \
                child_id TEXT NOT NULL, \
                PRIMARY KEY (parent_id, child_id)\
            )",
        )
        .execute(&pool)
        .await
        .expect("create fmc_post_labels junction");

        for (code, name) in &[("rust", "Rust"), ("go", "Go"), ("ts", "TypeScript")] {
            sqlx::query("INSERT INTO fmc_label (code, name) VALUES (?, ?)")
                .bind(*code)
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed label");
        }

        // Uuid-PK child table + junction.
        sqlx::query(
            "CREATE TABLE fmc_badge (id TEXT PRIMARY KEY, name TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("create fmc_badge");
        sqlx::query(
            "CREATE TABLE fmc_entry (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("create fmc_entry");
        sqlx::query(
            "CREATE TABLE fmc_entry_badges (\
                parent_id INTEGER NOT NULL, \
                child_id TEXT NOT NULL, \
                PRIMARY KEY (parent_id, child_id)\
            )",
        )
        .execute(&pool)
        .await
        .expect("create fmc_entry_badges junction");

        let badge_a = uuid::Uuid::from_u128(0xAAAA_0001);
        let badge_b = uuid::Uuid::from_u128(0xBBBB_0002);
        let badge_c = uuid::Uuid::from_u128(0xCCCC_0003);
        // Bind the Uuid typed (not as a string) so sqlx encodes it as
        // a BLOB — matching how sea_query / the ORM stores UUID values
        // in SQLite (sqlx Encode<Sqlite> for Uuid uses .as_bytes()).
        for (id, name) in &[(badge_a, "Gold"), (badge_b, "Silver"), (badge_c, "Bronze")] {
            sqlx::query("INSERT INTO fmc_badge (id, name) VALUES (?, ?)")
                .bind(*id)
                .bind(*name)
                .execute(&pool)
                .await
                .expect("seed badge");
        }
    })
    .await;
}

async fn junction_string_child_ids(parent_id: i64) -> Vec<String> {
    let pool = db::pool();
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT child_id FROM fmc_post_labels WHERE parent_id = ? ORDER BY child_id",
    )
    .bind(parent_id)
    .fetch_all(&pool)
    .await
    .expect("read fmc_post_labels junction");
    rows.into_iter().map(|(c,)| c).collect()
}

async fn junction_uuid_child_ids(parent_id: i64) -> Vec<String> {
    let pool = db::pool();
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT child_id FROM fmc_entry_badges WHERE parent_id = ? ORDER BY child_id",
    )
    .bind(parent_id)
    .fetch_all(&pool)
    .await
    .expect("read fmc_entry_badges junction");
    rows.into_iter().map(|(c,)| c).collect()
}

// ── tests: String-PK child ───────────────────────────────────────────────────

/// A form selecting two String-PK labels must write exactly those two
/// junction rows. Before the fix, `pk_string_to_sea_value` was only reached
/// for Text/Uuid — but the fallback `unwrap_or(SqlType::BigInt)` meant
/// non-Text/Uuid PK columns fell into the i64 arm, causing `.parse::<i64>()`
/// to fail on `"rust"` and silently dropping all ids.
#[tokio::test]
async fn m2m_form_string_pk_child_writes_junction_rows() {
    boot().await;
    let post = Post::validate(&data_multi_str("String-PK Test", &["rust", "go"]))
        .await
        .expect("form validated — rust and go exist");
    let created = Post::objects()
        .create(post)
        .await
        .expect("create post");
    let ids = junction_string_child_ids(created.id).await;
    assert_eq!(
        ids,
        vec!["go".to_string(), "rust".to_string()],
        "exactly the two selected String-PK child ids appear as junction rows (order: alphabetical from ORDER BY)"
    );
}

/// A form with a bad String-PK child id must fail validation and write
/// zero junction rows (same atomicity contract as the i64 path).
#[tokio::test]
async fn m2m_form_string_pk_child_bad_id_fails_validation() {
    boot().await;
    let err = Post::validate(&data_multi_str("String-PK-Bad", &["rust", "nonexistent-label"]))
        .await
        .expect_err("nonexistent label must fail validation");
    assert!(
        err.fields.contains_key("labels"),
        "validation error keyed to the labels field: {:?}",
        err.fields
    );
    // No parent row created.
    let count = Post::objects()
        .filter(post::TITLE.eq("String-PK-Bad"))
        .count()
        .await
        .expect("count");
    assert_eq!(count, 0, "no parent row on a failed m2m validation");
}

// ── tests: Uuid-PK child ─────────────────────────────────────────────────────

/// Same guarantee for a Uuid-PK child. The form submits UUIDs as their
/// lowercase-hyphenated string representation (how `Uuid::to_string()` and
/// HTML inputs both produce them). The junction write must store those UUIDs
/// as TEXT child_id rows.
#[tokio::test]
async fn m2m_form_uuid_pk_child_writes_junction_rows() {
    boot().await;
    let badge_a = uuid::Uuid::from_u128(0xAAAA_0001);
    let badge_b = uuid::Uuid::from_u128(0xBBBB_0002);
    let entry = Entry::validate(&data_multi_uuid("Uuid-PK Test", &[badge_a, badge_b]))
        .await
        .expect("form validated — both badges exist");
    let created = Entry::objects()
        .create(entry)
        .await
        .expect("create entry");
    let ids = junction_uuid_child_ids(created.id).await;
    let mut expected = vec![badge_a.to_string(), badge_b.to_string()];
    expected.sort();
    assert_eq!(
        ids, expected,
        "exactly the two selected Uuid-PK child ids appear as junction rows"
    );
}

/// A form with a bad Uuid-PK child id (unknown UUID) must fail validation
/// and write zero junction rows.
#[tokio::test]
async fn m2m_form_uuid_pk_child_bad_id_fails_validation() {
    boot().await;
    let badge_a = uuid::Uuid::from_u128(0xAAAA_0001);
    let nonexistent = uuid::Uuid::from_u128(0xDEAD_BEEF);
    let err = Entry::validate(&data_multi_uuid("Uuid-PK-Bad", &[badge_a, nonexistent]))
        .await
        .expect_err("nonexistent badge must fail validation");
    assert!(
        err.fields.contains_key("badges"),
        "validation error keyed to the badges field: {:?}",
        err.fields
    );
    let count = Entry::objects()
        .filter(entry::TITLE.eq("Uuid-PK-Bad"))
        .count()
        .await
        .expect("count");
    assert_eq!(count, 0, "no parent row on a failed m2m validation");
}

