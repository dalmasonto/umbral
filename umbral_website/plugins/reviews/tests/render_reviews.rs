//! Render smoke-test for the DB-driven `/reviews` page.

use std::path::PathBuf;

use reviews::models::Review;
use reviews::render_reviews;
use umbral::migrate::ModelMeta;
use umbral::orm::Model;
use umbral::plugin::{Plugin as PluginTrait, PluginError};

#[derive(Debug, Default, Clone)]
struct TemplatesOnly;

impl PluginTrait for TemplatesOnly {
    fn name(&self) -> &'static str {
        "reviews_templates_test"
    }
    fn models(&self) -> Vec<ModelMeta> {
        Vec::new()
    }
    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }
    fn on_ready(&self, _ctx: &umbral::plugin::AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

async fn boot() {
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    let site_templates = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("templates");

    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Review>()
        .templates_dir(site_templates)
        .plugin(TemplatesOnly::default())
        .build()
        .expect("App::build");

    sqlx::query(&format!(
        "CREATE TABLE {t} (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            author INTEGER, rating INTEGER NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL,
            role TEXT, company TEXT, umbral_version TEXT, usage_context TEXT NOT NULL,
            verified_developer INTEGER NOT NULL DEFAULT 0, moderation TEXT NOT NULL,
            featured INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
        )",
        t = Review::TABLE
    ))
    .execute(&pool)
    .await
    .expect("CREATE TABLE");

    reviews::seed::seed().await.expect("seed reviews");
}

#[tokio::test]
async fn reviews_render_approved_testimonials() {
    boot().await;
    let html = render_reviews().await.expect("reviews render");

    assert!(
        html.contains("Familiar workflow, Rust guarantees"),
        "a seeded review title renders"
    );
    assert!(
        html.contains("Staff Engineer"),
        "the reviewer byline renders"
    );
    assert!(
        html.contains("Verified developer"),
        "the verified badge renders"
    );
}
