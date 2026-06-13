//! Factory layer (feature #79) — build/create/create_with/create_batch
//! against a real ORM model, plus `seq()` keeping `unique` columns from
//! colliding across a batch.

#![allow(dead_code, private_interfaces)]

use umbra_testing::fake::Fake;
use umbra_testing::fake::faker::lorem::en::Word;
use umbra_testing::{Factory, seq};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
struct Widget {
    id: i64,
    name: String,
    #[umbra(unique, max_length = 100)]
    slug: String,
    count: i32,
}

/// The factory marker — `impl Factory for WidgetFactory` is legal here
/// because the marker is local (the orphan rule would reject
/// `impl Factory for Widget` in a downstream crate, which is the whole
/// reason for the marker shape).
struct WidgetFactory;

impl Factory for WidgetFactory {
    type Model = Widget;
    fn build() -> Widget {
        Widget {
            id: 0,
            name: Word().fake(),
            // `seq()` makes the unique slug collision-free across a batch.
            slug: format!("widget-{}", seq()),
            count: (1..100i32).fake(),
        }
    }
}

async fn boot() {
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    umbra::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Widget>()
        .build()
        .expect("App::build");

    sqlx::query(
        "CREATE TABLE widget (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE,
            count INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("CREATE TABLE");
}

#[tokio::test]
async fn factory_build_create_and_batch() {
    boot().await;

    // build() is pure — a realistic, unsaved instance with a fresh id of 0.
    let built = WidgetFactory::build();
    assert_eq!(built.id, 0, "built instance is unsaved");
    assert!(built.slug.starts_with("widget-"), "slug uses seq()");

    // create() persists and returns the row with its assigned id.
    let created = WidgetFactory::create().await.expect("create persists");
    assert!(created.id > 0, "created row has a real id");

    // create_with() overrides a field before persisting.
    let big = WidgetFactory::create_with(|w| w.count = 999)
        .await
        .expect("create_with persists");
    assert_eq!(big.count, 999, "the override took effect");
    assert!(big.id > 0);

    // create_batch() persists N rows; seq() keeps the unique slug distinct,
    // so the UNIQUE constraint never trips.
    let batch = WidgetFactory::create_batch(5)
        .await
        .expect("batch persists without UNIQUE collision");
    assert_eq!(batch.len(), 5);

    // 1 (create) + 1 (create_with) + 5 (batch) = 7 rows landed in the DB.
    let total = Widget::objects().count().await.expect("count");
    assert_eq!(total, 7, "every factory row was persisted through the ORM");
}
