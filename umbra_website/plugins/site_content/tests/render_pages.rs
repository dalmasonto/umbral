//! Render smoke-tests for the site_content pages (`/docs`, `/changelog`,
//! `/blog`). Catches Jinja template errors that `cargo check` cannot —
//! these would otherwise surface as a 500 at request time.

use std::path::PathBuf;

use site_content::models::BlogPost;
use site_content::render_blog;
use umbra::migrate::ModelMeta;
use umbra::orm::Model;
use umbra::plugin::{Plugin as PluginTrait, PluginError};
use umbra::templates::context;

#[derive(Debug, Default, Clone)]
struct TemplatesOnly;

impl PluginTrait for TemplatesOnly {
    fn name(&self) -> &'static str {
        "site_content_templates_test"
    }
    fn models(&self) -> Vec<ModelMeta> {
        Vec::new()
    }
    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }
    fn on_ready(&self, _ctx: &umbra::plugin::AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

async fn boot() {
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    let site_templates = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("templates");

    umbra::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<BlogPost>()
        .templates_dir(site_templates)
        .plugin(TemplatesOnly::default())
        .build()
        .expect("App::build");

    // /blog queries blog_post; the others are static renders.
    sqlx::query(&format!(
        "CREATE TABLE {t} (
            id INTEGER PRIMARY KEY AUTOINCREMENT, public_id TEXT NOT NULL, slug TEXT NOT NULL,
            title TEXT NOT NULL, excerpt TEXT, body TEXT NOT NULL, status TEXT NOT NULL,
            kind TEXT NOT NULL, author INTEGER, category INTEGER, cover_image_url TEXT,
            attachment_url TEXT, seo_title TEXT, seo_description TEXT,
            reading_minutes INTEGER NOT NULL DEFAULT 0, view_count INTEGER NOT NULL DEFAULT 0,
            featured INTEGER NOT NULL DEFAULT 0, published_at TEXT,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
        )",
        t = BlogPost::TABLE
    ))
    .execute(&pool)
    .await
    .expect("CREATE TABLE");
}

#[tokio::test]
async fn site_content_pages_render() {
    boot().await;

    // Static pages: render the templates directly (no DB).
    let docs = umbra::templates::render("site_content/docs.html", &context! {})
        .expect("docs renders");
    assert!(docs.contains("Learn Umbra"), "docs hero renders");
    assert!(docs.contains("Migrations"), "a docs topic card renders");

    let changelog = umbra::templates::render("site_content/changelog.html", &context! {})
        .expect("changelog renders");
    assert!(changelog.contains("v0.0.1"), "the current release renders");
    assert!(changelog.contains("Roadmap"), "the roadmap section renders");

    // /blog with no published posts → the honest empty state.
    let blog = render_blog().await.expect("blog renders");
    assert!(blog.contains("No posts yet"), "the empty state renders");
}
