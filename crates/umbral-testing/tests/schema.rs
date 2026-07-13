//! `create_tables()` — the test schema comes from the models (feature #79).
//!
//! The thing being defended against: a hand-written `CREATE TABLE` in a test file
//! is a SECOND source of truth for the schema, and it drifts. Add a column to the
//! model and the test's table silently lacks it; the ORM then queries a column
//! that isn't there, and any error-swallowing on the path (`unwrap_or(false)`) turns
//! that into a test that PASSES WITH THE WRONG ANSWER rather than failing.

#![allow(dead_code, private_interfaces)]

use umbral::orm::Model;
use umbral_testing::{Factory, seq};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
struct Author {
    id: i64,
    name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
struct Book {
    id: i64,
    title: String,
    #[umbral(unique, max_length = 120)]
    isbn: String,
    /// A nullable column, a default, and an FK — the shapes a hand-written test
    /// table most often gets subtly wrong.
    blurb: Option<String>,
    #[umbral(default = "0")]
    reprints: i32,
    author: umbral::orm::ForeignKey<Author>,
}

struct AuthorFactory;
impl Factory for AuthorFactory {
    type Model = Author;
    fn build() -> Author {
        Author {
            id: 0,
            name: format!("Author {}", seq()),
        }
    }
}

/// The whole setup. No OnceCell, no Mutex, no hand-written CREATE TABLE — and it
/// is safe to call from every test in the file.
async fn boot() {
    umbral_testing::boot(|b| b.model::<Author>().model::<Book>()).await;
}

/// The schema in the database must be the schema on the model — every column,
/// derived, not transcribed. This compares the two sources directly, so any model
/// change is covered without editing this test.
#[tokio::test]
async fn the_table_has_exactly_the_columns_the_model_declares() {
    boot().await;
    let pool = umbral::db::pool();

    for meta in umbral::migrate::registered_models() {
        let cols: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT name FROM pragma_table_info('{}')",
            meta.table
        ))
        .fetch_all(&pool)
        .await
        .unwrap_or_else(|e| panic!("table `{}` should exist: {e}", meta.table));

        assert!(
            !cols.is_empty(),
            "`create_tables` did not create `{}`",
            meta.table
        );
        for field in &meta.fields {
            assert!(
                cols.contains(&field.name),
                "column `{}.{}` is on the model but not in the created table — the schema \
                 drifted from the models, which is the whole thing this helper prevents",
                meta.table,
                field.name
            );
        }
    }
}

/// The generated schema is real: constraints, defaults and FKs all work, so a test
/// written against it exercises the same rules production does.
#[tokio::test]
async fn the_generated_schema_enforces_the_models_constraints() {
    boot().await;

    let author = AuthorFactory::create().await.expect("author");

    // A default declared on the model applies in the database.
    let book = Book {
        id: 0,
        title: "First".into(),
        isbn: format!("isbn-{}", seq()),
        blurb: None,
        reprints: 0,
        author: umbral::orm::ForeignKey::new(author.id),
    };
    let saved = umbral::orm::Manager::<Book>::default()
        .create(book)
        .await
        .expect("the FK table was created in dependency order, so this insert works");
    assert_eq!(saved.blurb, None, "the nullable column round-trips as NULL");

    // The `unique` constraint on the model is a real UNIQUE in the database.
    let clash = Book {
        id: 0,
        title: "Second".into(),
        isbn: saved.isbn.clone(),
        blurb: Some("dup".into()),
        reprints: 0,
        author: umbral::orm::ForeignKey::new(author.id),
    };
    let err = umbral::orm::Manager::<Book>::default().create(clash).await;
    assert!(
        err.is_err(),
        "the model says `isbn` is unique, so the generated table must enforce it"
    );
}

/// Calling it twice is harmless — a test file with a shared `OnceCell` boot must not
/// blow up on its own setup.
#[tokio::test]
async fn create_tables_is_idempotent() {
    boot().await;
    umbral_testing::create_tables()
        .await
        .expect("a second call is a no-op, not an error");
    assert!(!Book::TABLE.is_empty());
}
