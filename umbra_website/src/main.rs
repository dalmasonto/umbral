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
use sponsor::SponsorPlugin;
use umbra::prelude::*;
use umbra::templates::context;
use umbra::web::{Html, SlashRedirect, StatusCode};
use umbra_admin::AdminPlugin;
use umbra_auth::{AuthPlugin, AuthUser, login_required_html};
use umbra_cache::{Cache, CachePlugin};
use umbra_livereload::LiveReloadPlugin;
use umbra_media::MediaPlugin;
use umbra_oauth::OAuthPlugin;
use umbra_oauth::providers::{GitHubProvider, GoogleProvider};
use umbra_openapi::OpenApiPlugin;
use umbra_playground::PlaygroundPlugin;
use umbra_realtime::{ModelAction, ModelEvent, Realtime, RealtimePlugin};
use umbra_rest::{ResourceConfig, RestPlugin};
use umbra_security::{SecurityConfig, SecurityPlugin};
use umbra_sessions::SessionsPlugin;
use umbra_static::StaticPlugin;

// Admin dashboard widgets, grouped by rendering shape in `src/widgets/`
// and bound to the plugin-directory data. Mirrors the shop example's
// widgets module.
mod widgets;

// The `seed_orm_data` management command (orchestrates every website
// plugin's idempotent seed).
mod seed_command;

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
        // Cache plugin: installs a process-wide in-memory cache as the
        // ambient handle (BROKEN-9 wires it in on_ready). Handlers can now
        // cache expensive data/fragments via `umbra_cache::ambient()`.
        // NOTE: deliberately NOT layering `cache_page` on the HTML pages —
        // its cache key is URL-only, but `base.html` renders per-user nav
        // and would serve one visitor's view to another; and in dev it
        // would defeat live-reload. Page caching needs an anonymous-only +
        // non-dev mode first (see the framework follow-up).
        // .plugin(CachePlugin::new(Cache::memory()))
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
        .plugin(SponsorPlugin::default())
        .plugin(PublicPlugin::default())
        // Dev browser live-reload: a file watcher pushes reload / CSS-swap
        // events over SSE and the client script is auto-injected into HTML
        // responses — no manual refresh on a template/CSS edit. Inert
        // outside Dev. Watches the site templates + static + the per-plugin
        // template dirs under `plugins/`.
        .plugin(LiveReloadPlugin::new().watch("plugins"))
        // Real-time push (SSE at /realtime/sse). The plugin-notes section
        // broadcasts a posted note to `public:plugin-<id>` watchers; the
        // default GroupPolicy allows those `public:*` groups.
        //
        // Admin notifications: when new moderatable content is CREATED, the
        // ORM's post_save signal fires `on_model`, and we push an
        // `admin_notification` event to every superuser by user id (see
        // `notify_admins`). Because the events are user-targeted at
        // superusers — never a public group — this is effectively a
        // superuser-only channel: a normal visitor's SSE connection is
        // never sent these events. The superuser dashboard subscribes and
        // surfaces a live toast (see templates/dashboard.html).
        .plugin(
            RealtimePlugin::default()
                .on_model::<plugin_directory::models::PluginComment, _, _>(
                    |ev: ModelEvent| async move {
                        if ev.action == ModelAction::Created {
                            notify_admins(
                                "note",
                                "New plugin note awaiting moderation",
                                "/admin/plugin_directory/plugincomment/",
                            )
                            .await;
                        }
                    },
                )
                .on_model::<reviews::Review, _, _>(|ev: ModelEvent| async move {
                    if ev.action == ModelAction::Created {
                        notify_admins(
                            "review",
                            "New developer review awaiting moderation",
                            "/admin/reviews/review/",
                        )
                        .await;
                    }
                })
                .on_model::<plugin_directory::models::Plugin, _, _>(|ev: ModelEvent| async move {
                    if ev.action == ModelAction::Created {
                        notify_admins(
                            "submission",
                            "New plugin submission awaiting review",
                            "/admin/plugin_directory/plugin/",
                        )
                        .await;
                    }
                })
                .on_model::<site_content::models::ContactMessage, _, _>(
                    |ev: ModelEvent| async move {
                        if ev.action == ModelAction::Created {
                            notify_admins(
                                "contact",
                                "New contact message",
                                "/admin/site_content/contactmessage/",
                            )
                            .await;
                        }
                    },
                )
                .on_model::<sponsor::SponsorInquiry, _, _>(|ev: ModelEvent| async move {
                    if ev.action == ModelAction::Created {
                        notify_admins(
                            "sponsor",
                            "New sponsor inquiry",
                            "/admin/sponsor/sponsorinquiry/",
                        )
                        .await;
                    }
                }),
        )
        // Contributes the `seed_orm_data` management command.
        .plugin(seed_command::SeedDataPlugin::default())
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
        .error_template(StatusCode::TOO_MANY_REQUESTS, "429.html")
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

/// Push an `admin_notification` real-time event to every active superuser.
///
/// Targeting by user id (not a public group) makes this a superuser-only
/// channel: a normal visitor's SSE connection is never sent these events,
/// and there's no group name they could subscribe to. `to_user` is
/// fire-and-forget and no-ops for users with no live connection, so this
/// is cheap when nobody's watching. Called from the RealtimePlugin
/// `on_model` create handlers wired above.
async fn notify_admins(kind: &'static str, title: &'static str, url: &'static str) {
    // Skip the superuser query entirely when realtime isn't installed
    // (e.g. a CLI command run rather than `serve`).
    if !Realtime::is_installed() {
        return;
    }
    let ids: Vec<i64> = match AuthUser::objects()
        .filter(umbra_auth::auth_user::IS_SUPERUSER.eq(true))
        .filter(umbra_auth::auth_user::IS_ACTIVE.eq(true))
        .fetch()
        .await
    {
        Ok(rows) => rows.into_iter().map(|u| u.id).collect(),
        Err(e) => {
            tracing::warn!("admin notify: superuser lookup failed: {e}");
            return;
        }
    };
    let payload = serde_json::json!({ "kind": kind, "title": title, "url": url });
    for id in ids {
        Realtime::to_user(id)
            .send("admin_notification", &payload)
            .await;
    }
}
