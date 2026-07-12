//! PluginDirectoryPlugin — owns the plugin directory for umbral.dev.
//!
//! Wire this into your App by adding to `src/main.rs`:
//!
//! ```ignore
//! .plugin(plugin_directory::PluginDirectoryPlugin::default())
//! ```
//!
//! Both the `/plugins` listing and the `/plugins/{slug}` detail page
//! are DB-driven: the listing loads every approved, non-deleted
//! `Plugin` in one annotated query (e.g.
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
    PluginMaturity, PluginModeration, PluginModerator, PluginSource, PluginStatus, SecurityStatus,
};

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use umbral::forms::{FormValidate, ValidationErrors};
use umbral::migrate::ModelMeta;
use umbral::plugin::{AppContext, Plugin, PluginError};
use umbral::prelude::*;
use umbral::routes::RouteSpec;
use umbral::templates::context;
use umbral::web::{ApiError, 
    get, post, Form, HeaderMap, Html, IntoResponse, Path, Query, Redirect, Response, Router,
    StatusCode,
};
use umbral_auth::{AuthUser, OptionalUser};

use models::{
    self as pd, plugin, plugin_comment, plugin_compatibility, plugin_feature, plugin_moderator,
    Plugin as PluginModel,
};

#[derive(Debug, Default, Clone)]
pub struct PluginDirectoryPlugin;

impl Plugin for PluginDirectoryPlugin {
    fn name(&self) -> &'static str {
        "plugin_directory"
    }

    /// FKs into `auth_user`. "plugin_directory" happens to sort after "auth"
    /// alphabetically, so this ordering held by luck rather than by contract;
    /// declaring it makes the toposort enforce what the schema requires.
    fn dependencies(&self) -> &'static [&'static str] {
        &["auth"]
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![
            ModelMeta::for_::<models::Plugin>(),
            ModelMeta::for_::<models::PluginFeature>(),
            ModelMeta::for_::<models::PluginCompatibility>(),
            ModelMeta::for_::<models::PluginComment>(),
            ModelMeta::for_::<models::PluginModerator>(),
        ]
    }

    fn routes(&self) -> Router {
        Router::new()
            .route("/prebuilt", get(prebuilt_plugins))
            .route("/plugins", get(plugin_directory))
            .route("/plugins/submit", get(submit_page).post(post_submission))
            .route("/plugins/{slug}", get(plugin_detail))
            .route("/plugins/{slug}/notes", post(post_plugin_note))
            .route("/plugins/{slug}/moderators", post(post_add_moderator))
            .route(
                "/plugins/{slug}/moderators/{user_id}/remove",
                post(post_remove_moderator),
            )
            .route(
                "/plugins/{slug}/comments/{comment_id}/moderate",
                post(post_moderate_comment),
            )
            .route(
                "/plugins/{slug}/issues/{comment_id}/resolve",
                post(post_resolve_issue),
            )
            .route(
                "/plugins/{slug}/issues/{comment_id}/reopen",
                post(post_reopen_issue),
            )
            // Self-service moderation area for plugin owners / moderators.
            // Aggregates the plugins a signed-in user owns or moderates and,
            // per plugin, the issues + notes they can act on — reusing the
            // `/plugins/{slug}/...` moderation POST endpoints above.
            .route("/account/plugins", get(account_plugins_page))
            .route("/account/plugins/{slug}", get(account_plugin_manage_page))
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
            RouteSpec::new("/plugins/{slug}/moderators", vec!["POST"]),
            RouteSpec::new("/plugins/{slug}/moderators/{user_id}/remove", vec!["POST"]),
            RouteSpec::new(
                "/plugins/{slug}/comments/{comment_id}/moderate",
                vec!["POST"],
            ),
            RouteSpec::new("/plugins/{slug}/issues/{comment_id}/resolve", vec!["POST"]),
            RouteSpec::new("/plugins/{slug}/issues/{comment_id}/reopen", vec!["POST"]),
            RouteSpec::new("/account/plugins", vec!["GET"]),
            RouteSpec::new("/account/plugins/{slug}", vec!["GET"]),
            RouteSpec::new("/report", vec!["GET", "POST"]),
            RouteSpec::new("/search", vec!["GET"]),
        ]
    }

    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        // Plugin-directory seed data is intentionally command-driven:
        // `cargo run -- seed_plugins` refreshes the catalog without making
        // normal web-server startup perform database writes.
        Ok(())
    }
}

async fn prebuilt_plugins() -> Result<Html<String>, ApiError> {
    render_prebuilt().await.map(Html).map_err(internal_error)
}

/// One official-plugin card on `/prebuilt` — the plugin plus its feature
/// tracker rows.
#[derive(Debug, Serialize)]
struct PrebuiltCard {
    slug: String,
    /// Dotted crate name for display: `umbral-admin` → `umbral.admin`.
    crate_dotted: String,
    /// Two-letter monogram tile.
    icon: String,
    short_description: String,
    /// "Shipped / stable" — status label + maturity.
    status: String,
    /// "ok" / "warn" / "muted" — drives the pill colour.
    status_kind: &'static str,
    /// The `plugins += ["umbral.admin"]` install line.
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

    umbral::templates::render(
        "plugin_directory/prebuilt.html",
        &context! { plugins => cards },
    )
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
fn search_predicate(q: &str) -> umbral::orm::Predicate<PluginModel> {
    plugin::NAME.icontains(q)
        | plugin::CRATE_NAME.icontains(q)
        | plugin::SHORT_DESCRIPTION.icontains(q)
}

/// Cards per listing page.
const PAGE_SIZE: i64 = 12;

/// Predicate for the "Audited" facet: a plugin counts as reviewed when an
/// Umbral, third-party, or maintainer self-review has signed off. The string
/// column has no `in_`, so this ORs the `eq` predicates (`Predicate: BitOr`)
/// — the same definition the `PluginCard.audited` badge uses.
fn audited_predicate() -> umbral::orm::Predicate<PluginModel> {
    plugin::AUDIT_STATUS.eq("self_reviewed")
        | plugin::AUDIT_STATUS.eq("umbral_reviewed")
        | plugin::AUDIT_STATUS.eq("third_party_reviewed")
}

async fn plugin_directory(
    Query(q): Query<ListingQuery>,
) -> Result<Html<String>, ApiError> {
    render_listing(
        q.source.as_deref(),
        truthy(q.audited.as_deref()),
        q.search.as_deref(),
        q.page.unwrap_or(1),
    )
    .await
    .map(Html)
    .map_err(ApiError::internal)
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
    // (`annotate(n=Count("comment_set"))`). Soft-deleted rows
    // are excluded automatically (Plugin is `#[umbral(soft_delete)]`).
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

    umbral::templates::render(
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
    /// Plugins whose audit_status is self-, umbral-, or third-party-reviewed.
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
    /// First non-empty line of `installation_commands`, else `umbral add
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
                AuditStatus::SelfReviewed
                    | AuditStatus::UmbralReviewed
                    | AuditStatus::ThirdPartyReviewed
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
) -> Result<Html<String>, ApiError> {
    render_search(sq.q.as_deref().unwrap_or(""))
        .await
        .map(Html)
        .map_err(ApiError::internal)
}

/// A single search hit — the shape `search_results.html` iterates. The global
/// search spans both plugins and blog posts; `kind` distinguishes them and
/// `href` is the link target.
#[derive(Debug, Serialize)]
struct SearchHit {
    /// "plugin" or "blog" — drives the result icon and the kind label.
    kind: &'static str,
    /// The link target: `/plugins/{slug}` or `/blog/{slug}`.
    href: String,
    name: String,
    /// Short label shown after the name (the plugin source, or "blog").
    label: String,
    short_description: String,
    /// Plugin logo storage key (resolved by `media_url()`); `None` for posts.
    logo: Option<String>,
}

/// Load + render the `/search` result fragment. Public so the render
/// smoke-test drives the query → fragment path without an axum runtime.
/// An empty (or whitespace-only) query short-circuits to the hint state
/// without touching the DB. The fragment does NOT extend `base.html` —
/// it's injected into the header dialog by client JS.
pub async fn render_search(q: &str) -> Result<String, String> {
    let trimmed = q.trim();

    let mut hits: Vec<SearchHit> = Vec::new();
    if !trimmed.is_empty() {
        use site_content::models::BlogPost;
        // One ranked UNION across both models (ORM-built `ts_rank` on Postgres,
        // weighted `LIKE` on SQLite). A backend error (e.g. a test DB without
        // the blog table) degrades to no hits rather than 500-ing the search
        // box. `SearchHit.pk` is the slug for both kinds (see each model's
        // `Searchable::ident`), so it routes straight into the detail URLs.
        match umbral::orm::Search::across::<(PluginModel, BlogPost)>(trimmed, 10).await {
            Ok(found) => {
                // The ranked hits carry slug/title/snippet but not the plugin
                // logo or source label (the template shows both). Batch-load
                // those for the plugin hits in ONE ORM query (slug -> row),
                // OR-chaining `slug.eq(..)` since string columns have no `in_`.
                // The ranked order from `found` is preserved below; no N+1.
                let plugin_slugs: Vec<String> = found
                    .iter()
                    .filter(|h| h.kind == "plugin")
                    .map(|h| h.pk.clone())
                    .collect();
                let mut meta_by_slug: std::collections::HashMap<
                    String,
                    (Option<String>, &'static str),
                > = std::collections::HashMap::new();
                if let Some(pred) = plugin_slugs
                    .iter()
                    .map(|s| plugin::SLUG.eq(s.as_str()))
                    .reduce(|acc, p| acc | p)
                {
                    let rows = PluginModel::objects()
                        .filter(pred)
                        .fetch()
                        .await
                        .map_err(|e| e.to_string())?;
                    for p in rows {
                        meta_by_slug.insert(
                            p.slug,
                            (p.logo.map(|f| f.key().to_string()), source_str(p.source)),
                        );
                    }
                }
                for h in found {
                    match h.kind.as_str() {
                        "plugin" => {
                            let (logo, label) =
                                meta_by_slug.get(&h.pk).cloned().unwrap_or((None, "plugin"));
                            hits.push(SearchHit {
                                kind: "plugin",
                                href: format!("/plugins/{}", h.pk),
                                name: h.title,
                                label: label.to_string(),
                                short_description: h.snippet,
                                logo,
                            });
                        }
                        _ => {
                            hits.push(SearchHit {
                                kind: "blog",
                                href: format!("/blog/{}", h.pk),
                                name: h.title,
                                label: "blog".to_string(),
                                short_description: h.snippet,
                                logo: None,
                            });
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "global search: cross-model search failed; returning no hits"
                );
            }
        }
    }

    umbral::templates::render(
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
    OptionalUser(maybe_user): OptionalUser,
) -> Result<Html<String>, ApiError> {
    // `/plugins/submit` is served by its own static route (registered
    // before this `/plugins/{slug}` matcher), so the submit page never
    // reaches here — this is the canonical plugin-detail path only.
    let submitted = q.submitted.as_deref() == Some("1");
    let viewer = maybe_user.map(|u| u.id);
    match render_detail_for(&slug, submitted, viewer)
        .await
        .map_err(ApiError::internal)?
    {
        Some(html) => Ok(Html(html)),
        None => Err(ApiError::not_found(format!(
            "No plugin directory entry exists for `{slug}` yet."
        ))),
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
    OptionalUser(maybe_user): OptionalUser,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let json = wants_json(&headers);

    let body = form.get("body").map(|s| s.trim()).unwrap_or("");
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "A note body is required.".into()));
    }

    // Posting requires a logged-in account — every note is attributed to a
    // user (no anonymous false reports) and linked for later features (a
    // user's own notes, a contributor leaderboard). The composer is hidden
    // from logged-out visitors in the template; this is the backstop (the
    // AJAX path surfaces this message inline on the form).
    let Some(user) = maybe_user else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Please log in to post a note.".into(),
        ));
    };

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
    // Display name + author FK come from the logged-in user, not a free-text
    // field — names can't be spoofed and the note links back to the account.
    let author = Some((user.id, user.username.clone()));

    // Optional parent: set by the inline reply form's hidden `parent_id`.
    // `create_note` validates it (visible, top-level, same plugin) and rejects
    // anything else, so a forged/blank value is harmless.
    let parent_id = form
        .get("parent_id")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<i64>().ok());

    let Some(payload) = create_note(&slug, body, kind, author, parent_id)
        .await
        .map_err(internal_tuple)?
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
            "parent_id": payload.parent_id,
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
        .get(umbral::web::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("application/json"))
        .unwrap_or(false)
}

/// A JSON 200, optionally carrying the throttle `Set-Cookie`.
fn note_json_response(body: String, cookie: Option<String>) -> Response {
    let mut resp = (
        StatusCode::OK,
        [(umbral::web::header::CONTENT_TYPE, "application/json")],
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
    if let Ok(v) = umbral::web::header::HeaderValue::from_str(cookie) {
        resp.headers_mut()
            .insert(umbral::web::header::SET_COOKIE, v);
    }
}

/// True when enough time has passed since the last `pd_nt` post, or there is
/// no parseable cookie. False only when a recent timestamp is present.
fn note_interval_ok(headers: &HeaderMap) -> bool {
    let last = headers
        .get(umbral::web::header::COOKIE)
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
    author: Option<(i64, String)>,
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
    comment.author = author.as_ref().map(|(id, _)| ForeignKey::new(*id));
    comment.author_label = author.map(|(_, name)| name);

    let created = pd::PluginComment::objects()
        .create(comment)
        .await
        .map_err(|e| e.to_string())?;

    // Render the new row exactly as the page loop would, then ship it.
    let id = created.id;
    let preview = CommentPreview::from_model(created);
    let html = if parent.is_some() {
        render_reply_row(&preview)?
    } else {
        render_comment_row(&preview)?
    };

    // Live note feed (SSE): everyone watching this plugin's thread gets the
    // rendered row and inserts it in place. No-op when RealtimePlugin isn't
    // installed (e.g. the render smoke-test).
    umbral_realtime::Realtime::to_group(format!("public:plugin-{}", plugin.id))
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

async fn report_page(Query(q): Query<ReportQuery>) -> Result<Html<String>, ApiError> {
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
) -> Result<Response, ApiError> {
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
    // This is an ISSUE (a bug / abuse report), not a discussion note — so it
    // lands in the Issues set distinctly from notes and is resolvable by a
    // moderator. A security/malware report stays private to the moderation
    // queue (`is_public = false`); other categories are public so the
    // community can see and corroborate them. `is_resolved` starts false.
    comment.is_issue = true;
    comment.is_public = !matches!(category, "security" | "malware");
    comment.is_resolved = false;

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

    umbral::templates::render(
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

async fn submit_page(Query(q): Query<SubmitQuery>) -> Result<Html<String>, ApiError> {
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
    OptionalUser(maybe_user): OptionalUser,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    // A logged-in submitter OWNS the plugin (`created_by`); an anonymous
    // submission stays unowned (`None`) exactly as before.
    let owner_id = maybe_user.map(|u| u.id);
    match create_submission(&form, owner_id).await {
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
/// `moderation = Pending`), persist it through the ORM, and return the
/// new row's id. `owner_id` is the logged-in submitter's user id: when
/// present the row's `created_by` is set so the submitter OWNS (and can
/// moderate) the plugin; `None` keeps it unowned (an anonymous
/// submission, as before). A UNIQUE clash (name / slug / crate_name
/// already taken) surfaces as a friendly field-keyed [`ValidationErrors`],
/// never a 500. Public so the render smoke-test can drive the validate →
/// create path without an axum runtime.
pub async fn create_submission(
    data: &HashMap<String, String>,
    owner_id: Option<i64>,
) -> Result<i64, ValidationErrors> {
    // `featured` / `display_order` carry a SQL `default` but no
    // `#[umbral(noform)]`, so the Form derive treats them as submittable
    // and required. They're not on the public form (a visitor must not
    // pick their own placement / featured flag), so inject the safe
    // server-side defaults before validation rather than exposing inputs.
    let mut data = data.clone();
    data.entry("featured".to_string())
        .or_insert_with(|| "false".to_string());
    data.entry("display_order".to_string())
        .or_insert_with(|| "0".to_string());

    // The Form derive validates the user-submittable `#[form(...)]`
    // fields and fills the `#[umbral(noform)]` ones from Default. Async
    // because `created_by` (an optional FK) is existence-checked when
    // present — an empty value skips the probe.
    let mut plugin = PluginModel::validate(&data).await?;

    // Server-managed: a public submission is always a pending community
    // row. Ownership comes from the authenticated session, not a
    // spoofable form field — a logged-in submitter owns the plugin (and
    // thus can moderate it); an anonymous submission stays unowned.
    plugin.source = PluginSource::Community;
    plugin.moderation = PluginModeration::Pending;
    plugin.created_by = owner_id.map(ForeignKey::new);

    let created = PluginModel::objects()
        .create(plugin)
        .await
        .map_err(write_to_validation)?;
    Ok(created.id)
}

/// Whether `user_id` OWNS `plugin` (is its `created_by`). Owner-only
/// actions (managing the moderator roster) gate on this, NOT on
/// [`can_moderate`] — only the creator adds or removes moderators, while
/// the moderators they add can act on Notes/Issues but not on the roster
/// itself.
pub fn is_owner(plugin: &PluginModel, user_id: i64) -> bool {
    plugin.created_by.as_ref().map(|fk| fk.id()) == Some(user_id)
}

/// Whether `user_id` may moderate `plugin`'s Notes and Issues.
///
/// Two roles can moderate: the plugin's owner (its `created_by`) and any
/// user with a [`PluginModerator`] grant for the plugin. This is the
/// single authorization check Tasks B/C gate every moderation action
/// behind. Reads go through the ORM — no raw SQL — so the check works on
/// every backend.
pub async fn can_moderate(plugin: &PluginModel, user_id: i64) -> bool {
    // Owner: the submitter who created the plugin moderates implicitly.
    if plugin.created_by.as_ref().map(|fk| fk.id()) == Some(user_id) {
        return true;
    }

    // Granted moderator: a `(plugin, user)` row in the moderator table.
    PluginModerator::objects()
        .filter(plugin_moderator::PLUGIN.eq(plugin.id))
        .filter(plugin_moderator::USER.eq(user_id))
        .exists()
        .await
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Moderation actions (Task B) — moderator roster + Note/Issue moderation
// ---------------------------------------------------------------------------
//
// Two authorization tiers gate these:
//
// * Owner-only (`is_owner`): managing the moderator roster (add / remove).
//   Only the plugin's creator hands out or revokes moderation rights.
// * `can_moderate` (owner OR granted moderator): acting on the content —
//   hiding/unhiding/flagging a Note, resolving/reopening an Issue.
//
// Each handler follows the same shape: resolve `OptionalUser` (401 if
// absent) → load the approved plugin by slug (404 if absent) → authz
// check (403 on failure) → ORM mutation → existing-style Response. The
// row-level work lives in extracted `*_logic` functions so the tests can
// drive the exact same code the routes call (no parallel logic), and
// every read/write goes through the ORM (no raw SQL).

/// Look the (approved) plugin up by slug for a moderation action. `None`
/// is the handler's 404.
async fn load_plugin_for_moderation(slug: &str) -> Result<Option<PluginModel>, String> {
    PluginModel::objects()
        .filter(plugin::SLUG.eq(slug))
        .filter(plugin::MODERATION.eq("approved"))
        .first()
        .await
        .map_err(|e| e.to_string())
}

/// The outcome of an `add moderator` attempt, mapped to a friendly
/// response by the handler. `AlreadyModerator` is the graceful path for a
/// UNIQUE(plugin, user) clash — never a 500.
#[derive(Debug, PartialEq, Eq)]
pub enum AddModeratorOutcome {
    Added,
    AlreadyModerator,
    UserNotFound,
}

/// Grant `target` (resolved from a username or a numeric user id) moderation
/// rights over `plugin`, recording `added_by`. Owner-only — the caller has
/// already verified `is_owner`. Idempotent against the UNIQUE(plugin, user)
/// constraint: a re-add returns [`AddModeratorOutcome::AlreadyModerator`]
/// rather than erroring. Public so the test drives the exact roster-mutation
/// path the route calls.
pub async fn add_moderator_logic(
    plugin: &PluginModel,
    target: &str,
    added_by: i64,
) -> Result<AddModeratorOutcome, String> {
    // Resolve the target user: a bare integer is a user id, anything else a
    // username. Either way the AuthUser must exist.
    let user_id = match target.trim().parse::<i64>() {
        Ok(id) => {
            if AuthUser::objects()
                .filter(umbral_auth::auth_user::ID.eq(id))
                .exists()
                .await
                .map_err(|e| e.to_string())?
            {
                id
            } else {
                return Ok(AddModeratorOutcome::UserNotFound);
            }
        }
        Err(_) => match AuthUser::objects()
            .filter(umbral_auth::auth_user::USERNAME.eq(target.trim()))
            .first()
            .await
            .map_err(|e| e.to_string())?
        {
            Some(u) => u.id,
            None => return Ok(AddModeratorOutcome::UserNotFound),
        },
    };

    // Idempotent: if the grant already exists, report it gracefully instead
    // of tripping the UNIQUE(plugin, user) constraint on insert.
    if PluginModerator::objects()
        .filter(plugin_moderator::PLUGIN.eq(plugin.id))
        .filter(plugin_moderator::USER.eq(user_id))
        .exists()
        .await
        .map_err(|e| e.to_string())?
    {
        return Ok(AddModeratorOutcome::AlreadyModerator);
    }

    let grant = PluginModerator {
        id: 0,
        plugin: ForeignKey::new(plugin.id),
        user: ForeignKey::new(user_id),
        added_by: Some(ForeignKey::new(added_by)),
        created_at: chrono::Utc::now(),
        deleted_at: None,
    };
    match PluginModerator::objects().create(grant).await {
        Ok(_) => Ok(AddModeratorOutcome::Added),
        // A concurrent insert could still race us into the UNIQUE clash —
        // treat that as "already a moderator" rather than a 500.
        Err(_) => Ok(AddModeratorOutcome::AlreadyModerator),
    }
}

/// Revoke `user_id`'s moderation grant over `plugin`. Owner-only (the
/// caller has verified `is_owner`). Idempotent: deleting a grant that
/// isn't there is a no-op `Ok(())`. Returns the number of rows removed.
pub async fn remove_moderator_logic(plugin: &PluginModel, user_id: i64) -> Result<u64, String> {
    PluginModerator::objects()
        .filter(plugin_moderator::PLUGIN.eq(plugin.id))
        .filter(plugin_moderator::USER.eq(user_id))
        .delete()
        .await
        .map_err(|e| e.to_string())
}

/// Apply a moderation `action` (`hide` | `unhide` | `flag`) to the comment
/// `comment_id`, which MUST belong to `plugin`. Sets `moderation` to
/// `Hidden` / `Visible` / `Flagged`. `can_moderate`-gated (the caller has
/// verified it). `Ok(false)` means the comment doesn't exist on this plugin
/// (a 404); an unknown action is a 400 surfaced as `Err`. Public so the test
/// drives the exact mutation path.
pub async fn moderate_comment_logic(
    plugin: &PluginModel,
    comment_id: i64,
    action: &str,
) -> Result<bool, String> {
    let new_state = match action.trim() {
        "hide" => CommentModeration::Hidden,
        "unhide" => CommentModeration::Visible,
        "flag" => CommentModeration::Flagged,
        other => return Err(format!("Unknown moderation action `{other}`.")),
    };

    // The comment must belong to THIS plugin — scope the predicate so a
    // moderator of plugin X can't touch plugin Y's thread.
    let belongs = pd::PluginComment::objects()
        .filter(plugin_comment::ID.eq(comment_id))
        .filter(plugin_comment::PLUGIN.eq(plugin.id))
        .exists()
        .await
        .map_err(|e| e.to_string())?;
    if !belongs {
        return Ok(false);
    }

    // Mirror `moderation` into the `is_public` flag so a hidden/flagged row
    // drops out of the public thread (which filters on `moderation`) and the
    // moderator-visibility flag stays consistent.
    let is_public = matches!(new_state, CommentModeration::Visible);
    let mut values = serde_json::Map::new();
    values.insert(
        "moderation".to_string(),
        serde_json::Value::String(moderation_db_literal(new_state).to_string()),
    );
    values.insert("is_public".to_string(), serde_json::Value::Bool(is_public));

    pd::PluginComment::objects()
        .filter(plugin_comment::ID.eq(comment_id))
        .filter(plugin_comment::PLUGIN.eq(plugin.id))
        .update_values(values)
        .await
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Set `is_resolved` on an issue comment belonging to `plugin`.
/// `can_moderate`-gated (verified by the caller). Only meaningful for
/// `is_issue = true` rows; the column is set regardless but the value is
/// inert on a plain note. `Ok(false)` means the comment isn't on this plugin
/// (a 404). Public so the test drives the exact mutation path.
pub async fn resolve_issue_logic(
    plugin: &PluginModel,
    comment_id: i64,
    resolved: bool,
) -> Result<bool, String> {
    let belongs = pd::PluginComment::objects()
        .filter(plugin_comment::ID.eq(comment_id))
        .filter(plugin_comment::PLUGIN.eq(plugin.id))
        .exists()
        .await
        .map_err(|e| e.to_string())?;
    if !belongs {
        return Ok(false);
    }

    let mut values = serde_json::Map::new();
    values.insert("is_resolved".to_string(), serde_json::Value::Bool(resolved));
    pd::PluginComment::objects()
        .filter(plugin_comment::ID.eq(comment_id))
        .filter(plugin_comment::PLUGIN.eq(plugin.id))
        .update_values(values)
        .await
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// The DB literal (`rename_all = "snake_case"`) for a [`CommentModeration`]
/// variant — the string the `moderation` column stores. Kept next to the
/// logic functions so the `update_values` map writes the same literal the
/// detail query filters on (`moderation = 'visible'` etc.).
fn moderation_db_literal(m: CommentModeration) -> &'static str {
    match m {
        CommentModeration::Pending => "pending",
        CommentModeration::Visible => "visible",
        CommentModeration::Hidden => "hidden",
        CommentModeration::Flagged => "flagged",
        CommentModeration::Deleted => "deleted",
        CommentModeration::Locked => "locked",
    }
}

/// `POST /plugins/{slug}/moderators` — owner-only. Add a user (by
/// `username` or `user_id` form field) to the plugin's moderator roster.
async fn post_add_moderator(
    Path(slug): Path<String>,
    OptionalUser(maybe_user): OptionalUser,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let Some(user) = maybe_user else {
        return Err((StatusCode::UNAUTHORIZED, "Please log in.".into()));
    };
    let Some(plugin) = load_plugin_for_moderation(&slug)
        .await
        .map_err(internal_tuple)?
    else {
        return Err((StatusCode::NOT_FOUND, format!("No plugin `{slug}`.")));
    };
    if !is_owner(&plugin, user.id) {
        return Err((
            StatusCode::FORBIDDEN,
            "Only the plugin owner can manage moderators.".into(),
        ));
    }

    let target = form
        .get("username")
        .or_else(|| form.get("user_id"))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let Some(target) = target else {
        return Err((
            StatusCode::BAD_REQUEST,
            "A username or user_id is required.".into(),
        ));
    };

    match add_moderator_logic(&plugin, target, user.id)
        .await
        .map_err(internal_tuple)?
    {
        AddModeratorOutcome::Added => Ok(Redirect::to(&format!("/plugins/{slug}")).into_response()),
        AddModeratorOutcome::AlreadyModerator => Ok((
            StatusCode::OK,
            format!("`{target}` is already a moderator of this plugin."),
        )
            .into_response()),
        AddModeratorOutcome::UserNotFound => Err((
            StatusCode::NOT_FOUND,
            format!("No user matches `{target}`."),
        )),
    }
}

/// `POST /plugins/{slug}/moderators/{user_id}/remove` — owner-only.
/// Idempotent revoke of a moderator grant.
async fn post_remove_moderator(
    Path((slug, user_id)): Path<(String, i64)>,
    OptionalUser(maybe_user): OptionalUser,
) -> Result<Response, (StatusCode, String)> {
    let Some(user) = maybe_user else {
        return Err((StatusCode::UNAUTHORIZED, "Please log in.".into()));
    };
    let Some(plugin) = load_plugin_for_moderation(&slug)
        .await
        .map_err(internal_tuple)?
    else {
        return Err((StatusCode::NOT_FOUND, format!("No plugin `{slug}`.")));
    };
    if !is_owner(&plugin, user.id) {
        return Err((
            StatusCode::FORBIDDEN,
            "Only the plugin owner can manage moderators.".into(),
        ));
    }

    remove_moderator_logic(&plugin, user_id)
        .await
        .map_err(internal_tuple)?;
    Ok(Redirect::to(&format!("/plugins/{slug}")).into_response())
}

/// `POST /plugins/{slug}/comments/{comment_id}/moderate` —
/// `can_moderate`-gated. Body: `action = hide | unhide | flag`.
async fn post_moderate_comment(
    Path((slug, comment_id)): Path<(String, i64)>,
    OptionalUser(maybe_user): OptionalUser,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    let Some(user) = maybe_user else {
        return Err((StatusCode::UNAUTHORIZED, "Please log in.".into()));
    };
    let Some(plugin) = load_plugin_for_moderation(&slug)
        .await
        .map_err(internal_tuple)?
    else {
        return Err((StatusCode::NOT_FOUND, format!("No plugin `{slug}`.")));
    };
    if !can_moderate(&plugin, user.id).await {
        return Err((
            StatusCode::FORBIDDEN,
            "You don't have moderation rights on this plugin.".into(),
        ));
    }

    let action = form.get("action").map(|s| s.trim()).unwrap_or("");
    match moderate_comment_logic(&plugin, comment_id, action).await {
        Ok(true) => Ok(Redirect::to(&redirect_after_moderation(&form, &slug)).into_response()),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            "That comment isn't on this plugin.".into(),
        )),
        Err(msg) => Err((StatusCode::BAD_REQUEST, msg)),
    }
}

/// `POST /plugins/{slug}/issues/{comment_id}/resolve` —
/// `can_moderate`-gated. Marks an issue resolved.
async fn post_resolve_issue(
    Path((slug, comment_id)): Path<(String, i64)>,
    OptionalUser(maybe_user): OptionalUser,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    moderate_issue_resolution(slug, comment_id, maybe_user, true, &form).await
}

/// `POST /plugins/{slug}/issues/{comment_id}/reopen` —
/// `can_moderate`-gated. Re-opens a resolved issue.
async fn post_reopen_issue(
    Path((slug, comment_id)): Path<(String, i64)>,
    OptionalUser(maybe_user): OptionalUser,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    moderate_issue_resolution(slug, comment_id, maybe_user, false, &form).await
}

/// Shared body for the resolve / reopen handlers (same authz + lookup, only
/// the boolean differs). `form` carries an optional `next` local path so the
/// account-area pages return to `/account/plugins/{slug}` instead of the
/// public detail page (the HTMX detail-page forms send no `next`, so they
/// keep redirecting to `/plugins/{slug}`).
async fn moderate_issue_resolution(
    slug: String,
    comment_id: i64,
    maybe_user: Option<AuthUser>,
    resolved: bool,
    form: &HashMap<String, String>,
) -> Result<Response, (StatusCode, String)> {
    let Some(user) = maybe_user else {
        return Err((StatusCode::UNAUTHORIZED, "Please log in.".into()));
    };
    let Some(plugin) = load_plugin_for_moderation(&slug)
        .await
        .map_err(internal_tuple)?
    else {
        return Err((StatusCode::NOT_FOUND, format!("No plugin `{slug}`.")));
    };
    if !can_moderate(&plugin, user.id).await {
        return Err((
            StatusCode::FORBIDDEN,
            "You don't have moderation rights on this plugin.".into(),
        ));
    }

    match resolve_issue_logic(&plugin, comment_id, resolved)
        .await
        .map_err(internal_tuple)?
    {
        true => Ok(Redirect::to(&redirect_after_moderation(form, &slug)).into_response()),
        false => Err((
            StatusCode::NOT_FOUND,
            "That issue isn't on this plugin.".into(),
        )),
    }
}

/// Where a moderation action redirects on success. An account-area form sends
/// a `next` field (`/account/plugins/{slug}`); the public detail page's HTMX
/// forms don't, so they fall back to `/plugins/{slug}`. `next` is accepted only
/// when it's a same-site absolute path (leading `/`, not `//`) — an open-
/// redirect guard, mirroring the accounts plugin's `safe_next`.
fn redirect_after_moderation(form: &HashMap<String, String>, slug: &str) -> String {
    form.get("next")
        .map(|s| s.trim())
        .filter(|s| s.starts_with('/') && !s.starts_with("//"))
        .map(str::to_string)
        .unwrap_or_else(|| format!("/plugins/{slug}"))
}

// ---------------------------------------------------------------------------
// Self-service moderation account area (`/account/plugins`)
// ---------------------------------------------------------------------------
//
// A private home for a plugin author: the plugins they own (`created_by`) or
// were granted moderation on (`PluginModerator`), and per plugin the issues +
// notes they can act on. The action buttons POST to the EXISTING
// `/plugins/{slug}/...` moderation endpoints (with a `next` field pointing
// back here), so this area adds aggregation + authz-scoped listing, never new
// mutation paths. Both pages resolve the caller with `OptionalUser` and gate
// on the same `is_owner` / `can_moderate` checks the POST handlers use.

/// One plugin row on the `/account/plugins` overview.
#[derive(Serialize)]
struct AccountPluginRow {
    slug: String,
    name: String,
    icon: String,
    role: &'static str,
    status: String,
    moderation: String,
    open_issues: i64,
    hidden: i64,
    pending_notes: i64,
}

/// Count the moderation-relevant comments for one plugin. Soft-deleted rows are
/// excluded automatically (the model is `soft_delete`, so `objects()` filters
/// `deleted_at IS NULL`).
async fn account_plugin_counts(plugin_id: i64) -> Result<(i64, i64, i64), String> {
    let open_issues = pd::PluginComment::objects()
        .filter(plugin_comment::PLUGIN.eq(plugin_id))
        .filter(plugin_comment::IS_ISSUE.eq(true))
        .filter(plugin_comment::IS_RESOLVED.eq(false))
        .count()
        .await
        .map_err(|e| e.to_string())? as i64;
    let hidden = pd::PluginComment::objects()
        .filter(plugin_comment::PLUGIN.eq(plugin_id))
        .filter(plugin_comment::MODERATION.eq("hidden"))
        .count()
        .await
        .map_err(|e| e.to_string())? as i64;
    let pending_notes = pd::PluginComment::objects()
        .filter(plugin_comment::PLUGIN.eq(plugin_id))
        .filter(plugin_comment::MODERATION.eq("pending"))
        .count()
        .await
        .map_err(|e| e.to_string())? as i64;
    Ok((open_issues, hidden, pending_notes))
}

/// `GET /account/plugins` — the plugins the signed-in user owns or moderates,
/// each with the counts that tell them whether anything needs attention.
async fn account_plugins_page(
    OptionalUser(maybe_user): OptionalUser,
) -> Result<Response, ApiError> {
    let Some(user) = maybe_user else {
        return Ok(Redirect::to("/login?next=/account/plugins").into_response());
    };

    // Owned (created_by == the caller) and moderated (a PluginModerator grant)
    // sets. A user can be both; owner wins for the role label, and we never
    // list a plugin twice.
    let owned = PluginModel::objects()
        .filter(plugin::CREATED_BY.eq(user.id))
        .fetch()
        .await
        .map_err(internal_error)?;
    let owned_ids: std::collections::HashSet<i64> = owned.iter().map(|p| p.id).collect();

    let grants = PluginModerator::objects()
        .filter(plugin_moderator::USER.eq(user.id))
        .fetch()
        .await
        .map_err(internal_error)?;

    let mut rows: Vec<AccountPluginRow> = Vec::new();
    for p in &owned {
        rows.push(account_row(p, "Owner").await.map_err(internal_error)?);
    }
    for g in &grants {
        let pid = g.plugin.id();
        if owned_ids.contains(&pid) {
            continue;
        }
        if let Some(p) = PluginModel::objects()
            .filter(plugin::ID.eq(pid))
            .first()
            .await
            .map_err(internal_error)?
        {
            rows.push(account_row(&p, "Moderator").await.map_err(internal_error)?);
        }
    }
    // Stable, friendly order: needs-attention first (open issues desc), then
    // name. Sorting in Rust keeps the query simple and the set is small.
    rows.sort_by(|a, b| {
        b.open_issues
            .cmp(&a.open_issues)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let body = umbral::templates::render("account/plugins.html", &context! { rows => rows })
        .map_err(internal_error)?;
    Ok(Html(body).into_response())
}

/// Build one overview row (identity + counts) for a plugin the caller manages.
async fn account_row(p: &PluginModel, role: &'static str) -> Result<AccountPluginRow, String> {
    let (open_issues, hidden, pending_notes) = account_plugin_counts(p.id).await?;
    Ok(AccountPluginRow {
        slug: p.slug.clone(),
        name: p.name.clone(),
        icon: initials(&p.name),
        role,
        status: title_case(&format!("{:?}", p.status)),
        moderation: title_case(&format!("{:?}", p.moderation)),
        open_issues,
        hidden,
        pending_notes,
    })
}

/// One issue/note row on the per-plugin manage page.
#[derive(Serialize)]
struct ManageComment {
    id: i64,
    body: String,
    kind: String,
    author: String,
    created: String,
    is_resolved: bool,
    moderation: String,
    hidden: bool,
    pending: bool,
}

impl ManageComment {
    fn from_row(c: &pd::PluginComment) -> Self {
        let moderation = moderation_db_literal(c.moderation).to_string();
        ManageComment {
            id: c.id,
            body: c.body.clone(),
            kind: title_case(&format!("{:?}", c.kind)),
            // Prefer the curated self-identification label; a plain visitor
            // note carries neither an author nor a label — show "Anonymous"
            // rather than issuing an N+1 user lookup per row.
            author: c
                .author_label
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "Anonymous".to_string()),
            created: c.created_at.format("%b %-d, %Y").to_string(),
            is_resolved: c.is_resolved,
            hidden: moderation == "hidden" || moderation == "flagged",
            pending: moderation == "pending",
            moderation,
        }
    }
}

/// The `/account/plugins/{slug}` manage view.
#[derive(Serialize)]
struct ManageView {
    slug: String,
    name: String,
    icon: String,
    role: &'static str,
    next: String,
    detail_url: String,
    open_issues: Vec<ManageComment>,
    resolved_issues: Vec<ManageComment>,
    notes: Vec<ManageComment>,
}

/// `GET /account/plugins/{slug}` — manage one plugin's issues + notes. Gated to
/// its owner or a granted moderator (403 otherwise); reuses the same
/// `is_owner` / `can_moderate` checks the POST endpoints enforce.
async fn account_plugin_manage_page(
    Path(slug): Path<String>,
    OptionalUser(maybe_user): OptionalUser,
) -> Result<Response, (StatusCode, String)> {
    let Some(user) = maybe_user else {
        return Ok(Redirect::to(&format!("/login?next=/account/plugins/{slug}")).into_response());
    };
    let Some(plugin) = load_plugin_for_moderation(&slug)
        .await
        .map_err(internal_tuple)?
    else {
        return Err((StatusCode::NOT_FOUND, format!("No plugin `{slug}`.")));
    };
    let role = if is_owner(&plugin, user.id) {
        "Owner"
    } else if can_moderate(&plugin, user.id).await {
        "Moderator"
    } else {
        return Err((
            StatusCode::FORBIDDEN,
            "You don't own or moderate this plugin.".into(),
        ));
    };

    // Every non-deleted comment for the plugin, oldest first, partitioned into
    // open issues / resolved issues / notes.
    let comments = pd::PluginComment::objects()
        .filter(plugin_comment::PLUGIN.eq(plugin.id))
        .order_by(plugin_comment::CREATED_AT.desc())
        .fetch()
        .await
        .map_err(internal_tuple)?;

    let mut open_issues = Vec::new();
    let mut resolved_issues = Vec::new();
    let mut notes = Vec::new();
    for c in &comments {
        let row = ManageComment::from_row(c);
        if c.is_issue {
            if c.is_resolved {
                resolved_issues.push(row);
            } else {
                open_issues.push(row);
            }
        } else {
            notes.push(row);
        }
    }

    let view = ManageView {
        slug: plugin.slug.clone(),
        name: plugin.name.clone(),
        icon: initials(&plugin.name),
        role,
        next: format!("/account/plugins/{}", plugin.slug),
        detail_url: format!("/plugins/{}", plugin.slug),
        open_issues,
        resolved_issues,
        notes,
    };

    let body =
        umbral::templates::render("account/plugin_manage.html", &context! { plugin => view })
            .map_err(internal_tuple)?;
    Ok(Html(body).into_response())
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

    umbral::templates::render(
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
fn write_to_validation(e: umbral::orm::write::WriteError) -> ValidationErrors {
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
    render_detail_for(slug, false, None).await
}

/// `render_detail`, with the `?submitted=1` success-banner flag threaded
/// through to the template. The viewer is anonymous (no moderation UI).
pub async fn render_detail_with(slug: &str, submitted: bool) -> Result<Option<String>, String> {
    render_detail_for(slug, submitted, None).await
}

/// `render_detail_with`, with the current viewer's user id threaded through
/// so the moderation UI (the moderator roster, per-note actions, issue
/// resolve/reopen) renders only for an owner / moderator. `viewer = None`
/// is an anonymous (or logged-out) visitor — none of the controls render.
pub async fn render_detail_for(
    slug: &str,
    submitted: bool,
    viewer: Option<i64>,
) -> Result<Option<String>, String> {
    let Some(plugin) = PluginModel::objects()
        .filter(plugin::SLUG.eq(slug))
        .filter(plugin::MODERATION.eq("approved"))
        .first()
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };

    // Authorization for the moderation UI: the owner gets the moderator
    // roster (add/remove); the owner OR a granted moderator gets the
    // per-note actions + issue resolve/reopen. Both compute through the
    // Task A/B fns (ORM-only, every backend). An anonymous viewer is
    // neither, so nothing renders.
    let (is_owner_view, can_moderate_view) = match viewer {
        Some(uid) => (is_owner(&plugin, uid), can_moderate(&plugin, uid).await),
        None => (false, false),
    };

    // The moderator roster (owner-only UI): every `PluginModerator` grant
    // for this plugin joined to the granted user's `AuthUser` for the
    // username. Loaded only when the owner is viewing (the roster is hidden
    // otherwise) so a stranger's render does no extra work. One grants
    // query + one batched user lookup — no N+1.
    let mut moderators: Vec<ModeratorRow> = Vec::new();
    if is_owner_view {
        let grants = PluginModerator::objects()
            .filter(plugin_moderator::PLUGIN.eq(plugin.id))
            .order_by(plugin_moderator::CREATED_AT.asc())
            .fetch()
            .await
            .map_err(|e| e.to_string())?;
        if !grants.is_empty() {
            let user_ids: Vec<i64> = grants.iter().map(|g| g.user.id()).collect();
            let mut name_by_id: std::collections::HashMap<i64, String> =
                std::collections::HashMap::new();
            let users = AuthUser::objects()
                .filter(umbral_auth::auth_user::ID.in_(&user_ids))
                .fetch()
                .await
                .map_err(|e| e.to_string())?;
            for u in users {
                name_by_id.insert(u.id, u.username);
            }
            for g in grants {
                let uid = g.user.id();
                moderators.push(ModeratorRow {
                    user_id: uid,
                    username: name_by_id
                        .get(&uid)
                        .cloned()
                        .unwrap_or_else(|| format!("user #{uid}")),
                    added: g.created_at.format("%b %-d, %Y").to_string(),
                });
            }
        }
    }

    // Issues (`is_issue = true`) are a distinct set from discussion notes:
    // bug/abuse reports a moderator resolves. The public set is the public,
    // non-issue-private rows (`is_public = true`); a moderator additionally
    // sees the private (security/malware) reports. Each carries its
    // resolved/open status for the status badge.
    let issue_rows = {
        let mut q = plugin
            .reverse::<pd::PluginComment>()
            .map_err(|e| e.to_string())?
            .filter(plugin_comment::IS_ISSUE.eq(true));
        if !can_moderate_view {
            // Non-moderators only see public issues (security/malware reports
            // stay private to the moderation queue).
            q = q.filter(plugin_comment::IS_PUBLIC.eq(true));
        }
        q.order_by(plugin_comment::IS_RESOLVED.asc())
            .order_by(plugin_comment::CREATED_AT.desc())
            .limit(50)
            .fetch()
            .await
            .map_err(|e| e.to_string())?
    };
    let issues: Vec<IssueRow> = issue_rows.into_iter().map(IssueRow::from_model).collect();

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

    // True total of visible comments (notes + replies) for the "N notes" stat
    // — counted DB-side, not by loading every row into memory.
    let notes_total = plugin
        .reverse::<pd::PluginComment>()
        .map_err(|e| e.to_string())?
        .filter(plugin_comment::MODERATION.eq("visible"))
        .count()
        .await
        .map_err(|e| e.to_string())?;

    // Top-level notes (parent IS NULL), pinned first then chronological, capped
    // at 10. The IS NULL predicate and the LIMIT are both pushed to the query
    // layer — the cap is enforced by the DB, not by truncating an in-memory Vec.
    // A moderator additionally sees hidden / flagged notes (so they can
    // unhide them inline); the public view stays `visible`-only. Discussion
    // notes are non-issues, so issues never leak into the thread.
    let top_level = {
        let q = plugin
            .reverse::<pd::PluginComment>()
            .map_err(|e| e.to_string())?
            .filter(plugin_comment::IS_ISSUE.eq(false))
            .filter(plugin_comment::PARENT.is_null());
        let q = if can_moderate_view {
            q.filter(
                plugin_comment::MODERATION.eq("visible")
                    | plugin_comment::MODERATION.eq("hidden")
                    | plugin_comment::MODERATION.eq("flagged"),
            )
        } else {
            q.filter(plugin_comment::MODERATION.eq("visible"))
        };
        q.order_by(plugin_comment::PINNED.desc())
            .order_by(plugin_comment::CREATED_AT.asc())
            .limit(10)
            .fetch()
            .await
            .map_err(|e| e.to_string())?
    };

    // Replies for exactly those notes, fetched in one IN query (FK column
    // `parent` IN [note ids]) and grouped by parent id. No N+1; an empty note
    // list skips the query entirely.
    let note_ids: Vec<i64> = top_level.iter().map(|n| n.id).collect();
    let mut replies_by_parent: std::collections::HashMap<i64, Vec<pd::PluginComment>> =
        std::collections::HashMap::new();
    if !note_ids.is_empty() {
        let replies = pd::PluginComment::objects()
            .filter(plugin_comment::PARENT.in_(&note_ids))
            .filter(plugin_comment::MODERATION.eq("visible"))
            .order_by(plugin_comment::CREATED_AT.asc())
            .fetch()
            .await
            .map_err(|e| e.to_string())?;
        for r in replies {
            if let Some(pid) = r.parent.as_ref().map(|fk| fk.id()) {
                replies_by_parent.entry(pid).or_default().push(r);
            }
        }
    }

    let comment_rows: Vec<CommentThread> = top_level
        .into_iter()
        .map(|note| {
            let replies = replies_by_parent
                .remove(&note.id)
                .unwrap_or_default()
                .into_iter()
                .map(CommentPreview::from_model)
                .collect();
            CommentThread {
                note: CommentPreview::from_model(note),
                replies,
            }
        })
        .collect();

    let detail = PluginDetail::build(plugin, feature_rows, compat_rows, comment_rows, notes_total);
    umbral::templates::render(
        "plugin_directory/plugin.html",
        &context!(
            plugin => detail,
            submitted => submitted,
            is_owner => is_owner_view,
            can_moderate => can_moderate_view,
            moderators => moderators,
            issues => issues,
        ),
    )
    .map(Some)
    .map_err(|e| e.to_string())
}

/// One moderator-roster row in the owner-only management section: the
/// granted user's id (the remove-button target), their username and when
/// the grant was made.
#[derive(Debug, Serialize)]
struct ModeratorRow {
    user_id: i64,
    username: String,
    added: String,
}

/// One issue (a bug / abuse report, `is_issue = true`) in the Issues tab:
/// the comment id (resolve/reopen target), the report body, its date, the
/// reporter label, whether it's resolved, and a moderator-only privacy
/// flag so the template can mark the private (security/malware) reports.
#[derive(Debug, Serialize)]
struct IssueRow {
    id: i64,
    body: String,
    created: String,
    reporter: String,
    resolved: bool,
    /// `false` for the security/malware reports that stay private to the
    /// moderation queue — surfaced so a moderator sees the "private" mark.
    public: bool,
}

impl IssueRow {
    fn from_model(c: pd::PluginComment) -> Self {
        let reporter = c
            .author_label
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "Issue report".to_string());
        Self {
            id: c.id,
            created: c.created_at.format("%b %-d, %Y").to_string(),
            reporter,
            resolved: c.is_resolved,
            public: c.is_public,
            body: c.body,
        }
    }
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
    /// The canonical `umbral add <crate>` line (always present, copyable).
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
    comments: Vec<CommentThread>,
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
/// the Umbral version, backend chips, MSRV, and verified-vs-declared.
#[derive(Debug, Serialize)]
struct CompatRow {
    umbral_version: String,
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
        comments: Vec<CommentThread>,
        notes_total: i64,
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
                    c.umbral_version.clone()
                } else {
                    format!("{} · {}", c.umbral_version, backends)
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
                    umbral_version: c.umbral_version,
                    backends,
                    minimum_rust_version: c.minimum_rust_version,
                    notes: c.notes,
                    verified: c.verified_at.is_some(),
                    verified_at: c.verified_at.map(|d| d.format("%b %-d, %Y").to_string()),
                }
            })
            .collect();

        let comment_previews = comments;

        let install = install_line(&p.installation_commands, &p.crate_name);
        let add_line = format!("umbral add {}", p.crate_name);
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
            notes: notes_total,
            tags: derive_tags(&p),
            links,
            features: feature_rows,
            shipped_features,
            total_features,
            progress_pct,
            usage_title: "Usage".to_string(),
            usage_intro: p.setup_notes.clone().unwrap_or_else(|| {
                "Add the plugin to your project's plugin list and wire it in \
                     `main.rs` like every other Umbral battery."
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
    /// Moderation state literal ("visible" / "hidden" / "flagged" / …) so a
    /// moderator sees the current status badge + the right action buttons.
    moderation: &'static str,
    /// True when this note is currently hidden (drives the "Hidden" badge +
    /// the Unhide action).
    is_hidden: bool,
    /// True when this note is currently flagged.
    is_flagged: bool,
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
        let moderation = moderation_db_literal(c.moderation);
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
            moderation,
            is_hidden: matches!(c.moderation, CommentModeration::Hidden),
            is_flagged: matches!(c.moderation, CommentModeration::Flagged),
        }
    }
}

/// One top-level note plus its (depth-1) replies, in render order.
#[derive(Debug, Serialize)]
struct CommentThread {
    note: CommentPreview,
    replies: Vec<CommentPreview>,
}

/// Render a single comment row to HTML using the same partial the page
/// loop includes, so a live-inserted note is byte-identical to a reloaded
/// one. The body is sanitized by the `markdown` filter (ammonia), so the
/// returned HTML is safe to broadcast and insert via `innerHTML`.
fn render_comment_row(preview: &CommentPreview) -> Result<String, String> {
    umbral::templates::render(
        "plugin_directory/_comment.html",
        &umbral::templates::context! { comment => preview },
    )
    .map_err(|e| e.to_string())
}

/// Render one reply row via the slim `_reply.html`, the reply counterpart to
/// [`render_comment_row`] — so a live-inserted reply is byte-identical to a
/// reloaded one. Body sanitized by `| markdown`, safe to broadcast.
fn render_reply_row(preview: &CommentPreview) -> Result<String, String> {
    umbral::templates::render(
        "plugin_directory/_reply.html",
        &umbral::templates::context! { comment => preview },
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
        AuditStatus::UmbralReviewed => ("ok", "Audited"),
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
/// `umbral add <crate>` default.
fn install_line(commands: &str, crate_name: &str) -> String {
    commands
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("umbral add {crate_name}"))
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

/// Two-character uppercase initials from a name ("Umbral REST" → "UR",
/// "umbral-rest" → "UR", "rest" → "RE").
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

/// Opaque 500 as a `(StatusCode, String)` tuple, for the handlers that ALSO return 401 /
/// 429 and therefore cannot use `ApiError` — core's `ApiError` has no variant for either
/// (gaps3 #62).
///
/// The point is the same as `internal_error`'s: the cause is LOGGED, never sent. What
/// used to sit here was a helper returning the error's own text, which put the database's
/// error message on the page.
fn internal_tuple<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    tracing::error!(error = %err, "plugin_directory: internal error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal server error".to_string(),
    )
}

fn internal_error<E: std::fmt::Display>(err: E) -> ApiError {
    // gaps3 #58: this used to be `(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())`,
    // which sent the database's own error text to the browser — a missing table, a column
    // name, a SQL fragment, shown to whoever asked for the page. `ApiError::internal` logs
    // the cause server-side and returns an opaque 500.
    ApiError::internal(err.to_string())
}

trait AuditDateLabel {
    fn ne_or_label(&self) -> &'static str;
}

impl AuditDateLabel for AuditStatus {
    fn ne_or_label(&self) -> &'static str {
        match self {
            AuditStatus::UmbralReviewed => "Umbral team",
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
        assert_eq!(initials("Umbral REST"), "UR");
        assert_eq!(initials("umbral-rest"), "UR");
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
            install_line("\n  umbral add umbral-rest\nmore", "umbral-rest"),
            "umbral add umbral-rest"
        );
        assert_eq!(install_line("   \n  ", "umbral-x"), "umbral add umbral-x");
    }

    #[test]
    fn backend_summary_normalizes_known_backends() {
        let v = serde_json::json!(["postgres", "sqlite", "mysql"]);
        assert_eq!(backend_summary(&v), "PostgreSQL, SQLite, MySQL");
        assert_eq!(backend_summary(&serde_json::json!("notarray")), "");
    }

    #[test]
    fn audit_badge_kinds() {
        assert_eq!(audit_badge(AuditStatus::UmbralReviewed).0, "ok");
        assert_eq!(audit_badge(AuditStatus::NotReviewed).0, "warn");
    }
}
