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
use umbra_oauth::OAuthPlugin;
use umbra_oauth::providers::{GitHubProvider, GoogleProvider};
use umbra_openapi::OpenApiPlugin;
use umbra_playground::PlaygroundPlugin;
use umbra_rest::{ResourceConfig, RestPlugin};
use umbra_security::{SecurityConfig, SecurityPlugin};
use umbra_sessions::SessionsPlugin;
use umbra_static::StaticPlugin;

// Admin dashboard widgets, grouped by rendering shape in `src/widgets/`
// and bound to the plugin-directory data. Mirrors the shop example's
// widgets module.
mod widgets;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = Settings::from_env()?;

    // OAuth credentials. figment parses `.env` into `Settings.extra` as
    // `oauth_<provider>_client_id` / `oauth_<provider>_client_secret`
    // (from `UMBRA_OAUTH_<PROVIDER>_CLIENT_ID` / `_CLIENT_SECRET`).
    // Reading them from Settings is reliable regardless of whether `.env`
    // also reached the raw process environment. Each provider registers
    // only when BOTH its id and secret are present.
    let google = settings
        .extra_str("oauth_google_client_id")
        .zip(settings.extra_str("oauth_google_client_secret"))
        .map(|(id, secret)| GoogleProvider::new(id, secret));
    let github = settings
        .extra_str("oauth_github_client_id")
        .zip(settings.extra_str("oauth_github_client_secret"))
        .map(|(id, secret)| GitHubProvider::new(id, secret));
    let oauth_base = settings
        .extra_str("oauth_redirect_base")
        .unwrap_or("http://localhost:8100")
        .to_string();

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
        // OAuth / social login. Credentials are read from the environment
        // (UMBRA_OAUTH_<PROVIDER>_CLIENT_ID / _CLIENT_SECRET); a provider
        // with no credentials is simply not registered, so this is safe to
        // leave on with nothing configured. `redirect_base` is the public
        // origin — override it in prod with UMBRA_OAUTH_REDIRECT_BASE.
        .plugin({
            let mut oauth = OAuthPlugin::new(oauth_base).login_redirect("/dashboard");
            if let Some(p) = google {
                oauth = oauth.provider(p);
            }
            if let Some(p) = github {
                oauth = oauth.provider(p);
            }
            oauth
        })
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
            AdminPlugin::default()
                .site_title("Umbra".to_string())
                // Dashboard layout — named sections, one per rendering
                // shape. Builders live in `src/widgets/`; see that
                // module's docstring for which file owns each one.
                .dashboard_section(
                    umbra_admin::WidgetSection::new("Directory overview")
                        .subtitle("Headline counts across the plugin directory")
                        .widget(widgets::total_plugins_card())
                        .widget(widgets::pending_review_card())
                        .widget(widgets::discussion_notes_card())
                        .widget(widgets::featured_card()),
                )
                .dashboard_section(
                    umbra_admin::WidgetSection::new("Composition")
                        .subtitle("How the directory breaks down by source, status, and maturity")
                        .widget(widgets::source_mix_donut())
                        .widget(widgets::status_mix_donut())
                        .widget(widgets::submissions_bar())
                        .widget(widgets::status_maturity_heatmap()),
                )
                .dashboard_section(
                    umbra_admin::WidgetSection::new("Trends")
                        .subtitle("Submissions + discussion activity over the last week")
                        .widget(widgets::submissions_chart().with_default_period("7d"))
                        .widget(widgets::activity_chart().with_default_period("7d")),
                )
                .dashboard_section(
                    umbra_admin::WidgetSection::new("Gauges & rankings")
                        .subtitle("Audit coverage gauge, maturity breakdown, and a shipped KPI")
                        .widget(widgets::audit_coverage_radial())
                        .widget(widgets::plugins_by_maturity())
                        .widget(widgets::shipped_kpi()),
                )
                .dashboard_section(
                    umbra_admin::WidgetSection::new("Recent activity")
                        .subtitle("The latest plugins listed in the directory")
                        .widget(widgets::recent_plugins_table())
                        .widget(widgets::recent_activity_feed()),
                ),
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
    _user: umbra_auth::LoggedIn<AuthUser>,
) -> Result<Html<String>, (StatusCode, String)> {
    // Don't pass our own `user` into the context: that would shadow the
    // ambient `user` injected by AuthPlugin::with_user_in_templates()
    // (the only one that carries `is_authenticated`), desyncing this
    // page's body from the base-template nav — the page body would say
    // "logged in" while the header showed "Log in / Sign up". The
    // `LoggedIn` extractor stays to enforce the login requirement; the
    // template reads `user.username` / `user.is_authenticated` from the
    // ambient context.
    let body = umbra::templates::render("dashboard.html", &context! {}).map_err(internal_error)?;
    Ok(Html(body))
}

fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}
