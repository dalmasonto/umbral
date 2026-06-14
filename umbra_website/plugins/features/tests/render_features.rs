//! Render smoke-test for the DB-driven `/features` catalog.

use std::path::PathBuf;

use features::models::{FeatureCategory, FeatureStatusEvent, FrameworkFeature};
use features::render_features;
use umbra::migrate::ModelMeta;
use umbra::orm::Model;
use umbra::plugin::{Plugin as PluginTrait, PluginError};

#[derive(Debug, Default, Clone)]
struct TemplatesOnly;

impl PluginTrait for TemplatesOnly {
    fn name(&self) -> &'static str {
        "features_templates_test"
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
        .model::<FeatureCategory>()
        .model::<FrameworkFeature>()
        .model::<FeatureStatusEvent>()
        .templates_dir(site_templates)
        .plugin(TemplatesOnly::default())
        .build()
        .expect("App::build");

    ensure_tables(&pool).await;
    features::seed::seed().await.expect("seed features");
}

async fn ensure_tables(pool: &sqlx::SqlitePool) {
    let stmts = [
        format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL, slug TEXT NOT NULL, description TEXT,
                display_order INTEGER NOT NULL DEFAULT 0, visible INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
            )",
            t = FeatureCategory::TABLE
        ),
        format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                category INTEGER NOT NULL REFERENCES {ct}(id),
                name TEXT NOT NULL, slug TEXT NOT NULL, short_summary TEXT NOT NULL,
                full_description TEXT NOT NULL, status TEXT NOT NULL, maturity TEXT NOT NULL,
                docs_url TEXT, example_url TEXT, related_plugin_slug TEXT, release_target TEXT,
                display_order INTEGER NOT NULL DEFAULT 0, visible INTEGER NOT NULL DEFAULT 1,
                metadata TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted_at TEXT
            )",
            t = FrameworkFeature::TABLE,
            ct = FeatureCategory::TABLE
        ),
    ];
    for sql in stmts {
        sqlx::query(&sql).execute(pool).await.expect("CREATE TABLE");
    }
}

#[tokio::test]
async fn features_render_categories_and_status() {
    boot().await;
    let html = render_features().await.expect("features render");

    // A seeded category and one of its features render.
    assert!(
        html.contains("ORM &amp; Migrations") || html.contains("ORM & Migrations"),
        "a category heading renders"
    );
    assert!(html.contains("QuerySet builder"), "a feature renders");
    assert!(
        html.contains("shipped"),
        "a shipped feature's status label renders"
    );
    // A planned feature carries the muted label.
    assert!(
        html.contains("planned"),
        "a planned feature's status label renders"
    );
    // gaps2 #58: the "same struct for models, forms & serializers" message
    // surfaces — both the editorial callout and the seeded catalog entry.
    assert!(
        html.contains("One struct"),
        "the one-struct differentiator is surfaced on /features"
    );
    assert!(
        html.contains("Model, form,") && html.contains("serializer"),
        "the callout spells out the three roles of a single struct"
    );
}
