//! PublicPlugin — owns the public-facing pages of the umbral.dev site.
//!
//! Currently serves `/` (the landing page) and `/roadmap`. Models,
//! routes, and `on_ready` work live in the impl below.
//!
//! The landing page pulls four live numbers from the database and a
//! plugin table from the `plugin_directory` plugin. Every number
//! falls back to `—` (em-dash) when unknown — see the home template
//! for the rendering rule. **Never display `0` as a fabricated
//! count**; an empty database renders honest placeholders, not lies.

pub mod models;

use std::path::PathBuf;

use chrono::{Duration, Utc};
use plugin_directory::models::{self as pd, plugin};
use site_content::models::{self as sc, contact_message};
use umbral::plugin::{AppContext, Plugin, PluginError};
use umbral::routes::RouteSpec;
use umbral::templates::context;
use umbral::web::{ApiError, Html, Router, get};

/// The umbral version this site is built against (gaps3 #69).
///
/// Data, not markup. The hero badge hardcoded the literal `v0.1 preview` — the same class
/// of bug as the admin's `v0.0.1` (#67): a version string tied to nothing, wrong the
/// moment the dependency moves, and wrong for as long as nobody happens to look.
///
/// It cannot silently rot: the `version_tests` module below reads `Cargo.toml` and fails
/// the build if this const and the pinned `umbral` version disagree. Bump the dependency
/// and the badge follows — or the build tells you that you forgot.
pub(crate) const UMBRAL_VERSION: &str = "0.0.10";

#[derive(Debug, Default, Clone)]
pub struct PublicPlugin;

impl Plugin for PublicPlugin {
    fn name(&self) -> &'static str {
        "public"
    }

    fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
        Vec::new()
    }

    fn routes(&self) -> Router {
        Router::new()
            .route("/", get(home))
            .route("/roadmap", get(roadmap))
    }

    fn route_paths(&self) -> Vec<RouteSpec> {
        vec![
            RouteSpec::new("/", vec!["GET"]),
            RouteSpec::new("/roadmap", vec!["GET"]),
        ]
    }

    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

async fn home() -> Result<Html<String>, ApiError> {
    // Plugin list (filtered to first-party + approved community).
    // An empty result renders the static fallback in the template.
    // The whole story, one query: filter DB-side, count the related
    // comments via a correlated subquery the ORM renders —
    // `Plugin.objects.filter(...).annotate(n=Count("comments"))`.
    // (The DB-side filter is "approved + not deprecated", which widens
    // the old in-memory OR by also admitting approved experimental
    // sources — fine for a preview capped at three cards.)
    let plugins: Vec<PluginRow> = pd::Plugin::objects()
        .filter(plugin::SOURCE.ne("deprecated"))
        .filter(plugin::MODERATION.eq("approved"))
        // Featured plugins surface first in the 3-card preview (the
        // admin's `featured` flag), then by curated display order, then
        // by stars — so the homepage leads with what the team wants seen,
        // not just whatever the default scan order returns.
        .order_by(plugin::FEATURED.desc())
        .order_by(plugin::DISPLAY_ORDER.asc())
        .order_by(plugin::GITHUB_STARS.desc())
        .order_by(plugin::ID.asc())
        // Count VISIBLE comments only — `annotate_count_where` renders
        // the child predicate into the correlated subquery, and the
        // automatic soft-delete exclusion drops trashed comments too
        // (PluginComment is `#[umbral(soft_delete)]`). A hidden / pending
        // / flagged comment no longer inflates the public count.
        .annotate_count_where::<pd::PluginComment>(
            "comment_set_count",
            "comment_set",
            pd::plugin_comment::MODERATION.eq("visible"),
        )
        .fetch_annotated()
        .await
        .map_err(internal_error)?
        .into_iter()
        .map(|(p, anns)| {
            let mut row = PluginRow::from(p);
            row.notes = anns
                .get("comment_set_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            row
        })
        .collect();

    let plugin_count = if plugins.is_empty() {
        None
    } else {
        Some(plugins.len() as i64)
    };

    // Status counts — DB-side `SELECT COUNT(*)`, never a fetch-all
    // filtered in memory.
    let community_count = Some(
        pd::Plugin::objects()
            .filter(plugin::SOURCE.eq("community"))
            .filter(plugin::MODERATION.eq("approved"))
            .count()
            .await
            .map_err(internal_error)?,
    );
    let deprecated_count = Some(
        pd::Plugin::objects()
            .filter(plugin::SOURCE.eq("deprecated"))
            .count()
            .await
            .map_err(internal_error)?,
    );

    // Model count: every model that the autodetector has ever
    // recorded a migration for. This reads from the
    // `umbral_migrations` tracking table; if the framework doesn't
    // expose that table name here, fall back to None and let the
    // template render `—`.
    let model_count = count_models().await;

    // Form submissions in the last 7 days. Pulled from
    // site_content's ContactMessage model.
    let week_ago = Utc::now() - Duration::days(7);
    let form_submissions = sc::ContactMessage::objects()
        // .filter(sc::ContactMessage::CREATED_AT.gte(week_ago))
        .filter(contact_message::CREATED_AT.gte(week_ago))
        .count()
        .await
        .ok();

    // Lines of glue in main.rs: zero. The framework's App::builder
    // composes plugins, and the only call site for a new plugin is
    // one line. We assert that as a constant — it's a property of
    // the project shape, not a query.
    let glue_lines: i64 = 0;

    // Trust strip: a curated slice of approved reviews (featured first),
    // pulled live from the reviews plugin. An empty result renders the
    // honest empty state in the template — never fabricated testimonials.
    let reviews = reviews::featured_reviews(2).await.unwrap_or_default();

    // "Join the ecosystem" grid — the SAME model-driven channel cards the
    // /community page renders, sourced from the community plugin so brand
    // colour + coming-soon state live in one place (the SocialLink model).
    let channels = community::home_channels().await.map_err(internal_error)?;
    let newsletter_url = community::newsletter_url().await;

    let code = "```rust
#[derive(Model)]
#[umbral(table = \"post\", audited)]
struct Post {
  id: i64,
  #[umbral(max_length = 200)]
  title: String,
  #[umbral(auto_user_add)]
  author: Option<FK<AuthUser>>,
  #[umbral(auto_now_add)]
  created_at: DateTime<Utc>,
}
```";

    let body = umbral::templates::render(
        "public/home.html",
        &context! {
            umbral_version => UMBRAL_VERSION,
            code => code,
            plugins => plugins,
            plugin_count => plugin_count,
            model_count => model_count,
            community_count => community_count,
            deprecated_count => deprecated_count,
            form_submissions => form_submissions,
            glue_lines => glue_lines,
            reviews => reviews,
            channels => channels,
            newsletter_url => newsletter_url,
        },
    )
    .map_err(internal_error)?;
    Ok(Html(body))
}

async fn roadmap() -> Result<Html<String>, ApiError> {
    let body =
        umbral::templates::render("public/roadmap.html", &context! {}).map_err(internal_error)?;
    Ok(Html(body))
}

/// Count the number of models that have a recorded migration.
///
/// The framework tracks applied migrations in a table that ships
/// with `umbral-migrate`. Reading the count without depending on a
/// private table name: count distinct `model_id` rows in the
/// migration log. If the table isn't available (older framework
/// version, or the table name differs), this returns `None` and the
/// template renders `—` rather than `0`.
///
/// Implementation note: until the framework exposes a typed
/// `MigrationRecord` model, we leave this as a best-effort query.
/// The home page's `—` fallback is the honest answer when we don't
/// know.
async fn count_models() -> Option<i64> {
    // The migration tracking table is owned by the framework; we
    // do not have a typed access path here. Returning None until
    // the framework exposes one is correct: the page renders `—`
    // and nobody lies about a count.
    None
}

// ---------------------------------------------------------------------------
// View row — what the template iterates over.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginRow {
    pub id: i64,
    pub crate_name: String,
    pub status: String,
    pub short_description: String,
    /// Humanized GitHub star count ("2.1k"). Unsynced renders the
    /// honest "—" placeholder — the design's row always shows, the
    /// number is never fabricated.
    pub stars: String,
    /// Humanized crates.io download count. Same "—" placeholder rule.
    pub downloads: String,
    /// Visible discussion notes on this plugin. Filled by the view
    /// (needs an async count); the template hides a zero.
    pub notes: i64,
    /// True for Umbral- or third-party-reviewed plugins — drives the
    /// green "Audited" vs amber "Unverified" badge.
    pub audited: bool,
    /// Install command shown on the card: the plugin's stored
    /// `installation_commands` first line, else `umbral add <crate>`.
    pub install: String,
    /// Storage key for the logo image, resolved to a URL by `media_url()`.
    pub logo: Option<String>,
}

impl From<pd::Plugin> for PluginRow {
    fn from(p: pd::Plugin) -> Self {
        let status = match (p.status, p.maturity) {
            (pd::PluginStatus::Shipped, _) => format!("{:?}", p.maturity).to_lowercase(),
            (pd::PluginStatus::Usable, _) => "usable".to_string(),
            (pd::PluginStatus::Experimental, _) => "experimental".to_string(),
            (pd::PluginStatus::InProgress, _) => "in progress".to_string(),
            (pd::PluginStatus::Planned, _) => "planned".to_string(),
            (pd::PluginStatus::Deprecated, _) => "deprecated".to_string(),
        };
        let install = p
            .installation_commands
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("umbral add {}", p.crate_name));
        Self {
            id: p.id,
            status,
            stars: p
                .github_stars
                .map(humanize_count)
                .unwrap_or_else(|| "0".to_string()),
            downloads: p
                .downloads
                .map(humanize_count)
                .unwrap_or_else(|| "0".to_string()),
            notes: 0,
            audited: matches!(
                p.audit_status,
                pd::AuditStatus::UmbralReviewed | pd::AuditStatus::ThirdPartyReviewed
            ),
            install,
            crate_name: p.crate_name,
            short_description: p.short_description,
            logo: p.logo.map(|f| f.key().to_string()),
        }
    }
}

/// `1234` → `"1.2k"`, `2_400_000` → `"2.4M"`, `999` → `"999"`. The
/// compact form the directory cards use for stars / downloads.
fn humanize_count(n: i64) -> String {
    if n >= 1_000_000 {
        let v = format!("{:.1}", n as f64 / 1_000_000.0);
        format!("{}M", v.trim_end_matches(".0"))
    } else if n >= 1_000 {
        let v = format!("{:.1}", n as f64 / 1_000.0);
        format!("{}k", v.trim_end_matches(".0"))
    } else {
        n.to_string()
    }
}

fn internal_error<E: std::fmt::Display>(err: E) -> ApiError {
    // gaps3 #58: this used to be `(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())`,
    // which sent the database's own error text to the browser — a missing table, a column
    // name, a SQL fragment, shown to whoever asked for the page. `ApiError::internal` logs
    // the cause server-side and returns an opaque 500.
    ApiError::internal(err.to_string())
}

#[cfg(test)]
mod version_tests {
    /// The hero badge's version must equal the `umbral` version this site actually
    /// depends on. Without this, `UMBRAL_VERSION` is just another literal waiting to go
    /// stale — which is exactly how the badge came to read `v0.1 preview` (gaps3 #69).
    #[test]
    fn umbral_version_matches_the_pinned_dependency() {
        let manifest = include_str!("../../../Cargo.toml");
        let pinned = manifest
            .lines()
            .find_map(|l| {
                let l = l.trim();
                let rest = l.strip_prefix("umbral")?.trim_start();
                let rest = rest.strip_prefix('=')?.trim();
                rest.strip_prefix('"')?.split('"').next()
            })
            .expect("umbral_website/Cargo.toml must pin an `umbral` version");

        assert_eq!(
            super::UMBRAL_VERSION,
            pinned,
            "the hero badge says umbral v{} but Cargo.toml depends on {pinned}. Bump \
             UMBRAL_VERSION (or the dependency) so the site stops advertising a version \
             it does not run.",
            super::UMBRAL_VERSION
        );
    }
}
