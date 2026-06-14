//! Render smoke-test for the DB-driven `/changelog` table.

use std::path::PathBuf;

use site_content::models::ChangelogEntry;
use site_content::render_changelog;
use umbra::migrate::ModelMeta;
use umbra::orm::Model;
use umbra::plugin::{Plugin as PluginTrait, PluginError};

#[derive(Debug, Default, Clone)]
struct TemplatesOnly;

impl PluginTrait for TemplatesOnly {
    fn name(&self) -> &'static str {
        "site_content_changelog_test"
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
        .model::<ChangelogEntry>()
        .templates_dir(site_templates)
        .plugin(TemplatesOnly::default())
        .build()
        .expect("App::build");

    sqlx::query(&format!(
        "CREATE TABLE {t} (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            version TEXT NOT NULL,
            title TEXT NOT NULL,
            body TEXT NOT NULL,
            kind TEXT NOT NULL,
            current INTEGER NOT NULL DEFAULT 0,
            released_at TEXT,
            display_order INTEGER NOT NULL DEFAULT 0,
            published INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            deleted_at TEXT
        )",
        t = ChangelogEntry::TABLE
    ))
    .execute(&pool)
    .await
    .expect("CREATE TABLE");

    let n = site_content::seed::seed_changelog().await.expect("seed changelog");
    assert_eq!(n, 2, "two changelog entries seeded");
}

#[tokio::test]
async fn changelog_renders_as_a_table_from_db() {
    boot().await;
    let html = render_changelog().await.expect("changelog renders");

    // The table shell + both seeded rows.
    assert!(html.contains("<table"), "the changelog renders as a table");
    assert!(html.contains("v0.0.1"), "the shipped version renders");
    assert!(html.contains("toward v0.1"), "the roadmap row renders");
    assert!(
        html.contains("The core loop, end to end"),
        "the entry title renders"
    );
    // Markdown body → a real list the table cell shows.
    assert!(html.contains("<li>"), "the markdown highlights render as a list");
    // Status pills + the current marker.
    assert!(html.contains("Released") && html.contains("Roadmap"), "status pills render");
    assert!(html.contains("Current"), "the current release is flagged");
    // The roadmap row has no date → the honest em-dash.
    assert!(html.contains("—"), "roadmap rows render the em-dash for no date");
}
