//! Render + create smoke-test for the `/sponsor` page.

use std::collections::HashMap;
use std::path::PathBuf;

use sponsor::models::{InquiryStatus, SponsorInquiry};
use sponsor::{create_inquiry, render_sponsor};
use umbral::migrate::ModelMeta;
use umbral::orm::Model;
use umbral::plugin::{Plugin as PluginTrait, PluginError};

#[derive(Debug, Default, Clone)]
struct TemplatesOnly;

impl PluginTrait for TemplatesOnly {
    fn name(&self) -> &'static str {
        "sponsor_templates_test"
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

// `App::build` calls the process-global `settings::init` exactly once, so
// every test in this file shares ONE boot (and one in-memory pool + table).
static BOOT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
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
            .model::<SponsorInquiry>()
            .templates_dir(site_templates)
            .plugin(TemplatesOnly::default())
            .build()
            .expect("App::build");

        sqlx::query(&format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                email TEXT NOT NULL,
                organization TEXT,
                interest TEXT,
                message TEXT NOT NULL,
                status TEXT NOT NULL,
                ip_address TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            )",
            t = SponsorInquiry::TABLE
        ))
        .execute(&pool)
        .await
        .expect("CREATE TABLE");
    })
    .await;
}

#[tokio::test]
async fn sponsor_page_renders_form_and_github() {
    boot().await;
    // Partners table intentionally absent — render_sponsor must degrade to
    // the empty state, not 500.
    let html = render_sponsor(false, None, &HashMap::new())
        .await
        .expect("sponsor page renders");

    assert!(html.contains("Talk to us"), "the inquiry form renders");
    assert!(
        html.contains("github.com/sponsors/dalmasonto"),
        "the GitHub Sponsors link renders"
    );
    assert!(
        html.contains("No partners listed yet"),
        "the honest empty state renders when there are no partners"
    );
}

#[tokio::test]
async fn valid_inquiry_is_created_as_new() {
    boot().await;
    let data: HashMap<String, String> = [
        ("name", "Ada Partner"),
        ("email", "ada@example.com"),
        ("organization", "Analytical Engines Ltd"),
        ("message", "We'd love to sponsor the docs effort — let's talk."),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    let id = create_inquiry(&data).await.expect("valid inquiry created");
    assert!(id > 0, "a row id comes back");

    let row = SponsorInquiry::objects()
        .filter(sponsor::models::sponsor_inquiry::ID.eq(id))
        .first()
        .await
        .expect("query ok")
        .expect("row exists");
    assert_eq!(row.status, InquiryStatus::New, "status defaults to New");
    assert_eq!(row.email, "ada@example.com");
}

#[tokio::test]
async fn invalid_inquiry_is_rejected() {
    boot().await;
    let data: HashMap<String, String> = [("name", "x"), ("email", "not-an-email"), ("message", "short")]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let err = create_inquiry(&data).await.expect_err("invalid inquiry rejected");
    assert!(
        err.fields.contains_key("email") || err.fields.contains_key("message"),
        "validation errors are keyed to the offending fields"
    );
}
