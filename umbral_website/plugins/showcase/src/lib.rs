//! ShowcasePlugin — the `/showcase` gallery of projects built with Umbral.
//!
//! Renders verified/featured `ShowcaseEntry` rows. Greenfield-honest: the
//! seed is dogfooding only (the framework's own properties); real
//! third-party projects arrive via the submission form.

pub mod models;
pub mod seed;

pub use models::{
    ShowcaseDatabase, ShowcaseDeployment, ShowcaseEntry, ShowcaseProjectType, ShowcaseStatus,
};

use std::path::PathBuf;

use serde::Serialize;
use umbral::migrate::ModelMeta;
use umbral::plugin::{AppContext, Plugin, PluginError};
use umbral::templates::context;
use umbral::web::{Html, Router, StatusCode, get};

use models::showcase_entry;

#[derive(Debug, Default, Clone)]
pub struct ShowcasePlugin;

impl Plugin for ShowcasePlugin {
    fn name(&self) -> &'static str {
        "showcase"
    }

    /// FKs into `auth_user`. Held by alphabetical luck before; now declared.
    fn dependencies(&self) -> &'static [&'static str] {
        &["auth"]
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![ModelMeta::for_::<models::ShowcaseEntry>()]
    }

    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }

    fn routes(&self) -> Router {
        Router::new().route("/showcase", get(showcase_page))
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        tokio::spawn(async move {
            match seed::seed().await {
                Ok(0) => {}
                Ok(n) => tracing::info!("showcase: seeded {n} entries"),
                Err(e) => tracing::warn!("showcase: seed failed: {e}"),
            }
        });
        Ok(())
    }
}

/// One project card on `/showcase`.
#[derive(Debug, Serialize)]
struct EntryView {
    project_name: String,
    url: String,
    owner: String,
    short_description: String,
    /// Up to ~6 plugin tags parsed from the comma-separated field.
    plugins: Vec<String>,
    project_type: String,
    backend: String,
    featured: bool,
    verified: bool,
    initials: String,
}

fn type_label(t: ShowcaseProjectType) -> &'static str {
    match t {
        ShowcaseProjectType::Website => "Website",
        ShowcaseProjectType::Dashboard => "Dashboard",
        ShowcaseProjectType::ApiService => "API service",
        ShowcaseProjectType::InternalTool => "Internal tool",
        ShowcaseProjectType::MobileBackend => "Mobile backend",
        ShowcaseProjectType::Demo => "Demo",
        ShowcaseProjectType::Other => "Project",
    }
}

fn backend_label(b: ShowcaseDatabase) -> &'static str {
    match b {
        ShowcaseDatabase::Sqlite => "SQLite",
        ShowcaseDatabase::Postgres => "PostgreSQL",
        ShowcaseDatabase::Mysql => "MySQL",
        ShowcaseDatabase::Other => "Other DB",
    }
}

fn initials(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().filter(|w| !w.is_empty()).collect();
    let out: String = match words.as_slice() {
        [] => "??".to_string(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
    };
    out.to_uppercase()
}

async fn showcase_page() -> Result<Html<String>, (StatusCode, String)> {
    render_showcase()
        .await
        .map(Html)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Load + render `/showcase`: verified/featured entries, featured first.
/// Public so a render smoke-test can drive it without an axum runtime.
pub async fn render_showcase() -> Result<String, String> {
    let entries: Vec<EntryView> = ShowcaseEntry::objects()
        .filter(showcase_entry::VERIFIED.eq(true))
        .order_by(showcase_entry::FEATURED.desc())
        .order_by(showcase_entry::ID.asc())
        .fetch()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|e| {
            let plugins: Vec<String> = e
                .plugins_used
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .take(6)
                .collect();
            EntryView {
                initials: initials(&e.project_name),
                project_name: e.project_name,
                url: e.url,
                owner: e.owner,
                short_description: e.short_description,
                plugins,
                project_type: type_label(e.project_type).to_string(),
                backend: backend_label(e.database_backend).to_string(),
                featured: e.featured,
                verified: e.verified,
            }
        })
        .collect();

    umbral::templates::render("showcase/showcase.html", &context! { entries => entries })
        .map_err(|e| e.to_string())
}
