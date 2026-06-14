//! Render smoke-test for the DB-driven blog detail page (`/blog/{slug}`).
//!
//! Boots a minimal app, seeds the markdown posts, and renders one post to
//! prove: the markdown body renders to HTML (a fenced code block becomes a
//! `<pre>`), the `data-md` enhancer hook is present, and a 404 (Ok(None))
//! comes back for an unknown slug.

use std::path::PathBuf;

use site_content::models::BlogPost;
use site_content::render_blog_detail;
use umbra::migrate::ModelMeta;
use umbra::orm::Model;
use umbra::plugin::{Plugin as PluginTrait, PluginError};

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

    sqlx::query(&format!(
        "CREATE TABLE {t} (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            public_id TEXT NOT NULL,
            slug TEXT NOT NULL,
            title TEXT NOT NULL,
            excerpt TEXT,
            body TEXT NOT NULL,
            status TEXT NOT NULL,
            kind TEXT NOT NULL,
            author INTEGER,
            category INTEGER,
            cover_image_url TEXT,
            attachment_url TEXT,
            seo_title TEXT,
            seo_description TEXT,
            reading_minutes INTEGER NOT NULL DEFAULT 0,
            view_count INTEGER NOT NULL DEFAULT 0,
            featured INTEGER NOT NULL DEFAULT 0,
            published_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            deleted_at TEXT
        )",
        t = BlogPost::TABLE
    ))
    .execute(&pool)
    .await
    .expect("CREATE TABLE");

    let n = site_content::seed::seed().await.expect("seed blog");
    assert_eq!(n, 5, "five posts seeded");
}

#[tokio::test]
async fn blog_detail_renders_markdown_and_enhancer_hook() {
    boot().await;

    let html = render_blog_detail("why-umbra-exists")
        .await
        .expect("detail renders")
        .expect("published post found");

    assert!(html.contains("Why Umbra exists"), "the post title renders");
    assert!(
        html.contains("data-md"),
        "the markdown enhancer hook is present on the body container"
    );
    assert!(
        html.contains("<pre>") || html.contains("<pre "),
        "a fenced code block rendered to a <pre> the enhancer can wrap"
    );
    assert!(
        html.contains("<h2") && html.contains("</h2>"),
        "markdown headings rendered to real heading tags"
    );

    // Unknown slug → Ok(None) (a 404 at the handler), not an error.
    let missing = render_blog_detail("no-such-post")
        .await
        .expect("query ok");
    assert!(missing.is_none(), "unknown slug yields a 404, not a row");
}
