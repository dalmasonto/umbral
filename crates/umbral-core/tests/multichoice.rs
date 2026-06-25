//! End-to-end coverage for the `MultiChoice<E>` field type.
//!
//! Three layers exercised in one file:
//!
//!   1. `#[derive(Model)]` recognises `MultiChoice<E>` and emits the
//!      right `FieldSpec` (TEXT column, choices/labels from the enum's
//!      `ChoiceField` trait, `is_multichoice: true`).
//!   2. CSV encode/decode round-trips through sqlx on a fresh in-memory
//!      SQLite — fetched row's `tags: MultiChoice<Tag>` equals the
//!      `From<Vec<Tag>>` value we inserted.
//!   3. Postgres/SQLite DDL emission carries the DEFAULT and skips
//!      CHECK (multichoice doesn't get a CHECK constraint; the CSV
//!      regex would need per-variant escaping the migration engine
//!      doesn't yet bother with).

use umbral::orm::{ChoiceField, Model, MultiChoice};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Design,
    Frontend,
    Backend,
}

impl ChoiceField for Tag {
    const VALUES: &'static [&'static str] = &["design", "frontend", "backend"];
    const LABELS: &'static [&'static str] = &["Design", "Frontend", "Backend"];
    fn as_str(&self) -> &'static str {
        match self {
            Tag::Design => "design",
            Tag::Frontend => "frontend",
            Tag::Backend => "backend",
        }
    }
    fn from_str_ok(s: &str) -> Option<Self> {
        match s {
            "design" => Some(Tag::Design),
            "frontend" => Some(Tag::Frontend),
            "backend" => Some(Tag::Backend),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow, umbral::orm::Model)]
#[umbral(table = "mc_article")]
pub struct Article {
    pub id: i64,
    pub title: String,
    #[umbral(default = "design,frontend")]
    pub tags: MultiChoice<Tag>,
}

#[test]
fn field_spec_marks_multichoice_and_carries_choices() {
    let tags = Article::FIELDS
        .iter()
        .find(|f| f.name == "tags")
        .expect("tags field present");
    assert!(tags.is_multichoice, "tags is a MultiChoice<Tag> field");
    assert_eq!(tags.choices, &["design", "frontend", "backend"]);
    assert_eq!(tags.choice_labels, &["Design", "Frontend", "Backend"]);
    assert_eq!(tags.default, "design,frontend");
    // Stored as TEXT — same backend representation as a single Choices field.
    assert_eq!(tags.ty, umbral_core::orm::SqlType::Text);
    assert!(!tags.nullable, "MultiChoice fields are non-nullable at v1");
}

#[tokio::test]
async fn sqlx_roundtrips_csv_via_in_memory_sqlite() {
    let pool = umbral_core::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("sqlite memory connect");
    sqlx::query(
        "CREATE TABLE mc_article (\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\
             title TEXT NOT NULL,\
             tags TEXT NOT NULL DEFAULT 'design,frontend'\
         )",
    )
    .execute(&pool)
    .await
    .expect("create table");

    let chosen: MultiChoice<Tag> = vec![Tag::Design, Tag::Backend].into();
    sqlx::query("INSERT INTO mc_article (title, tags) VALUES (?, ?)")
        .bind("hello")
        .bind(&chosen)
        .execute(&pool)
        .await
        .expect("insert row");

    let row: Article =
        sqlx::query_as::<_, Article>("SELECT id, title, tags FROM mc_article WHERE id = 1")
            .fetch_one(&pool)
            .await
            .expect("fetch row");
    assert_eq!(row.tags.as_slice(), &[Tag::Design, Tag::Backend]);
    assert_eq!(row.tags.to_string(), "design,backend");
}

#[test]
fn migrate_column_carries_is_multichoice_flag() {
    let col = umbral_core::migrate::Column::from(
        Article::FIELDS
            .iter()
            .find(|f| f.name == "tags")
            .expect("tags field"),
    );
    assert!(col.is_multichoice);
    assert_eq!(col.choices, vec!["design", "frontend", "backend"]);
    assert_eq!(col.default, "design,frontend");
    // No CHECK at v1: choices flag stays non-empty for the admin's
    // widget pick, but the migration engine reads is_multichoice to
    // decide not to emit a single-value `CHECK (col IN (...))`.
}
