//! Render smoke-test for the DB-driven `/community` hub.
//!
//! Boots a minimal app against in-memory SQLite, registers the community
//! models + the site templates (`base.html`) + the plugin's own template,
//! seeds the channels / newsletter / lists through the ORM, then calls the
//! real `render_community` handler and asserts the seeded values render.

use std::path::PathBuf;

use community::models::{CommunityResource, NewsletterConfig, SocialLink};
use community::render_community;
use umbral::migrate::ModelMeta;
use umbral::orm::Model;
use umbral::plugin::{Plugin as PluginTrait, PluginError};

/// Contributes only the template dirs — we seed deterministically rather
/// than relying on command-driven seed data.
#[derive(Debug, Default, Clone)]
struct TemplatesOnly;

impl PluginTrait for TemplatesOnly {
    fn name(&self) -> &'static str {
        "community_templates_test"
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

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let site_templates = manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("templates");

    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<SocialLink>()
        .model::<CommunityResource>()
        .model::<NewsletterConfig>()
        .templates_dir(site_templates)
        .plugin(TemplatesOnly::default())
        .build()
        .expect("App::build");

    ensure_tables(&pool).await;
    community::seed::seed().await.expect("seed community");
}

async fn ensure_tables(pool: &sqlx::SqlitePool) {
    let stmts = [
        format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL, slug TEXT NOT NULL, platform TEXT NOT NULL,
                url TEXT NOT NULL, icon_key TEXT NOT NULL, description TEXT,
                color TEXT, coming_soon INTEGER NOT NULL DEFAULT 0,
                display_order INTEGER NOT NULL DEFAULT 0, active INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
            )",
            t = SocialLink::TABLE
        ),
        format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL, slug TEXT NOT NULL, kind TEXT NOT NULL,
                url TEXT NOT NULL, summary TEXT, is_featured INTEGER NOT NULL DEFAULT 0,
                display_order INTEGER NOT NULL DEFAULT 0, metadata TEXT,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
            )",
            t = CommunityResource::TABLE
        ),
        format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL, provider TEXT NOT NULL, hosted_subscribe_url TEXT NOT NULL,
                api_endpoint TEXT, list_id TEXT, success_redirect_url TEXT,
                failure_redirect_url TEXT, daily_digest_time TEXT, active INTEGER NOT NULL DEFAULT 1,
                metadata TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
            )",
            t = NewsletterConfig::TABLE
        ),
    ];
    for sql in stmts {
        sqlx::query(&sql).execute(pool).await.expect("CREATE TABLE");
    }
}

#[tokio::test]
async fn community_renders_channels_newsletter_and_lists() {
    boot().await;
    let html = render_community().await.expect("community renders");

    // Channels (from SocialLink): names + descriptions + a github href.
    assert!(html.contains("GitHub"), "the GitHub channel renders");
    assert!(html.contains("Discord"), "the Discord channel renders");
    assert!(
        html.contains("https://github.com/dalmasonto/umbral"),
        "a channel's seeded URL renders"
    );
    assert!(
        html.contains("Real-time chat"),
        "a channel's seeded description renders"
    );

    // Newsletter URL (from NewsletterConfig) drives the subscribe button.
    assert!(
        html.contains("sentinmail.app/subscribe/24479467"),
        "the seeded newsletter subscribe URL renders"
    );

    // Newsletter lists (from CommunityResource kind=newsletter).
    assert!(
        html.contains("The Umbral Monthly"),
        "a seeded newsletter list title renders"
    );
    assert!(
        html.contains("real-world showcases"),
        "a seeded newsletter list summary renders"
    );
}
