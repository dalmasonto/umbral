//! Render smoke-test for the DB-driven `/showcase` gallery.

use std::path::PathBuf;

use showcase::models::ShowcaseEntry;
use showcase::render_showcase;
use umbra::migrate::ModelMeta;
use umbra::orm::Model;
use umbra::plugin::{Plugin as PluginTrait, PluginError};

#[derive(Debug, Default, Clone)]
struct TemplatesOnly;

impl PluginTrait for TemplatesOnly {
    fn name(&self) -> &'static str {
        "showcase_templates_test"
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
        .model::<ShowcaseEntry>()
        .templates_dir(site_templates)
        .plugin(TemplatesOnly::default())
        .build()
        .expect("App::build");

    sqlx::query(&format!(
        "CREATE TABLE {t} (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            submitted_by INTEGER, project_name TEXT NOT NULL, url TEXT NOT NULL, owner TEXT NOT NULL,
            short_description TEXT NOT NULL, long_content TEXT, screenshot_url TEXT, logo_url TEXT,
            project_type TEXT NOT NULL, plugins_used TEXT, database_backend TEXT NOT NULL,
            deployment_platform TEXT NOT NULL, launch_date TEXT, source_url TEXT,
            verified INTEGER NOT NULL DEFAULT 0, featured INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
        )",
        t = ShowcaseEntry::TABLE
    ))
    .execute(&pool)
    .await
    .expect("CREATE TABLE");

    showcase::seed::seed().await.expect("seed showcase");
}

#[tokio::test]
async fn showcase_renders_dogfooding_entries() {
    boot().await;
    let html = render_showcase().await.expect("showcase render");

    assert!(
        html.contains("Umbra Plugin Directory"),
        "a seeded showcase entry renders"
    );
    assert!(html.contains("Shop Example"), "the shop demo entry renders");
    assert!(
        html.contains("Featured"),
        "the featured badge renders"
    );
    // A parsed plugin tag from the comma-separated field.
    assert!(html.contains("admin"), "a plugin tag renders");
}
