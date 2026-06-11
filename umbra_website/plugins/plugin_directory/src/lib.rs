//! PluginDirectoryPlugin — owns the plugin directory for umbra.dev.
//!
//! Wire this into your App by adding to `src/main.rs`:
//!
//! ```ignore
//! .plugin(plugin_directory::PluginDirectoryPlugin::default())
//! ```
//!
//! Both the `/plugins` listing and the `/plugins/{slug}` detail page
//! are DB-driven: the listing loads every approved, non-deleted
//! `Plugin` in one annotated query (Django's
//! `Plugin.objects.filter(...).annotate(n=Count("comment_set"))`) and
//! the detail page loads the plugin plus its features, compatibility
//! rows and visible comments via the framework's reverse-relation API
//! (`plugin.reverse::<PluginFeature>()`).
//!
//! Honest-placeholder rule (mirrors `plugins/public`): an unknown
//! `github_stars` / `downloads` renders `—` (em-dash), NEVER a
//! fabricated `0`.

pub mod models;
pub mod seed;

pub use models::{
    AuditStatus, CommentKind, CommentModeration, PluginCompatibility, PluginFeature,
    PluginMaturity, PluginModeration, PluginSource, PluginStatus, SecurityStatus,
};

use std::path::PathBuf;

use serde::Serialize;
use umbra::migrate::ModelMeta;
use umbra::plugin::{AppContext, Plugin, PluginError};
use umbra::prelude::*;
use umbra::routes::RouteSpec;
use umbra::templates::context;
use umbra::web::{Html, Path, Query, Router, StatusCode, get};

use models::{
    self as pd, plugin, plugin_comment, plugin_compatibility, plugin_feature, Plugin as PluginModel,
};

#[derive(Debug, Default, Clone)]
pub struct PluginDirectoryPlugin;

impl Plugin for PluginDirectoryPlugin {
    fn name(&self) -> &'static str {
        "plugin_directory"
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![
            ModelMeta::for_::<models::Plugin>(),
            ModelMeta::for_::<models::PluginFeature>(),
            ModelMeta::for_::<models::PluginCompatibility>(),
            ModelMeta::for_::<models::PluginComment>(),
        ]
    }

    fn routes(&self) -> Router {
        Router::new()
            .route("/prebuilt", get(prebuilt_plugins))
            .route("/plugins", get(plugin_directory))
            .route("/plugins/{slug}", get(plugin_detail))
    }

    fn route_paths(&self) -> Vec<RouteSpec> {
        vec![
            RouteSpec::new("/prebuilt", vec!["GET"]),
            RouteSpec::new("/plugins", vec!["GET"]),
            RouteSpec::new("/plugins/{slug}", vec!["GET"]),
        ]
    }

    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        // Seed the first-party plugin rows the first time the
        // server starts. The seed is idempotent (short-circuits
        // when the table is non-empty), so this is safe on every
        // boot. Failures log a warning but do not crash startup —
        // the home page falls back to its static table when the
        // DB is empty.
        let plugin_name = self.name();
        tokio::spawn(async move {
            match seed::seed_official_plugins().await {
                Ok(0) => tracing::debug!(
                    "{}: official plugin table already populated, seed skipped",
                    plugin_name
                ),
                Ok(n) => tracing::info!("{}: seeded {} official plugin rows", plugin_name, n),
                Err(e) => tracing::warn!(
                    "{}: official plugin seed failed: {e}. \
                     Home page will fall back to the static plugin table.",
                    plugin_name
                ),
            }
        });
        Ok(())
    }
}

async fn prebuilt_plugins() -> Result<Html<String>, (StatusCode, String)> {
    render("plugin_directory/prebuilt.html", &serde_json::json!({}))
}

// ---------------------------------------------------------------------------
// Listing — /plugins
// ---------------------------------------------------------------------------

/// Query string for the listing page: `?source=community` filters the
/// card list (and marks the active facet in the sidebar).
#[derive(Debug, Default, serde::Deserialize)]
struct ListingQuery {
    source: Option<String>,
}

async fn plugin_directory(
    Query(q): Query<ListingQuery>,
) -> Result<Html<String>, (StatusCode, String)> {
    render_listing(q.source.as_deref())
        .await
        .map(Html)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Load + render the `/plugins` listing. Public so the render
/// smoke-test can exercise the full query → view-model → template path
/// without an axum runtime. `source` is an optional facet filter.
pub async fn render_listing(source: Option<&str>) -> Result<String, String> {
    // The active source facet, validated against the known variants so a
    // junk `?source=` value doesn't silently filter to nothing.
    let active_source = source.filter(|s| {
        matches!(*s, "official" | "community" | "experimental" | "deprecated")
    });

    // One annotated query: every approved, non-deleted plugin with its
    // VISIBLE comment count in a correlated subquery the ORM renders
    // (Django's `annotate(n=Count("comment_set"))`). Soft-deleted rows
    // are excluded automatically (Plugin is `#[umbra(soft_delete)]`).
    let mut listing = PluginModel::objects()
        .filter(plugin::MODERATION.eq("approved"));
    if let Some(src) = active_source {
        listing = listing.filter(plugin::SOURCE.eq(src));
    }
    let rows = listing
        .annotate_count_where::<pd::PluginComment>(
            "comment_set_count",
            "comment_set",
            plugin_comment::MODERATION.eq("visible"),
        )
        .fetch_annotated()
        .await
        .map_err(|e| e.to_string())?;

    let mut cards: Vec<PluginCard> = rows
        .into_iter()
        .map(|(p, anns)| {
            let notes = anns
                .get("comment_set_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            PluginCard::from_model(p, notes)
        })
        .collect();

    // Featured-first, then by display_order, then by stars descending.
    // (Sorted in-memory so the secondary star tiebreak — an Option — is
    // honest about unknown values rather than coercing them to 0 in SQL.)
    cards.sort_by(|a, b| {
        b.featured
            .cmp(&a.featured)
            .then(a.display_order.cmp(&b.display_order))
            .then(b.star_count.cmp(&a.star_count))
    });

    // Real facet counts — one `SELECT COUNT(*)` per facet, never a
    // fetch-all-and-count-in-memory.
    let counts = FacetCounts::load().await.map_err(|e| e.to_string())?;
    let total = counts.total;
    let showing = cards.len();

    umbra::templates::render(
        "plugin_directory/plugins.html",
        &context! {
            plugins => cards,
            counts => counts,
            total => total,
            showing => showing,
            active_source => active_source,
        },
    )
    .map_err(|e| e.to_string())
}

/// Sidebar facet counts. Each field is its own `COUNT(*)` query.
#[derive(Debug, Serialize)]
struct FacetCounts {
    official: i64,
    community: i64,
    experimental: i64,
    deprecated: i64,
    flagged: i64,
    total: i64,
}

impl FacetCounts {
    async fn load() -> Result<Self, sqlx::Error> {
        let by_source = |src: &'static str| async move {
            PluginModel::objects()
                .filter(plugin::MODERATION.eq("approved"))
                .filter(plugin::SOURCE.eq(src))
                .count()
                .await
        };
        let official = by_source("official").await?;
        let community = by_source("community").await?;
        let experimental = by_source("experimental").await?;
        let deprecated = by_source("deprecated").await?;
        let flagged = PluginModel::objects()
            .filter(plugin::MODERATION.eq("approved"))
            .filter(plugin::SECURITY_STATUS.eq("blocked"))
            .count()
            .await?;
        let total = PluginModel::objects()
            .filter(plugin::MODERATION.eq("approved"))
            .count()
            .await?;
        Ok(Self {
            official,
            community,
            experimental,
            deprecated,
            flagged,
            total,
        })
    }
}

/// A single card in the listing — the shape `plugins.html` iterates.
#[derive(Debug, Serialize)]
struct PluginCard {
    slug: String,
    name: String,
    crate_name: String,
    author: String,
    short_description: String,
    /// "official" / "community" / "experimental" / "deprecated".
    source: String,
    featured: bool,
    flagged: bool,
    audited: bool,
    /// "ok" / "warn" / "bad" — drives the audit pill colour.
    audit_kind: &'static str,
    audit_label: &'static str,
    /// Humanized star count, or `—` when unknown. Never fabricated.
    stars: String,
    /// Humanized download count, or `—` when unknown.
    downloads: String,
    /// Visible comment count (`0` is hidden by the template).
    notes: i64,
    /// First non-empty line of `installation_commands`, else `umbra add
    /// <crate_name>`.
    install: String,
    /// ≤4 short tags derived from `metadata.tags` or source/maturity.
    tags: Vec<String>,
    /// Two-character tile initials.
    initials: String,
    // --- sort keys (not serialized for the template) ---
    #[serde(skip)]
    display_order: i32,
    #[serde(skip)]
    star_count: i64,
}

impl PluginCard {
    fn from_model(p: PluginModel, notes: i64) -> Self {
        let (audit_kind, audit_label) = audit_badge(p.audit_status);
        let flagged = matches!(p.security_status, SecurityStatus::Blocked);
        Self {
            slug: p.slug.clone(),
            name: p.name.clone(),
            crate_name: p.crate_name.clone(),
            author: p.author.clone(),
            short_description: p.short_description.clone(),
            source: source_str(p.source).to_string(),
            featured: p.featured,
            flagged,
            audited: matches!(
                p.audit_status,
                AuditStatus::UmbraReviewed | AuditStatus::ThirdPartyReviewed
            ),
            audit_kind: if flagged { "bad" } else { audit_kind },
            audit_label: if flagged { "Flagged" } else { audit_label },
            stars: humanize_opt(p.github_stars),
            downloads: humanize_opt(p.downloads),
            notes,
            install: install_line(&p.installation_commands, &p.crate_name),
            tags: derive_tags(&p),
            initials: initials(&p.name),
            display_order: p.display_order,
            star_count: p.github_stars.unwrap_or(0),
        }
    }
}

// ---------------------------------------------------------------------------
// Detail — /plugins/{slug}
// ---------------------------------------------------------------------------

async fn plugin_detail(Path(slug): Path<String>) -> Result<Html<String>, (StatusCode, String)> {
    if slug == "submit" {
        return render("plugin_directory/submit.html", &serde_json::json!({}));
    }

    match render_detail(&slug)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    {
        Some(html) => Ok(Html(html)),
        None => Err((
            StatusCode::NOT_FOUND,
            format!("No plugin directory entry exists for `{slug}` yet."),
        )),
    }
}

/// Load + render the `/plugins/{slug}` detail page. `Ok(None)` means
/// no approved plugin matches the slug (a 404). Public so the render
/// smoke-test drives the full reverse-relation → view-model → template
/// path directly.
pub async fn render_detail(slug: &str) -> Result<Option<String>, String> {
    let Some(plugin) = PluginModel::objects()
        .filter(plugin::SLUG.eq(slug))
        .filter(plugin::MODERATION.eq("approved"))
        .first()
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };

    // Reverse relations — children whose FK points back at this plugin.
    // `plugin.reverse::<Child>()` discovers the FK column from the child's
    // FIELDS (the one targeting the `plugin` table) and builds the
    // `Child::objects().filter(plugin = <pk>)` queryset for us.
    let feature_rows = plugin
        .reverse::<PluginFeature>()
        .map_err(|e| e.to_string())?
        .filter(plugin_feature::VISIBLE.eq(true))
        .order_by(plugin_feature::DISPLAY_ORDER.asc())
        .fetch()
        .await
        .map_err(|e| e.to_string())?;

    let compat_rows = plugin
        .reverse::<PluginCompatibility>()
        .map_err(|e| e.to_string())?
        .order_by(plugin_compatibility::CREATED_AT.asc())
        .fetch()
        .await
        .map_err(|e| e.to_string())?;

    let comment_rows = plugin
        .reverse::<pd::PluginComment>()
        .map_err(|e| e.to_string())?
        .filter(plugin_comment::MODERATION.eq("visible"))
        .order_by(plugin_comment::PINNED.desc())
        .order_by(plugin_comment::CREATED_AT.asc())
        .limit(10)
        .fetch()
        .await
        .map_err(|e| e.to_string())?;

    let detail = PluginDetail::build(plugin, feature_rows, compat_rows, comment_rows);
    umbra::templates::render("plugin_directory/plugin.html", &context!(plugin => detail))
        .map(Some)
        .map_err(|e| e.to_string())
}

/// The detail view-model the `plugin.html` template renders.
#[derive(Debug, Serialize)]
struct PluginDetail {
    slug: String,
    name: String,
    source: String,
    maintainer: String,
    description: String,
    /// Markdown body — rendered through `| markdown` in the template.
    full_content: String,
    icon: String,
    featured: bool,
    flagged: bool,
    status: String,
    status_kind: &'static str,
    maturity: String,
    channel: String,
    install: String,
    toml: String,
    /// `—` when there's no real rating data — never fabricated.
    rating: String,
    /// Humanized downloads, or `—`.
    installs: String,
    /// Real visible comment count.
    notes: i64,
    tags: Vec<String>,
    features: Vec<StatusRow>,
    shipped_features: usize,
    total_features: usize,
    progress_pct: u32,
    usage_title: String,
    usage_intro: String,
    usage_code: String,
    audit_label: &'static str,
    audit_date: String,
    audit_kind: &'static str,
    compatibility: Vec<StatusRow>,
    comments: Vec<CommentPreview>,
}

impl PluginDetail {
    fn build(
        p: PluginModel,
        features: Vec<PluginFeature>,
        compatibility: Vec<PluginCompatibility>,
        comments: Vec<pd::PluginComment>,
    ) -> Self {
        let flagged = matches!(p.security_status, SecurityStatus::Blocked);
        let (status, status_kind) = if flagged {
            ("Flagged".to_string(), "bad")
        } else {
            status_badge(p.status)
        };
        let (audit_kind, audit_label) = if flagged {
            ("bad", "Malicious")
        } else {
            let (k, l) = audit_badge(p.audit_status);
            (k, l)
        };
        let audit_date = p
            .audit_status
            .ne_or_label()
            .to_string();

        let total_features = features.len();
        let shipped_features = features
            .iter()
            .filter(|f| matches!(f.status, PluginStatus::Shipped | PluginStatus::Usable))
            .count();
        let progress_pct = if total_features == 0 {
            0
        } else {
            ((shipped_features as f64 / total_features as f64) * 100.0).round() as u32
        };

        let feature_rows: Vec<StatusRow> = features
            .into_iter()
            .map(|f| {
                let (status, kind) = feature_status(f.status);
                StatusRow {
                    label: f.name,
                    status,
                    kind,
                }
            })
            .collect();

        let compat_rows: Vec<StatusRow> = compatibility
            .into_iter()
            .map(|c| {
                let backends = backend_summary(&c.supported_database_backends);
                let label = if backends.is_empty() {
                    c.umbra_version.clone()
                } else {
                    format!("{} · {}", c.umbra_version, backends)
                };
                let kind = if c.verified_at.is_some() { "ok" } else { "warn" };
                StatusRow {
                    label,
                    status: if c.verified_at.is_some() {
                        "verified".to_string()
                    } else {
                        "declared".to_string()
                    },
                    kind,
                }
            })
            .collect();

        let comment_previews: Vec<CommentPreview> = comments
            .into_iter()
            .map(CommentPreview::from_model)
            .collect();
        let notes = comment_previews.len() as i64;

        let install = install_line(&p.installation_commands, &p.crate_name);
        let maintainer = p.author.clone();
        let source = source_str(p.source).to_string();
        let channel = match p.source {
            PluginSource::Official => "Stable channel",
            PluginSource::Community => "Community channel",
            PluginSource::Experimental => "Experimental channel",
            PluginSource::Deprecated => "Deprecated",
        }
        .to_string();

        Self {
            slug: p.slug.clone(),
            name: p.name.clone(),
            source,
            maintainer,
            description: p.short_description.clone(),
            full_content: p.full_content.clone(),
            icon: initials(&p.name),
            featured: p.featured,
            flagged,
            status,
            status_kind,
            maturity: title_case(&format!("{:?}", p.maturity)),
            channel,
            install,
            toml: p.installation_commands.clone(),
            rating: "—".to_string(),
            installs: humanize_opt(p.downloads),
            notes,
            tags: derive_tags(&p),
            features: feature_rows,
            shipped_features,
            total_features,
            progress_pct,
            usage_title: "Usage".to_string(),
            usage_intro: p
                .setup_notes
                .clone()
                .unwrap_or_else(|| {
                    "Add the plugin to your project's plugin list and wire it in \
                     `main.rs` like every other Umbra battery."
                        .to_string()
                }),
            usage_code: p.installation_commands.clone(),
            audit_label,
            audit_date,
            audit_kind,
            compatibility: compat_rows,
            comments: comment_previews,
        }
    }
}

#[derive(Debug, Serialize)]
struct StatusRow {
    label: String,
    status: String,
    kind: &'static str,
}

#[derive(Debug, Serialize)]
struct CommentPreview {
    initials: String,
    name: String,
    role: String,
    badge: String,
    badge_kind: &'static str,
    body: String,
}

impl CommentPreview {
    fn from_model(c: pd::PluginComment) -> Self {
        let name = c
            .author_label
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Anonymous".to_string());
        let (role, badge, badge_kind) = match c.kind {
            CommentKind::MaintainerReply => ("maintainer", "maintainer reply", "ok"),
            CommentKind::CompatibilityNote => ("compatibility note", "compatibility", "warn"),
            CommentKind::MigrationNote => ("migration note", "migration", "warn"),
            CommentKind::UsageNote => ("usage note", "usage", "ok"),
            CommentKind::Question => ("question", "question", "warn"),
            CommentKind::General => ("community note", "", "warn"),
        };
        let role = if c.pinned {
            format!("pinned · {role}")
        } else {
            role.to_string()
        };
        Self {
            initials: initials(&name),
            name,
            role,
            badge: badge.to_string(),
            badge_kind,
            body: c.body,
        }
    }
}

// ---------------------------------------------------------------------------
// Mapping helpers (enum → badge, humanize, install line, tags)
// ---------------------------------------------------------------------------

fn source_str(s: PluginSource) -> &'static str {
    match s {
        PluginSource::Official => "official",
        PluginSource::Community => "community",
        PluginSource::Experimental => "experimental",
        PluginSource::Deprecated => "deprecated",
    }
}

/// (status label, status_kind) for the install aside + tracker pill.
fn status_badge(s: PluginStatus) -> (String, &'static str) {
    match s {
        PluginStatus::Shipped => ("Shipped".into(), "ok"),
        PluginStatus::Usable => ("Usable".into(), "ok"),
        PluginStatus::Experimental => ("Experimental".into(), "warn"),
        PluginStatus::InProgress => ("In progress".into(), "warn"),
        PluginStatus::Planned => ("Planned".into(), "muted"),
        PluginStatus::Deprecated => ("Deprecated".into(), "muted"),
    }
}

/// (audit_kind, audit_label) for the audit pill.
fn audit_badge(a: AuditStatus) -> (&'static str, &'static str) {
    match a {
        AuditStatus::UmbraReviewed => ("ok", "Audited"),
        AuditStatus::ThirdPartyReviewed => ("ok", "Third-party audited"),
        AuditStatus::SelfReviewed => ("warn", "Self-reviewed"),
        AuditStatus::NeedsReview => ("warn", "Needs review"),
        AuditStatus::NotReviewed => ("warn", "Unverified"),
    }
}

/// Per-feature (status text, kind) for the tracker rows.
fn feature_status(s: PluginStatus) -> (String, &'static str) {
    match s {
        PluginStatus::Shipped => ("shipped".into(), "ok"),
        PluginStatus::Usable => ("usable".into(), "ok"),
        PluginStatus::Experimental => ("experimental".into(), "warn"),
        PluginStatus::InProgress => ("in progress".into(), "warn"),
        PluginStatus::Planned => ("planned".into(), "muted"),
        PluginStatus::Deprecated => ("deprecated".into(), "muted"),
    }
}

/// Compact, human summary of the JSON `supported_database_backends`
/// array (`["postgres","sqlite"]` → `"PostgreSQL, SQLite"`).
fn backend_summary(v: &serde_json::Value) -> String {
    let Some(arr) = v.as_array() else {
        return String::new();
    };
    arr.iter()
        .filter_map(|b| b.as_str())
        .map(|b| match b.to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" => "PostgreSQL".to_string(),
            "sqlite" => "SQLite".to_string(),
            "mysql" => "MySQL".to_string(),
            other => title_case(other),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// First non-empty line of the stored install commands, else a sane
/// `umbra add <crate>` default.
fn install_line(commands: &str, crate_name: &str) -> String {
    commands
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("umbra add {crate_name}"))
}

/// ≤4 short tags. Prefers `metadata.tags` (a JSON string array); falls
/// back to source + maturity words so a card always has something.
fn derive_tags(p: &PluginModel) -> Vec<String> {
    if let Some(meta) = &p.metadata {
        if let Some(arr) = meta.get("tags").and_then(|t| t.as_array()) {
            let tags: Vec<String> = arr
                .iter()
                .filter_map(|t| t.as_str())
                .map(|t| t.to_string())
                .take(4)
                .collect();
            if !tags.is_empty() {
                return tags;
            }
        }
    }
    let mut tags = vec![source_str(p.source).to_string()];
    tags.push(format!("{:?}", p.maturity).to_lowercase());
    tags.truncate(4);
    tags
}

/// Two-character uppercase initials from a name ("Umbra REST" → "UR",
/// "umbra-rest" → "UR", "rest" → "RE").
fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter(|w| !w.is_empty())
        .collect();
    let s: String = match words.as_slice() {
        [] => "??".to_string(),
        [one] => one.chars().take(2).collect(),
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
    };
    s.to_uppercase()
}

/// `"1234"` → `"1.2k"`, `2_400_000` → `"2.4M"`. Honest `—` for `None`.
fn humanize_opt(n: Option<i64>) -> String {
    match n {
        None => "—".to_string(),
        Some(n) if n >= 1_000_000 => {
            let v = format!("{:.1}", n as f64 / 1_000_000.0);
            format!("{}M", v.trim_end_matches(".0"))
        }
        Some(n) if n >= 1_000 => {
            let v = format!("{:.1}", n as f64 / 1_000.0);
            format!("{}k", v.trim_end_matches(".0"))
        }
        Some(n) => n.to_string(),
    }
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

fn render<C: Serialize>(template: &str, context: &C) -> Result<Html<String>, (StatusCode, String)> {
    umbra::templates::render(template, context)
        .map(Html)
        .map_err(internal_error)
}

fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

trait AuditDateLabel {
    fn ne_or_label(&self) -> &'static str;
}

impl AuditDateLabel for AuditStatus {
    fn ne_or_label(&self) -> &'static str {
        match self {
            AuditStatus::UmbraReviewed => "Umbra team",
            AuditStatus::ThirdPartyReviewed => "Third party",
            AuditStatus::SelfReviewed => "Maintainer",
            AuditStatus::NeedsReview => "Pending",
            AuditStatus::NotReviewed => "—",
        }
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::*;

    #[test]
    fn initials_handles_words_hyphens_and_single() {
        assert_eq!(initials("Umbra REST"), "UR");
        assert_eq!(initials("umbra-rest"), "UR");
        assert_eq!(initials("rest"), "RE");
        assert_eq!(initials(""), "??");
    }

    #[test]
    fn humanize_opt_is_honest_about_unknowns() {
        assert_eq!(humanize_opt(None), "—");
        assert_eq!(humanize_opt(Some(0)), "0");
        assert_eq!(humanize_opt(Some(999)), "999");
        assert_eq!(humanize_opt(Some(1_234)), "1.2k");
        assert_eq!(humanize_opt(Some(2_400_000)), "2.4M");
    }

    #[test]
    fn install_line_prefers_first_nonempty_then_falls_back() {
        assert_eq!(
            install_line("\n  umbra add umbra-rest\nmore", "umbra-rest"),
            "umbra add umbra-rest"
        );
        assert_eq!(install_line("   \n  ", "umbra-x"), "umbra add umbra-x");
    }

    #[test]
    fn backend_summary_normalizes_known_backends() {
        let v = serde_json::json!(["postgres", "sqlite", "mysql"]);
        assert_eq!(backend_summary(&v), "PostgreSQL, SQLite, MySQL");
        assert_eq!(backend_summary(&serde_json::json!("notarray")), "");
    }

    #[test]
    fn audit_badge_kinds() {
        assert_eq!(audit_badge(AuditStatus::UmbraReviewed).0, "ok");
        assert_eq!(audit_badge(AuditStatus::NotReviewed).0, "warn");
    }
}
