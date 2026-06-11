//! Behavioral coverage for FK / forward-O2O form fields. The Form
//! derive classifies `ForeignKey<T>` (and forward `OneToOne<T>`) into a
//! `ModelChoice`: validate() parses the submitted id, an existence
//! probe verifies a live parent (Task 5), and render fetches options
//! (Task 6). Every test drives the real path against an in-memory
//! SQLite DB and reads the object graph back.

#![allow(dead_code)]
use std::collections::HashMap;
use tokio::sync::OnceCell;
use umbra::forms::FormValidate;
use umbra::orm::{ForeignKey, Model};
use umbra_core::db;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "ffk_author")]
struct Author {
    pub id: i64,
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
#[umbra(table = "ffk_book")]
struct Book {
    #[umbra(primary_key)]
    pub id: i64,
    #[form(required, length(min = 1, max = 200))]
    pub title: String,
    pub author: ForeignKey<Author>,
}

fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

static BOOT: OnceCell<()> = OnceCell::const_new();
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Author>()
            .model::<Book>()
            .model::<Passport>()
            .build()
            .expect("App::build");
        sqlx::query("CREATE TABLE ffk_author (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
            .execute(&pool).await.expect("create author");
        sqlx::query("CREATE TABLE ffk_book (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, author INTEGER NOT NULL REFERENCES ffk_author(id))")
            .execute(&pool).await.expect("create book");
        sqlx::query("INSERT INTO ffk_author (name) VALUES ('Ada')")
            .execute(&pool).await.expect("seed author");
    })
    .await;
}

#[tokio::test]
async fn fk_field_parses_and_links_real_parent() {
    boot().await;
    let book = Book::validate(&data(&[("title", "Notes"), ("author", "1")]))
        .await
        .expect("valid FK");
    // The parsed FK carries the submitted id.
    assert_eq!(book.author.id(), 1);
    // Persist + read the parent back through the ORM.
    let created = Book::objects().create(book).await.expect("create book");
    let parent = created
        .author
        .resolve(&db::pool())
        .await
        .expect("resolve parent");
    assert_eq!(
        parent.name, "Ada",
        "FK resolves to the actual seeded parent"
    );
}

// Forward O2O is a unique FK — a duplicate target surfaces as a
// WriteError from the DB UNIQUE constraint, not a silent second row.
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
#[umbra(table = "ffk_passport")]
struct Passport {
    #[umbra(primary_key)]
    pub id: i64,
    #[umbra(unique)]
    pub holder: ForeignKey<Author>,
    #[form(required, length(min = 1, max = 40))]
    pub number: String,
}

#[tokio::test]
async fn fk_field_renders_select_with_seeded_options() {
    boot().await;
    let html = Book::render_html(&data(&[])).await;
    // The author <select> carries the seeded parent as an option.
    assert!(
        html.contains("<select name=\"author\""),
        "renders a select: {html}"
    );
    assert!(
        html.contains("value=\"1\""),
        "seeded author id is an option: {html}"
    );
    assert!(
        html.contains("Ada"),
        "label is the parent's text column: {html}"
    );
}

#[tokio::test]
async fn fk_field_rejects_nonexistent_parent_and_inserts_no_row() {
    boot().await;
    let before = Book::objects().count().await.expect("count before");
    let err = Book::validate(&data(&[("title", "Ghost"), ("author", "9999")]))
        .await
        .expect_err("nonexistent FK rejected");
    assert!(
        err.fields.contains_key("author"),
        "error keyed to the FK field"
    );
    let after = Book::objects().count().await.expect("count after");
    assert_eq!(before, after, "no row inserted on a bad FK");
}

#[tokio::test]
async fn forward_o2o_unique_violation_surfaces_as_write_error() {
    boot().await;
    sqlx::query("CREATE TABLE IF NOT EXISTS ffk_passport (id INTEGER PRIMARY KEY AUTOINCREMENT, holder INTEGER NOT NULL UNIQUE REFERENCES ffk_author(id), number TEXT NOT NULL)")
        .execute(&db::pool()).await.expect("create passport");
    let p1 = Passport::validate(&data(&[("holder", "1"), ("number", "A1")]))
        .await
        .expect("valid o2o");
    Passport::objects().create(p1).await.expect("first o2o row");
    let p2 = Passport::validate(&data(&[("holder", "1"), ("number", "B2")]))
        .await
        .expect("validates (existence ok); UNIQUE fires at insert");
    let err = Passport::objects()
        .create(p2)
        .await
        .expect_err("duplicate target");
    // A unique violation, not a silent second row.
    assert!(
        matches!(
            err,
            umbra::orm::write::WriteError::UniqueViolation { .. }
                | umbra::orm::write::WriteError::Multiple { .. }
                | umbra::orm::write::WriteError::Sqlx(_)
        ),
        "duplicate forward-O2O surfaces a WriteError: {err:?}"
    );
}
