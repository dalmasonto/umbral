//! Official Umbra website.
//!
//! The website intentionally dogfoods Umbra's project/app shape:
//! `main.rs` wires framework plugins, website apps, templates, and routes.
//! Database models live in `plugins/*/src/models.rs`, not in this file.

use accounts::AccountsPlugin;
use community::CommunityPlugin;
use features::FeaturesPlugin;
use plugin_directory::PluginDirectoryPlugin;
use public::PublicPlugin;
use reviews::ReviewsPlugin;
use security_reports::SecurityReportsPlugin;
use showcase::ShowcasePlugin;
use site_content::SiteContentPlugin;
use umbra::prelude::*;
use umbra::templates::context;
use umbra::web::{Html, SlashRedirect, StatusCode};
use umbra_admin::AdminPlugin;
use umbra_auth::{AuthPlugin, AuthUser, login_required_html};
use umbra_media::MediaPlugin;
use umbra_openapi::OpenApiPlugin;
use umbra_playground::PlaygroundPlugin;
use umbra_rest::{ResourceConfig, RestPlugin};
use umbra_security::{SecurityConfig, SecurityPlugin};
use umbra_sessions::SessionsPlugin;
use umbra_static::StaticPlugin;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = Settings::from_env()?;
    let pool = umbra::db::connect(&settings.database_url).await?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        // --- Framework plugins ---------------------------------------------
        .plugin(
            AuthPlugin::<AuthUser>::default()
                .with_default_routes()
                .with_user_in_templates(),
        )
        .plugin(SessionsPlugin::default())
        // --- Website apps ---------------------------------------------------
        .plugin(SiteContentPlugin::default())
        .plugin(FeaturesPlugin::default())
        .plugin(PluginDirectoryPlugin::default())
        .plugin(ReviewsPlugin::default())
        .plugin(ShowcasePlugin::default())
        .plugin(SecurityReportsPlugin::default())
        .plugin(AccountsPlugin::default())
        .plugin(CommunityPlugin::default())
        .plugin(PublicPlugin::default())
        // --- Admin/API/security --------------------------------------------
        .plugin(
            RestPlugin::default()
                .resource(ResourceConfig::for_::<AuthUser>().hide(["password_hash"])),
        )
        .plugin(OpenApiPlugin::new())
        .plugin(PlaygroundPlugin::new("Umbra").at("/api/playground/"))
        .plugin(StaticPlugin::new("/static", "./static"))
        // Registers the filesystem Storage backend that powers the
        // Plugin model's `logo` / `cover_image` File/Image fields.
        // Uploads land in ./media and serve at /media/<key>.
        .plugin(MediaPlugin::new("/media", "./media"))
        .plugin(SecurityPlugin::with_config(SecurityConfig {
            csrf_exempt_paths: vec!["/api".to_string()],
            ..Default::default()
        }))
        .plugin(
            AdminPlugin::default().site_title("Umbra".to_string()), // .register_widget(widget)
        )
        // --- Templates ------------------------------------------------------
        .templates_dir("templates")
        .not_found_template("404.html")
        .server_error_template("500.html")
        .slash_redirect(SlashRedirect::Append)
        // --- Routes ---------------------------------------------------------
        .routes(Routes::new().layered(
            "GET",
            "/dashboard",
            get(dashboard).layer(login_required_html("/login")),
        ))
        .build()?;

    umbra_cli::dispatch(app).await
}

async fn dashboard(
    user: umbra_auth::LoggedIn<AuthUser>,
) -> Result<Html<String>, (StatusCode, String)> {
    let body =
        umbra::templates::render("dashboard.html", &context!(user)).map_err(internal_error)?;
    Ok(Html(body))
}

fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}
