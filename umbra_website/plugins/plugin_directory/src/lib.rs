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
use umbra::forms::{FormValidate, ValidationErrors};
use umbra::migrate::ModelMeta;
use umbra::plugin::{AppContext, Plugin, PluginError};
use umbra::prelude::*;
use umbra::routes::RouteSpec;
use umbra::templates::context;
use umbra::web::{
    Form, HeaderMap, Html, IntoResponse, Path, Query, Redirect, Response, Router, StatusCode, get,
    post,
};

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
            .route("/plugins/submit", get(submit_page).post(post_submission))
            .route("/plugins/{slug}", get(plugin_detail))
            .route("/plugins/{slug}/notes", post(post_plugin_note))
            .route("/report", get(report_page).post(post_report))
            .route("/search", get(plugin_search))
    }

    fn route_paths(&self) -> Vec<RouteSpec> {
        vec![
            RouteSpec::new("/prebuilt", vec!["GET"]),
            RouteSpec::new("/plugins", vec!["GET"]),
            RouteSpec::new("/plugins/submit", vec!["GET", "POST"]),
            RouteSpec::new("/plugins/{slug}", vec!["GET"]),
            RouteSpec::new("/plugins/{slug}/notes", vec!["POST"]),
            RouteSpec::new("/report", vec!["GET", "POST"]),
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
            // Back-fill the editorial audit status on existing rows (the
            // row insert above short-circuits once populated, so these
            // updates are how already-seeded directories gain their audit
            // values). Idempotent — only rows still at the default move.
            match seed::backfill_audit_status().await {
                Ok(0) => {}
                Ok(n) => tracing::info!("{}: back-filled audit status on {} rows", plugin_name, n),
                Err(e) => tracing::warn!("{}: audit-status back-fill failed: {e}", plugin_name),
            }
            // Seed each official plugin's feature tracker rows so the
            // /prebuilt grid and the /plugins/{slug} tracker render real
            // data. Idempotent per plugin (back-fills already-seeded rows).
            match seed::seed_plugin_features().await {
                Ok(0) => {}
                Ok(n) => tracing::info!("{}: seeded {} plugin feature rows", plugin_name, n),
                Err(e) => tracing::warn!("{}: plugin-feature seed failed: {e}", plugin_name),
            }
            // Seed demo discussion notes so the admin dashboard's
            // engagement widgets have real data. Idempotent.
            match seed::seed_demo_comments().await {
                Ok(0) => {}
                Ok(n) => tracing::info!("{}: seeded {} demo discussion notes", plugin_name, n),
                Err(e) => tracing::warn!("{}: demo comment seed failed: {e}", plugin_name),
            }
        });
        Ok(())
    }
}

async fn prebuilt_plugins() -> Result<Html<String>, (StatusCode, String)> {
    render_prebuilt().await.map(Html).map_err(internal_error)
}

/// One official-plugin card on `/prebuilt` — the plugin plus its feature
/// tracker rows.
#[derive(Debug, Serialize)]
struct PrebuiltCard {
    slug: String,
    /// Dotted crate name for display: `umbra-admin` → `umbra.admin`.
    crate_dotted: String,
    /// Two-letter monogram tile.
    icon: String,
    short_description: String,
    /// "Shipped / stable" — status label + maturity.
    status: String,
    /// "ok" / "warn" / "muted" — drives the pill colour.
    status_kind: &'static str,
    /// The `plugins += ["umbra.admin"]` install line.
    install: String,
    docs_url: Option<String>,
    features: Vec<PrebuiltFeature>,
}

/// One row in a `/prebuilt` card's feature tracker.
#[derive(Debug, Serialize)]
struct PrebuiltFeature {
    name: String,
    /// "shipped" / "usable" / "experimental" / "planned" / …
    status: String,
    /// "ok" / "warn" / "muted" — drives the dot + label colour.
    kind: &'static str,
}

/// Load + render `/prebuilt`: every official, approved plugin with its
/// feature tracker, in one parents + one children query
/// (`prefetch_related("feature_set")`) — no N+1. Public so the render
/// smoke-test drives the full query → view-model → template path without
/// an axum runtime.
pub async fn render_prebuilt() -> Result<String, String> {
    let plugins = PluginModel::objects()
        .filter(plugin::MODERATION.eq("approved"))
        .filter(plugin::SOURCE.eq("official"))
        .order_by(plugin::FEATURED.desc())
        .order_by(plugin::DISPLAY_ORDER.asc())
        .order_by(plugin::ID.asc())
        .fetch()
        .await
        .map_err(|e| e.to_string())?;

    // Batch-load every plugin's visible features in ONE query (no N+1):
    // `WHERE plugin IN (<ids>)`, then group by plugin in memory. (We use
    // the IN-batch rather than `prefetch_related("feature_set")` because
    // that path returns empty buckets for a second reverse-FK field on the
    // same model — see planning/orm_fixes.md #1.)
    let ids: Vec<i64> = plugins.iter().map(|p| p.id).collect();
    let mut features_by_plugin: HashMap<i64, Vec<PrebuiltFeature>> = HashMap::new();
    if !ids.is_empty() {
        let rows = PluginFeature::objects()
            .filter(plugin_feature::PLUGIN.in_(&ids))
            .filter(plugin_feature::VISIBLE.eq(true))
            .order_by(plugin_feature::DISPLAY_ORDER.asc())
            .order_by(plugin_feature::ID.asc())
            .fetch()
            .await
            .map_err(|e| e.to_string())?;
        for f in rows {
            let (status, kind) = feature_status(f.status);
            features_by_plugin
                .entry(f.plugin.id())
                .or_default()
                .push(PrebuiltFeature {
                    name: f.name,
                    status,
                    kind,
                });
        }
    }

    let cards: Vec<PrebuiltCard> = plugins
        .into_iter()
        .map(|p| {
            let features = features_by_plugin.remove(&p.id).unwrap_or_default();
            let (status_label, status_kind) = status_badge(p.status);
            let maturity = format!("{:?}", p.maturity).to_lowercase();
            PrebuiltCard {
                slug: p.slug.clone(),
                crate_dotted: p.crate_name.replace('-', "."),
                icon: initials(&p.name),
                short_description: p.short_description.clone(),
                status: format!("{status_label} / {maturity}"),
                status_kind,
                install: format!("plugins += [\"{}\"]", p.crate_name.replace('-', ".")),
                docs_url: nonempty(p.docs_url.clone()),
                features,
            }
        })
        .collect();

    umbra::templates::render("plugin_directory/prebuilt.html", &context! { plugins => cards })
        .map_err(|e| e.to_string())
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
    /// `?audited=1` shows only audited plugins (the homepage "Audited"
    /// tab links here). Mutually exclusive with `source` in the UI.
    audited: Option<String>,
    /// `?search=<q>` — the landing-page and directory search box. Matches
    /// name / crate name / short description (case-insensitive substring).
    search: Option<String>,
    page: Option<u32>,
}

/// True for `?audited=1` / `?audited=true` — anything else is "off".
fn truthy(v: Option<&str>) -> bool {
    matches!(v, Some("1") | Some("true") | Some("yes") | Some("on"))
}

/// Case-insensitive substring match across the fields a user expects the
/// search box to cover: name, crate name, and short description. ORed via
/// `Predicate: BitOr` (string columns have no single multi-field search).
fn search_predicate(q: &str) -> umbra::orm::Predicate<PluginModel> {
    plugin::NAME.icontains(q)
        | plugin::CRATE_NAME.icontains(q)
        | plugin::SHORT_DESCRIPTION.icontains(q)
}

/// Cards per listing page.
const PAGE_SIZE: i64 = 12;

/// Predicate for the "Audited" facet: a plugin counts as audited when an
/// Umbra or third-party reviewer has signed off. The string column has no
/// `in_`, so this ORs the two `eq` predicates (`Predicate: BitOr`) — the
/// same definition the `PluginCard.audited` badge uses.
fn audited_predicate() -> umbra::orm::Predicate<PluginModel> {
    plugin::AUDIT_STATUS.eq("umbra_reviewed") | plugin::AUDIT_STATUS.eq("third_party_reviewed")
}

async fn plugin_directory(
    Query(q): Query<ListingQuery>,
) -> Result<Html<String>, (StatusCode, String)> {
    render_listing(
        q.source.as_deref(),
        truthy(q.audited.as_deref()),
        q.search.as_deref(),
        q.page.unwrap_or(1),
    )
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
pub async fn render_listing(
    source: Option<&str>,
    audited: bool,
    search: Option<&str>,
    page: u32,
) -> Result<String, String> {
    // The active source facet, validated against the known variants so a
    // junk `?source=` value doesn't silently filter to nothing. The
    // `audited` facet takes precedence: when on, the source facet is
    // ignored so the two never compose into a confusing combined filter.
    let active_source = if audited {
        None
    } else {
        source.filter(|s| matches!(*s, "official" | "community" | "experimental" | "deprecated"))
    };

    // Trimmed, non-empty search term — `?search=` / whitespace acts like
    // no search at all.
    let search = search.map(str::trim).filter(|s| !s.is_empty());

    // Real facet counts — one `SELECT COUNT(*)` per facet, never a
    // fetch-all-and-count-in-memory.
    let counts = FacetCounts::load().await.map_err(|e| e.to_string())?;

    // `total` is the count for the *current view*. With a search term the
    // facet counts no longer describe what's shown, so we count the
    // matching rows directly (search composed with the active facet);
    // without one we reuse the cheap precomputed facet count.
    let total = if let Some(q) = search {
        let mut c = PluginModel::objects().filter(plugin::MODERATION.eq("approved"));
        if audited {
            c = c.filter(audited_predicate());
        } else if let Some(src) = active_source {
            c = c.filter(plugin::SOURCE.eq(src));
        }
        c.filter(search_predicate(q))
            .count()
            .await
            .map_err(|e| e.to_string())?
    } else if audited {
        counts.audited
    } else {
        match active_source {
            Some("official") => counts.official,
            Some("community") => counts.community,
            Some("experimental") => counts.experimental,
            Some("deprecated") => counts.deprecated,
            _ => counts.total,
        }
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
    if audited {
        listing = listing.filter(audited_predicate());
    } else if let Some(src) = active_source {
        listing = listing.filter(plugin::SOURCE.eq(src));
    }
    if let Some(q) = search {
        listing = listing.filter(search_predicate(q));
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
            active_audited => audited,
            search => search.unwrap_or(""),
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
    /// Plugins whose audit_status is umbra- or third-party-reviewed.
    /// Drives the clickable "Audited" facet (`?audited=1`).
    audited: i64,
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
        let audited = PluginModel::objects()
            .filter(plugin::MODERATION.eq("approved"))
            .filter(audited_predicate())
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
            audited,
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
    /// Storage key for the logo image, resolved to a URL by `media_url()`.
    logo: Option<String>,
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
            logo: p.logo.map(|f| f.key().to_string()),
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
    /// Storage key for the logo image, resolved to a URL by `media_url()`.
    logo: Option<String>,
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
                logo: p.logo.map(|f| f.key().to_string()),
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
    // `/plugins/submit` is served by its own static route (registered
    // before this `/plugins/{slug}` matcher), so the submit page never
    // reaches here — this is the canonical plugin-detail path only.
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

/// Minimum seconds between two note posts from the same browser. A courtesy
/// throttle (the cookie is trivially cleared), paired with framework rate
/// limiting later.
const NOTE_MIN_INTERVAL_SECS: i64 = 20;

/// Handle a posted community note (`POST /plugins/{slug}/notes`). Looks up the
/// approved plugin by slug (404 if gone) and creates a visible
/// [`PluginComment`] through the ORM (publish-then-moderate: an admin sets
/// `Hidden` later to take it down).
///
/// Progressive enhancement: a `fetch()` submit (the note dialog sets `Accept:
/// application/json`) gets `{ ok, id, html }` back and inserts the rendered
/// row in place, so the page never reloads; the same row also rides the SSE
/// feed to every other open tab. A plain `<form>` submit (no JS) takes the
/// POST/redirect/GET path so a refresh won't re-submit. Two light guards run
/// first: a honeypot field and a per-browser submit interval.
async fn post_plugin_note(
    Path(slug): Path<String>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let json = wants_json(&headers);

    let body = form.get("body").map(|s| s.trim()).unwrap_or("");
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "A note body is required.".into()));
    }

    // Honeypot: a real visitor never sees the `website` field. A non-empty
    // value is a bot, so accept silently (give it no signal) but write and
    // broadcast nothing.
    if form
        .get("website")
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
    {
        return Ok(if json {
            note_json_response(r#"{"ok":true,"skipped":true}"#.to_string(), None)
        } else {
            Redirect::to(&format!("/plugins/{slug}?submitted=1")).into_response()
        });
    }

    // Per-browser throttle: reject a second post inside the interval window.
    if !note_interval_ok(&headers) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "You're posting too fast. Give it a few seconds.".into(),
        ));
    }

    let kind = form.get("kind").map(String::as_str).unwrap_or("general");
    let author_label = form
        .get("author_label")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let Some(payload) = create_note(&slug, body, kind, author_label, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("No plugin directory entry exists for `{slug}`."),
        ));
    };

    let cookie = note_throttle_cookie();
    if json {
        let body = serde_json::to_string(&serde_json::json!({
            "ok": true,
            "id": payload.id,
            "html": payload.html,
        }))
        .unwrap_or_else(|_| r#"{"ok":true}"#.to_string());
        Ok(note_json_response(body, Some(cookie)))
    } else {
        // No-JS fallback: POST/redirect/GET so a refresh won't re-submit.
        let mut resp = Redirect::to(&format!("/plugins/{slug}?submitted=1")).into_response();
        set_throttle_cookie(&mut resp, &cookie);
        Ok(resp)
    }
}

/// True when the client prefers a JSON response (the note dialog's
/// `fetch()` sets `Accept: application/json`). A plain browser form
/// submit sends `Accept: text/html`, so it takes the redirect path.
fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(umbra::web::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("application/json"))
        .unwrap_or(false)
}

/// A JSON 200, optionally carrying the throttle `Set-Cookie`.
fn note_json_response(body: String, cookie: Option<String>) -> Response {
    let mut resp = (
        StatusCode::OK,
        [(umbra::web::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response();
    if let Some(c) = cookie {
        set_throttle_cookie(&mut resp, &c);
    }
    resp
}

/// The `pd_nt` cookie value stamping the current post time so the throttle
/// survives reloads. HttpOnly + SameSite=Lax + site-wide path.
fn note_throttle_cookie() -> String {
    format!(
        "pd_nt={}; Path=/; Max-Age=86400; HttpOnly; SameSite=Lax",
        chrono::Utc::now().timestamp()
    )
}

/// Attach a `Set-Cookie` header, ignoring an unencodable value.
fn set_throttle_cookie(resp: &mut Response, cookie: &str) {
    if let Ok(v) = umbra::web::header::HeaderValue::from_str(cookie) {
        resp.headers_mut()
            .insert(umbra::web::header::SET_COOKIE, v);
    }
}

/// True when enough time has passed since the last `pd_nt` post, or there is
/// no parseable cookie. False only when a recent timestamp is present.
fn note_interval_ok(headers: &HeaderMap) -> bool {
    let last = headers
        .get(umbra::web::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| {
            raw.split(';').find_map(|kv| {
                kv.trim()
                    .strip_prefix("pd_nt=")
                    .and_then(|n| n.parse::<i64>().ok())
            })
        });
    match last {
        Some(ts) => chrono::Utc::now().timestamp() - ts >= NOTE_MIN_INTERVAL_SECS,
        None => true,
    }
}

/// Create a visible [`PluginComment`] for the approved plugin `slug` and
/// return its rendered row as a [`NotePayload`]. Returns `Ok(None)` when no
/// approved plugin matches the slug (a 404). Public so the render smoke-test
/// can drive the create path without an axum runtime. `kind` is the form's
/// `CommentKind` string; an unknown value falls back to `General`.
pub async fn create_note(
    slug: &str,
    body: &str,
    kind: &str,
    author_label: Option<String>,
    parent: Option<i64>,
) -> Result<Option<NotePayload>, String> {
    let Some(plugin) = PluginModel::objects()
        .filter(plugin::SLUG.eq(slug))
        .filter(plugin::MODERATION.eq("approved"))
        .first()
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };

    // A reply: the parent must be a VISIBLE, top-level (parent IS NULL) comment
    // on THIS plugin. Anything else (unknown id, hidden, cross-plugin, or a
    // reply-to-a-reply) is rejected as a 404 so it can't be forged. Depth stays 1.
    if let Some(parent_id) = parent {
        let ok = match pd::PluginComment::objects()
            .filter(plugin_comment::ID.eq(parent_id))
            .filter(plugin_comment::MODERATION.eq("visible"))
            .first()
            .await
            .map_err(|e| e.to_string())?
        {
            Some(p) => p.plugin.id() == plugin.id && p.parent.is_none(),
            None => false,
        };
        if !ok {
            return Ok(None);
        }
    }

    let kind = match kind {
        "question" => CommentKind::Question,
        "usage_note" => CommentKind::UsageNote,
        "compatibility_note" => CommentKind::CompatibilityNote,
        "migration_note" => CommentKind::MigrationNote,
        _ => CommentKind::General,
    };

    let mut comment = pd::PluginComment::default();
    comment.plugin = ForeignKey::new(plugin.id);
    comment.parent = parent.map(ForeignKey::new);
    comment.body = body.to_string();
    comment.kind = kind;
    // Publish-then-moderate: the note is visible immediately. An admin sets
    // `Hidden` later to take it down (the detail query + count both filter on
    // `visible`, so hiding drops it from the thread on the next load). The
    // body is sanitized at render time by the `markdown` filter, which is the
    // XSS boundary now that an unmoderated body goes public on submit.
    comment.moderation = CommentModeration::Visible;
    comment.author_label = author_label;

    let created = pd::PluginComment::objects()
        .create(comment)
        .await
        .map_err(|e| e.to_string())?;

    // Render the new row exactly as the page loop would, then ship it.
    let id = created.id;
    let preview = CommentPreview::from_model(created);
    let html = render_comment_row(&preview)?;

    // Live note feed (SSE): everyone watching this plugin's thread gets the
    // rendered row and inserts it in place. No-op when RealtimePlugin isn't
    // installed (e.g. the render smoke-test).
    umbra_realtime::Realtime::to_group(format!("public:plugin-{}", plugin.id))
        .send(
            "note",
            &serde_json::json!({ "id": id, "html": html, "parent_id": parent }),
        )
        .await;
    Ok(Some(NotePayload {
        id,
        html,
        parent_id: parent,
    }))
}

// ---------------------------------------------------------------------------
// Report an issue — GET /report?plugin=<slug>, POST /report
// ---------------------------------------------------------------------------

/// The category options shown in the report form's `<select>`. The value
/// is stored verbatim in the comment body prefix (`[security] …`) so a
/// moderator sees the reporter's classification in the queue.
const REPORT_CATEGORIES: &[(&str, &str)] = &[
    ("security", "Security vulnerability"),
    ("bug", "Bug / broken behaviour"),
    ("malware", "Malware / abuse"),
    ("licensing", "Licensing concern"),
    ("other", "Something else"),
];

/// Query string for the report page: `?plugin=<slug>` prefills the
/// target plugin; `?submitted=1` renders the success state after a POST.
#[derive(Debug, Default, serde::Deserialize)]
struct ReportQuery {
    plugin: Option<String>,
    submitted: Option<String>,
}

/// One category option handed to `report.html`.
#[derive(Debug, Serialize)]
struct ReportCategory {
    value: &'static str,
    label: &'static str,
}

fn report_categories() -> Vec<ReportCategory> {
    REPORT_CATEGORIES
        .iter()
        .map(|(value, label)| ReportCategory { value, label })
        .collect()
}

async fn report_page(Query(q): Query<ReportQuery>) -> Result<Html<String>, (StatusCode, String)> {
    let submitted = q.submitted.as_deref() == Some("1");
    render_report(q.plugin.as_deref(), submitted, None, &HashMap::new())
        .await
        .map(Html)
        .map_err(internal_error)
}

/// Handle a posted issue report (`POST /report`). Records the report as a
/// pending [`PluginComment`] on the named plugin, then redirects back to
/// the report page with `?submitted=1` (POST/redirect/GET, so a refresh
/// won't re-file). A missing `details` field re-renders the form with the
/// error instead of crashing.
async fn post_report(
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let slug = form
        .get("plugin")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let category = form
        .get("category")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("other");
    let details = form.get("details").map(|s| s.trim()).unwrap_or("");

    match create_report(slug.as_deref(), category, details).await {
        Ok(()) => {
            let target = slug.as_deref().unwrap_or("");
            let url = if target.is_empty() {
                "/report?submitted=1".to_string()
            } else {
                format!("/report?plugin={target}&submitted=1")
            };
            Ok(Redirect::to(&url).into_response())
        }
        // A validation miss (empty details) re-renders the form with the
        // typed-in values kept and the error shown under the field.
        Err(errs) => {
            let html = render_report(slug.as_deref(), false, Some(&errs), &form)
                .await
                .map_err(internal_error)?;
            Ok((StatusCode::UNPROCESSABLE_ENTITY, Html(html)).into_response())
        }
    }
}

/// File an issue report as a pending [`PluginComment`] through the ORM.
/// `details` must be non-empty (else a field-keyed [`ValidationErrors`]).
/// When `slug` names an approved plugin the report attaches to it; an
/// unknown / missing slug is accepted as a free-form report against the
/// first approved plugin we can find so the moderator still sees it — and
/// if the directory is empty the report is rejected with a form-level
/// note rather than a 500. Public so the render smoke-test can drive the
/// create path without an axum runtime.
pub async fn create_report(
    slug: Option<&str>,
    category: &str,
    details: &str,
) -> Result<(), ValidationErrors> {
    let details = details.trim();
    if details.is_empty() {
        let mut errs = ValidationErrors::new();
        errs.add("details", "Please describe the issue.");
        return Err(errs);
    }

    // Resolve the plugin: the named slug if it exists, else (for a
    // generic / unknown-slug report) the first approved plugin so the
    // note still lands in a moderation queue a human watches.
    let target = match slug.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => PluginModel::objects()
            .filter(plugin::SLUG.eq(s))
            .filter(plugin::MODERATION.eq("approved"))
            .first()
            .await
            .map_err(write_to_validation_str)?,
        None => None,
    };
    let target = match target {
        Some(p) => Some(p),
        None => PluginModel::objects()
            .filter(plugin::MODERATION.eq("approved"))
            .order_by(plugin::ID.asc())
            .first()
            .await
            .map_err(write_to_validation_str)?,
    };

    let Some(plugin) = target else {
        let mut errs = ValidationErrors::new();
        errs.add_non_field("There are no plugins to report against yet.");
        return Err(errs);
    };

    let mut comment = pd::PluginComment::default();
    comment.plugin = ForeignKey::new(plugin.id);
    // Prefix the moderator-facing body with the reporter's category so the
    // queue reads "[security] <details>" at a glance.
    comment.body = format!("[{category}] {details}");
    comment.kind = CommentKind::General;
    comment.moderation = CommentModeration::Pending;
    comment.author_label = Some("Issue report".to_string());

    pd::PluginComment::objects()
        .create(comment)
        .await
        .map_err(write_to_validation)?;
    Ok(())
}

/// View-model handed to `report.html`.
#[derive(Debug, Serialize)]
struct ReportView {
    /// The slug being reported (hidden field + back-link target).
    plugin_slug: String,
    /// The resolved plugin name when the slug matched an approved row,
    /// else `None` (a generic report).
    plugin_name: Option<String>,
    categories: Vec<ReportCategory>,
    submitted: bool,
}

/// Load + render the report page. `errors` carries a failed submission's
/// field map (so the template repopulates + shows red error text);
/// `form` is the raw submitted pairs (kept on the re-render). Public so
/// the render smoke-test can exercise the template path directly.
pub async fn render_report(
    slug: Option<&str>,
    submitted: bool,
    errors: Option<&ValidationErrors>,
    form: &HashMap<String, String>,
) -> Result<String, String> {
    let slug = slug
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Look the plugin up by slug to show its real name (honest: only when
    // the row exists + is approved — otherwise the form stays generic).
    let plugin_name = match &slug {
        Some(s) => PluginModel::objects()
            .filter(plugin::SLUG.eq(s))
            .filter(plugin::MODERATION.eq("approved"))
            .first()
            .await
            .map_err(|e| e.to_string())?
            .map(|p| p.name),
        None => None,
    };

    let view = ReportView {
        plugin_slug: slug.unwrap_or_default(),
        plugin_name,
        categories: report_categories(),
        submitted,
    };

    let errors_ctx = errors_to_ctx(errors);

    umbra::templates::render(
        "plugin_directory/report.html",
        &context! {
            report => view,
            errors => errors_ctx,
            form => form,
        },
    )
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Submit a plugin — GET /plugins/submit, POST /plugins/submit
// ---------------------------------------------------------------------------

/// Query string for the submit page: `?submitted=1` renders the
/// "submitted for review" success state after a POST/redirect/GET.
#[derive(Debug, Default, serde::Deserialize)]
struct SubmitQuery {
    submitted: Option<String>,
}

async fn submit_page(Query(q): Query<SubmitQuery>) -> Result<Html<String>, (StatusCode, String)> {
    let submitted = q.submitted.as_deref() == Some("1");
    render_submit(submitted, None, &HashMap::new())
        .await
        .map(Html)
        .map_err(internal_error)
}

/// Handle a posted plugin submission (`POST /plugins/submit`). Runs the
/// `Plugin` Form-derive validation; on success persists a `Community`
/// row with `moderation = Pending` and redirects to
/// `/plugins/submit?submitted=1`. On failure the form re-renders with the
/// per-field errors and the typed values kept.
async fn post_submission(
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    match create_submission(&form).await {
        Ok(_id) => Ok(Redirect::to("/plugins/submit?submitted=1").into_response()),
        Err(errs) => {
            let html = render_submit(false, Some(&errs), &form)
                .await
                .map_err(internal_error)?;
            Ok((StatusCode::UNPROCESSABLE_ENTITY, Html(html)).into_response())
        }
    }
}

/// Validate a public plugin submission through the `Plugin` Form derive,
/// set the server-managed fields (`source = Community`,
/// `moderation = Pending`, `created_by = None`), persist it through the
/// ORM, and return the new row's id. A UNIQUE clash (name / slug /
/// crate_name already taken) surfaces as a friendly field-keyed
/// [`ValidationErrors`], never a 500. Public so the render smoke-test can
/// drive the validate → create path without an axum runtime.
pub async fn create_submission(data: &HashMap<String, String>) -> Result<i64, ValidationErrors> {
    // `featured` / `display_order` carry a SQL `default` but no
    // `#[umbra(noform)]`, so the Form derive treats them as submittable
    // and required. They're not on the public form (a visitor must not
    // pick their own placement / featured flag), so inject the safe
    // server-side defaults before validation rather than exposing inputs.
    let mut data = data.clone();
    data.entry("featured".to_string())
        .or_insert_with(|| "false".to_string());
    data.entry("display_order".to_string())
        .or_insert_with(|| "0".to_string());

    // The Form derive validates the user-submittable `#[form(...)]`
    // fields and fills the `#[umbra(noform)]` ones from Default. Async
    // because `created_by` (an optional FK) is existence-checked when
    // present — an empty value skips the probe.
    let mut plugin = PluginModel::validate(&data).await?;

    // Server-managed: a public submission is always a pending community
    // row, never authored by an arbitrary user.
    plugin.source = PluginSource::Community;
    plugin.moderation = PluginModeration::Pending;
    plugin.created_by = None;

    let created = PluginModel::objects()
        .create(plugin)
        .await
        .map_err(write_to_validation)?;
    Ok(created.id)
}

/// Render the submit page. `errors` carries a failed submission's field
/// map; `form` is the raw submitted pairs (kept across the re-render).
/// Public so the render smoke-test drives the template path directly.
pub async fn render_submit(
    submitted: bool,
    errors: Option<&ValidationErrors>,
    form: &HashMap<String, String>,
) -> Result<String, String> {
    let errors_ctx = errors_to_ctx(errors);

    umbra::templates::render(
        "plugin_directory/submit.html",
        &context! {
            submitted => submitted,
            errors => errors_ctx,
            form => form,
        },
    )
    .map_err(|e| e.to_string())
}

/// Flatten a [`ValidationErrors`] into the template-friendly shape the
/// form pages expect: each field key maps to its FIRST message (so
/// `{{ errors.name }}` renders red text under the input), plus a `form`
/// key carrying the first non-field error for the page-level banner.
fn errors_to_ctx(errors: Option<&ValidationErrors>) -> serde_json::Value {
    let Some(errors) = errors else {
        return serde_json::Value::Null;
    };
    let mut out = serde_json::Map::new();
    for (field, msgs) in &errors.fields {
        if let Some(first) = msgs.first() {
            out.insert(field.clone(), serde_json::Value::String(first.clone()));
        }
    }
    if let Some(first) = errors.non_field.first() {
        out.insert("form".to_string(), serde_json::Value::String(first.clone()));
    }
    serde_json::Value::Object(out)
}

/// Lift an ORM [`WriteError`] (e.g. a UNIQUE clash on name/slug/crate)
/// into a field-keyed [`ValidationErrors`] so the form layer renders a
/// friendly message under the offending field instead of a 500.
fn write_to_validation(e: umbra::orm::write::WriteError) -> ValidationErrors {
    let mut errs = ValidationErrors::new();
    for (field, msgs) in e.field_errors() {
        for msg in msgs {
            errs.add(&field, msg);
        }
    }
    for msg in e.non_field_errors() {
        errs.add_non_field(msg);
    }
    // A WriteError with no parseable field (raw sqlx, etc.) still needs a
    // user-visible message — fall back to a generic non-field note.
    if errs.is_empty() {
        errs.add_non_field("We couldn't save your submission — please try again.");
    }
    errs
}

/// `write_to_validation`, but for the read paths in `create_report` that
/// only need a `String` error channel — collapse a failed lookup to its
/// `Display` text wrapped in a non-field [`ValidationErrors`].
fn write_to_validation_str(e: sqlx::Error) -> ValidationErrors {
    let mut errs = ValidationErrors::new();
    errs.add_non_field(e.to_string());
    errs
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
    /// Plugin PK — drives the live-note SSE group `public:plugin-<id>`.
    id: i64,
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
    /// Storage key for the logo image, resolved to a URL by `media_url()`.
    logo: Option<String>,
    /// Storage key for the cover image, resolved to a URL by `media_url()`.
    cover_image: Option<String>,
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
            id: p.id,
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
            logo: p.logo.map(|f| f.key().to_string()),
            cover_image: p.cover_image.map(|f| f.key().to_string()),
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
    /// PK, surfaced as `data-comment-id` so a live insert can dedupe
    /// against an already-rendered row.
    id: i64,
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
            id: c.id,
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

/// Render a single comment row to HTML using the same partial the page
/// loop includes, so a live-inserted note is byte-identical to a reloaded
/// one. The body is sanitized by the `markdown` filter (ammonia), so the
/// returned HTML is safe to broadcast and insert via `innerHTML`.
fn render_comment_row(preview: &CommentPreview) -> Result<String, String> {
    umbra::templates::render(
        "plugin_directory/_comment.html",
        &umbra::templates::context! { comment => preview },
    )
    .map_err(|e| e.to_string())
}

/// The live payload for a freshly posted note: the new row's PK and its
/// rendered HTML. Broadcast over SSE and returned to the AJAX submitter.
#[derive(Debug, Serialize)]
pub struct NotePayload {
    pub id: i64,
    pub html: String,
    /// The note this is a reply to, or `None` for a top-level note. Lets the
    /// client route a live insert into the right replies container.
    pub parent_id: Option<i64>,
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
