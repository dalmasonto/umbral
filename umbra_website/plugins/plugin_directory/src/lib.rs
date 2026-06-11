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

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use umbra::migrate::ModelMeta;
use umbra::plugin::{AppContext, Plugin, PluginError};
use umbra::prelude::*;
use umbra::routes::RouteSpec;
use umbra::templates::context;
use umbra::web::{Form, Html, Path, Query, Redirect, Router, StatusCode, get, post};

use models::{
    self as pd, Plugin as PluginModel, plugin, plugin_comment, plugin_compatibility, plugin_feature,
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
            .route("/plugins/{slug}/notes", post(post_plugin_note))
            .route("/search", get(plugin_search))
    }

    fn route_paths(&self) -> Vec<RouteSpec> {
        vec![
            RouteSpec::new("/prebuilt", vec!["GET"]),
            RouteSpec::new("/plugins", vec!["GET"]),
            RouteSpec::new("/plugins/{slug}", vec!["GET"]),
            RouteSpec::new("/plugins/{slug}/notes", vec!["POST"]),
            RouteSpec::new("/search", vec!["GET"]),
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
/// card list (and marks the active facet in the sidebar); `?page=N`
/// selects the 1-based page (page size [`PAGE_SIZE`]).
#[derive(Debug, Default, serde::Deserialize)]
struct ListingQuery {
    source: Option<String>,
    page: Option<u32>,
}

/// Cards per listing page.
const PAGE_SIZE: i64 = 12;

async fn plugin_directory(
    Query(q): Query<ListingQuery>,
) -> Result<Html<String>, (StatusCode, String)> {
    render_listing(q.source.as_deref(), q.page.unwrap_or(1))
        .await
        .map(Html)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Pagination view-model handed to `plugins.html`.
#[derive(Debug, Serialize)]
struct Pagination {
    page: i64,
    total_pages: i64,
    has_prev: bool,
    has_next: bool,
    prev_page: i64,
    next_page: i64,
    /// 1-based index of the first card on this page (within `total`).
    range_start: i64,
    /// 1-based index of the last card on this page.
    range_end: i64,
    /// The (small) window of page numbers to render as clickable links.
    page_window: Vec<i64>,
}

/// Load + render the `/plugins` listing. Public so the render
/// smoke-test can exercise the full query → view-model → template path
/// without an axum runtime. `source` is an optional facet filter;
/// `page` is the 1-based page number (clamped to `[1, total_pages]`).
pub async fn render_listing(source: Option<&str>, page: u32) -> Result<String, String> {
    // The active source facet, validated against the known variants so a
    // junk `?source=` value doesn't silently filter to nothing.
    let active_source =
        source.filter(|s| matches!(*s, "official" | "community" | "experimental" | "deprecated"));

    // Real facet counts — one `SELECT COUNT(*)` per facet, never a
    // fetch-all-and-count-in-memory.
    let counts = FacetCounts::load().await.map_err(|e| e.to_string())?;

    // `total` is the count for the *current view*: the active facet's
    // count when filtering, else the grand total. Pagination is computed
    // against this so "X of N" and the page count agree with what's shown.
    let total = match active_source {
        Some("official") => counts.official,
        Some("community") => counts.community,
        Some("experimental") => counts.experimental,
        Some("deprecated") => counts.deprecated,
        _ => counts.total,
    };
    let total_pages = if total == 0 {
        1
    } else {
        (total + PAGE_SIZE - 1) / PAGE_SIZE
    };
    // Clamp the requested page into range so `?page=0` / `?page=999`
    // can't produce an empty or negative-offset query.
    let page = (page.max(1) as i64).min(total_pages);
    let offset = (page - 1) * PAGE_SIZE;

    // One annotated query: every approved, non-deleted plugin with its
    // VISIBLE comment count in a correlated subquery the ORM renders
    // (Django's `annotate(n=Count("comment_set"))`). Soft-deleted rows
    // are excluded automatically (Plugin is `#[umbra(soft_delete)]`).
    // The ordering is pushed DB-side (featured first, then display_order,
    // then stars) so LIMIT/OFFSET slices a stable, page-consistent order
    // rather than an in-memory reshuffle that only sorts the current page.
    let mut listing = PluginModel::objects().filter(plugin::MODERATION.eq("approved"));
    if let Some(src) = active_source {
        listing = listing.filter(plugin::SOURCE.eq(src));
    }
    let rows = listing
        .order_by(plugin::FEATURED.desc())
        .order_by(plugin::DISPLAY_ORDER.asc())
        .order_by(plugin::GITHUB_STARS.desc())
        .order_by(plugin::ID.asc())
        .annotate_count_where::<pd::PluginComment>(
            "comment_set_count",
            "comment_set",
            plugin_comment::MODERATION.eq("visible"),
        )
        .limit(PAGE_SIZE as u64)
        .offset(offset as u64)
        .fetch_annotated()
        .await
        .map_err(|e| e.to_string())?;

    let cards: Vec<PluginCard> = rows
        .into_iter()
        .map(|(p, anns)| {
            let notes = anns
                .get("comment_set_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            PluginCard::from_model(p, notes)
        })
        .collect();

    let showing = cards.len();
    let range_start = if showing == 0 { 0 } else { offset + 1 };
    let range_end = offset + showing as i64;

    let pagination = Pagination {
        page,
        total_pages,
        has_prev: page > 1,
        has_next: page < total_pages,
        prev_page: (page - 1).max(1),
        next_page: (page + 1).min(total_pages),
        range_start,
        range_end,
        page_window: page_window(page, total_pages),
    };

    umbra::templates::render(
        "plugin_directory/plugins.html",
        &context! {
            plugins => cards,
            counts => counts,
            total => total,
            showing => showing,
            active_source => active_source,
            pagination => pagination,
        },
    )
    .map_err(|e| e.to_string())
}

/// A small window of page numbers centred on the current page (at most
/// five), so the control stays compact for large directories.
fn page_window(page: i64, total_pages: i64) -> Vec<i64> {
    const WINDOW: i64 = 5;
    if total_pages <= WINDOW {
        return (1..=total_pages).collect();
    }
    let half = WINDOW / 2;
    let mut start = (page - half).max(1);
    let end = (start + WINDOW - 1).min(total_pages);
    // Re-anchor the start if we hit the right edge so the window keeps
    // its full width (e.g. last page shows the final five, not three).
    start = (end - WINDOW + 1).max(1);
    (start..=end).collect()
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
        }
    }
}

// ---------------------------------------------------------------------------
// Global search — /search?q=...
// ---------------------------------------------------------------------------

/// Query string for the header search dialog: `?q=rest`.
#[derive(Debug, Default, serde::Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: Option<String>,
}

async fn plugin_search(
    Query(sq): Query<SearchQuery>,
) -> Result<Html<String>, (StatusCode, String)> {
    render_search(sq.q.as_deref().unwrap_or(""))
        .await
        .map(Html)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// A single search hit — the shape `search_results.html` iterates.
#[derive(Debug, Serialize)]
struct SearchHit {
    slug: String,
    name: String,
    source: String,
    short_description: String,
}

/// Load + render the `/search` result fragment. Public so the render
/// smoke-test drives the query → fragment path without an axum runtime.
/// An empty (or whitespace-only) query short-circuits to the hint state
/// without touching the DB. The fragment does NOT extend `base.html` —
/// it's injected into the header dialog by client JS.
pub async fn render_search(q: &str) -> Result<String, String> {
    let trimmed = q.trim();

    let hits: Vec<SearchHit> = if trimmed.is_empty() {
        Vec::new()
    } else {
        // Name / crate / description substring match across approved,
        // non-deleted plugins. `Q::or` nests the three `.contains()`
        // LIKE predicates; the ORM renders the backend-correct LIKE.
        PluginModel::objects()
            .filter(plugin::MODERATION.eq("approved"))
            .filter(Q::or(
                Q::or(
                    plugin::NAME.contains(trimmed),
                    plugin::CRATE_NAME.contains(trimmed),
                ),
                plugin::SHORT_DESCRIPTION.contains(trimmed),
            ))
            .order_by(plugin::FEATURED.desc())
            .order_by(plugin::DISPLAY_ORDER.asc())
            .limit(8)
            .fetch()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|p| SearchHit {
                slug: p.slug,
                name: p.name,
                source: source_str(p.source).to_string(),
                short_description: p.short_description,
            })
            .collect()
    };

    umbra::templates::render(
        "plugin_directory/search_results.html",
        &context! {
            q => trimmed,
            hits => hits,
        },
    )
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Detail — /plugins/{slug}
// ---------------------------------------------------------------------------

/// Query string for the detail page: `?submitted=1` after a successful
/// note POST renders the "pending moderation" success banner.
#[derive(Debug, Default, serde::Deserialize)]
struct DetailQuery {
    submitted: Option<String>,
}

async fn plugin_detail(
    Path(slug): Path<String>,
    Query(q): Query<DetailQuery>,
) -> Result<Html<String>, (StatusCode, String)> {
    if slug == "submit" {
        return render("plugin_directory/submit.html", &serde_json::json!({}));
    }

    let submitted = q.submitted.as_deref() == Some("1");
    match render_detail_with(&slug, submitted)
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

/// Handle a posted community note (`POST /plugins/{slug}/notes`). Looks
/// up the approved plugin by slug (404 if gone), creates a
/// [`PluginComment`] through the ORM with `moderation = Pending` (so it
/// awaits moderation), then redirects back to the detail page with
/// `?submitted=1` so the success banner renders. The redirect (a fresh
/// GET) is the POST/redirect/GET pattern — a refresh won't re-submit.
async fn post_plugin_note(
    Path(slug): Path<String>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Redirect, (StatusCode, String)> {
    let body = form.get("body").map(|s| s.trim()).unwrap_or("");
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "A note body is required.".into()));
    }
    let kind = form.get("kind").map(String::as_str).unwrap_or("general");
    let author_label = form
        .get("author_label")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    match create_note(&slug, body, kind, author_label)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    {
        true => Ok(Redirect::to(&format!("/plugins/{slug}?submitted=1"))),
        false => Err((
            StatusCode::NOT_FOUND,
            format!("No plugin directory entry exists for `{slug}`."),
        )),
    }
}

/// Create a pending [`PluginComment`] for the approved plugin `slug`.
/// Returns `Ok(false)` when no approved plugin matches the slug (a 404).
/// Public so the render smoke-test can drive the create path without an
/// axum runtime. `kind` is the form's `CommentKind` string; an unknown
/// value falls back to `General`.
pub async fn create_note(
    slug: &str,
    body: &str,
    kind: &str,
    author_label: Option<String>,
) -> Result<bool, String> {
    let Some(plugin) = PluginModel::objects()
        .filter(plugin::SLUG.eq(slug))
        .filter(plugin::MODERATION.eq("approved"))
        .first()
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };

    let kind = match kind {
        "question" => CommentKind::Question,
        "usage_note" => CommentKind::UsageNote,
        "compatibility_note" => CommentKind::CompatibilityNote,
        "migration_note" => CommentKind::MigrationNote,
        _ => CommentKind::General,
    };

    let mut comment = pd::PluginComment::default();
    comment.plugin = ForeignKey::new(plugin.id);
    comment.body = body.to_string();
    comment.kind = kind;
    comment.moderation = CommentModeration::Pending;
    comment.author_label = author_label;

    pd::PluginComment::objects()
        .create(comment)
        .await
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Load + render the `/plugins/{slug}` detail page. `Ok(None)` means
/// no approved plugin matches the slug (a 404). Public so the render
/// smoke-test drives the full reverse-relation → view-model → template
/// path directly.
pub async fn render_detail(slug: &str) -> Result<Option<String>, String> {
    render_detail_with(slug, false).await
}

/// `render_detail`, with the `?submitted=1` success-banner flag threaded
/// through to the template.
pub async fn render_detail_with(slug: &str, submitted: bool) -> Result<Option<String>, String> {
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
    umbra::templates::render(
        "plugin_directory/plugin.html",
        &context!(plugin => detail, submitted => submitted),
    )
    .map(Some)
    .map_err(|e| e.to_string())
}

/// The detail view-model the `plugin.html` template renders.
#[derive(Debug, Serialize)]
struct PluginDetail {
    slug: String,
    name: String,
    crate_name: String,
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
    /// The canonical `umbra add <crate>` line (always present, copyable).
    add_line: String,
    toml: String,
    /// Humanized GitHub stars, or `—`. Never fabricated.
    stars: String,
    /// Humanized downloads, or `—`.
    installs: String,
    /// `version` or `—`.
    version: String,
    /// `—` when there's no real rating data — never fabricated.
    rating: String,
    /// Real visible comment count.
    notes: i64,
    tags: Vec<String>,
    /// External links, each only present when its field is set.
    links: Links,
    features: Vec<FeatureRow>,
    shipped_features: usize,
    total_features: usize,
    progress_pct: u32,
    usage_title: String,
    usage_intro: String,
    usage_code: String,
    audit_label: &'static str,
    audit_date: String,
    audit_kind: &'static str,
    /// Per-row compatibility detail for the Compatibility tab.
    compat: Vec<CompatRow>,
    /// Compact compatibility summary for the sidebar (one StatusRow/row).
    compatibility: Vec<StatusRow>,
    comments: Vec<CommentPreview>,
    /// CommentKind options for the "Add a note" dialog select.
    note_kinds: Vec<NoteKind>,
}

/// External links shown in the header links row + Issues tab. A field is
/// `None` when the underlying URL is absent — the template omits the link
/// rather than rendering a dead one (honesty rule).
#[derive(Debug, Serialize)]
struct Links {
    docs: Option<String>,
    source: Option<String>,
    issues: Option<String>,
    /// `source` again, but only when it's a github.com URL (drives the
    /// "View on GitHub" social link).
    github: Option<String>,
}

/// One feature in the tracker, driven by its individual `PluginFeature`
/// row: name, description (markdown), a status badge and the maturity.
#[derive(Debug, Serialize)]
struct FeatureRow {
    name: String,
    description: String,
    status: String,
    kind: &'static str,
    maturity: String,
}

/// One compatibility declaration, expanded for the Compatibility tab:
/// the Umbra version, backend chips, MSRV, and verified-vs-declared.
#[derive(Debug, Serialize)]
struct CompatRow {
    umbra_version: String,
    backends: Vec<Backend>,
    minimum_rust_version: Option<String>,
    notes: Option<String>,
    verified: bool,
    verified_at: Option<String>,
}

/// A single database-backend chip ("PostgreSQL" / "SQLite" / "MySQL").
#[derive(Debug, Serialize)]
struct Backend {
    label: String,
}

/// One option in the "Add a note" dialog `kind` select.
#[derive(Debug, Serialize)]
struct NoteKind {
    value: &'static str,
    label: &'static str,
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
        let audit_date = p.audit_status.ne_or_label().to_string();

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

        let feature_rows: Vec<FeatureRow> = features
            .into_iter()
            .map(|f| {
                let (status, kind) = feature_status(f.status);
                FeatureRow {
                    name: f.name,
                    description: f.description,
                    status,
                    kind,
                    maturity: title_case(&format!("{:?}", f.maturity)),
                }
            })
            .collect();

        // Sidebar summary: one compact StatusRow per declaration.
        let compat_rows: Vec<StatusRow> = compatibility
            .iter()
            .map(|c| {
                let backends = backend_summary(&c.supported_database_backends);
                let label = if backends.is_empty() {
                    c.umbra_version.clone()
                } else {
                    format!("{} · {}", c.umbra_version, backends)
                };
                let kind = if c.verified_at.is_some() {
                    "ok"
                } else {
                    "warn"
                };
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

        // Compatibility tab: the full row with backend chips + MSRV.
        let compat_detail: Vec<CompatRow> = compatibility
            .into_iter()
            .map(|c| {
                let backends = backend_chips(&c.supported_database_backends);
                CompatRow {
                    umbra_version: c.umbra_version,
                    backends,
                    minimum_rust_version: c.minimum_rust_version,
                    notes: c.notes,
                    verified: c.verified_at.is_some(),
                    verified_at: c.verified_at.map(|d| d.format("%b %-d, %Y").to_string()),
                }
            })
            .collect();

        let comment_previews: Vec<CommentPreview> = comments
            .into_iter()
            .map(CommentPreview::from_model)
            .collect();
        let notes = comment_previews.len() as i64;

        let install = install_line(&p.installation_commands, &p.crate_name);
        let add_line = format!("umbra add {}", p.crate_name);
        let maintainer = p.author.clone();
        let source = source_str(p.source).to_string();
        let channel = match p.source {
            PluginSource::Official => "Stable channel",
            PluginSource::Community => "Community channel",
            PluginSource::Experimental => "Experimental channel",
            PluginSource::Deprecated => "Deprecated",
        }
        .to_string();

        let github = p
            .source_url
            .as_ref()
            .filter(|u| u.contains("github.com"))
            .cloned();
        let links = Links {
            docs: nonempty(p.docs_url.clone()),
            source: nonempty(p.source_url.clone()),
            issues: nonempty(p.issue_tracker_url.clone()),
            github,
        };

        Self {
            slug: p.slug.clone(),
            name: p.name.clone(),
            crate_name: p.crate_name.clone(),
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
            add_line,
            toml: p.installation_commands.clone(),
            stars: humanize_opt(p.github_stars),
            installs: humanize_opt(p.downloads),
            version: p
                .version
                .clone()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "—".to_string()),
            rating: "—".to_string(),
            notes,
            tags: derive_tags(&p),
            links,
            features: feature_rows,
            shipped_features,
            total_features,
            progress_pct,
            usage_title: "Usage".to_string(),
            usage_intro: p.setup_notes.clone().unwrap_or_else(|| {
                "Add the plugin to your project's plugin list and wire it in \
                     `main.rs` like every other Umbra battery."
                    .to_string()
            }),
            usage_code: p.installation_commands.clone(),
            audit_label,
            audit_date,
            audit_kind,
            compat: compat_detail,
            compatibility: compat_rows,
            comments: comment_previews,
            note_kinds: note_kinds(),
        }
    }
}

/// `Some(s)` only when `s` is present and non-blank; collapses an empty
/// URL string to `None` so the template omits a dead link.
fn nonempty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.trim().is_empty())
}

/// The `CommentKind` options shown in the "Add a note" dialog select.
fn note_kinds() -> Vec<NoteKind> {
    vec![
        NoteKind {
            value: "general",
            label: "General",
        },
        NoteKind {
            value: "question",
            label: "Question",
        },
        NoteKind {
            value: "usage_note",
            label: "Usage note",
        },
        NoteKind {
            value: "compatibility_note",
            label: "Compatibility note",
        },
        NoteKind {
            value: "migration_note",
            label: "Migration note",
        },
    ]
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
    /// Humanized creation date ("Jun 11, 2026").
    created: String,
    /// `plugin_version` tag, when the visitor set one.
    plugin_version: Option<String>,
    /// `database_backend` tag, when the visitor set one.
    backend: Option<String>,
}

impl CommentPreview {
    fn from_model(c: pd::PluginComment) -> Self {
        let name = c
            .author_label
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Anonymous".to_string());
        let created = c.created_at.format("%b %-d, %Y").to_string();
        let plugin_version = c.plugin_version.clone().filter(|s| !s.trim().is_empty());
        let backend = c.database_backend.clone().filter(|s| !s.trim().is_empty());
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
            created,
            plugin_version,
            backend,
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

/// Normalized backend chips from the JSON `supported_database_backends`
/// array (`["postgres","sqlite"]` → `[PostgreSQL, SQLite]`), for the
/// Compatibility tab's chip row.
fn backend_chips(v: &serde_json::Value) -> Vec<Backend> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|b| b.as_str())
        .map(|b| Backend {
            label: match b.to_ascii_lowercase().as_str() {
                "postgres" | "postgresql" => "PostgreSQL".to_string(),
                "sqlite" => "SQLite".to_string(),
                "mysql" => "MySQL".to_string(),
                other => title_case(other),
            },
        })
        .collect()
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
