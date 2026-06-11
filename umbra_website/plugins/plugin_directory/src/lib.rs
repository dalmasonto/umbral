//! PluginDirectoryPlugin — owns the plugin directory for umbra.dev.
//!
//! Wire this into your App by adding to `src/main.rs`:
//!
//! ```ignore
//! .plugin(plugin_directory::PluginDirectoryPlugin::default())
//! ```
//!
//! Declare models, routes, and `on_ready` work in the impl below.
//! See `documentation/docs/v0.0.1/plugins/the-plugin-trait.mdx` for
//! what each method does.

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
use umbra::routes::RouteSpec;
use umbra::templates::context;
use umbra::web::{Html, Path, Router, StatusCode, get};

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
                Ok(n) => tracing::info!(
                    "{}: seeded {} official plugin rows",
                    plugin_name,
                    n
                ),
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

async fn plugin_directory() -> Result<Html<String>, (StatusCode, String)> {
    render("plugin_directory/plugins.html", &serde_json::json!({}))
}

async fn plugin_detail(
    Path(slug): Path<String>,
) -> Result<Html<String>, (StatusCode, String)> {
    if slug == "submit" {
        return render("plugin_directory/submit.html", &serde_json::json!({}));
    }

    let Some(plugin) = plugin_by_slug(&slug) else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("No plugin directory entry exists for `{slug}` yet."),
        ));
    };

    render("plugin_directory/plugin.html", &context!(plugin))
}

fn render<C: Serialize>(
    template: &str,
    context: &C,
) -> Result<Html<String>, (StatusCode, String)> {
    umbra::templates::render(template, context)
        .map(Html)
        .map_err(internal_error)
}

fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

#[derive(Debug, Clone, Serialize)]
struct PluginDetail {
    slug: &'static str,
    name: &'static str,
    source: &'static str,
    maintainer: &'static str,
    description: &'static str,
    icon: &'static str,
    featured: bool,
    flagged: bool,
    status: &'static str,
    status_kind: &'static str,
    maturity: &'static str,
    channel: &'static str,
    install: &'static str,
    toml: &'static str,
    rating: &'static str,
    installs: &'static str,
    notes: &'static str,
    tags: Vec<&'static str>,
    features: Vec<StatusRow>,
    shipped_features: usize,
    total_features: usize,
    usage_title: &'static str,
    usage_intro: &'static str,
    usage_code: &'static str,
    audit_label: &'static str,
    audit_date: &'static str,
    audit_kind: &'static str,
    compatibility: Vec<StatusRow>,
    comments: Vec<CommentPreview>,
}

#[derive(Debug, Clone, Serialize)]
struct StatusRow {
    label: &'static str,
    status: &'static str,
    kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct CommentPreview {
    initials: &'static str,
    name: &'static str,
    role: &'static str,
    badge: &'static str,
    badge_kind: &'static str,
    body: &'static str,
}

fn plugin_by_slug(slug: &str) -> Option<PluginDetail> {
    match slug {
        "umbra-rest" => Some(PluginDetail {
            slug: "umbra-rest",
            name: "umbra-rest",
            source: "official",
            maintainer: "Umbra team",
            description: "Build REST APIs the familiar way: serializers, viewsets, routers, pagination, filtering, OpenAPI schemas and an in-browser request playground.",
            icon: "{}",
            featured: true,
            flagged: false,
            status: "Shipped",
            status_kind: "ok",
            maturity: "Stable",
            channel: "Stable channel",
            install: "umbra add umbra-rest",
            toml: "plugins += [\"umbra.rest\"]",
            rating: "4.9",
            installs: "38k",
            notes: "52",
            tags: vec!["api", "openapi", "serializers", "viewsets", "playground"],
            features: vec![
                shipped("Serializers"),
                shipped("Viewsets and routers"),
                shipped("Pagination"),
                shipped("Filtering"),
                shipped("OpenAPI schema"),
                shipped("Request playground"),
                beta("Generated SDK examples"),
            ],
            shipped_features: 6,
            total_features: 7,
            usage_title: "Usage",
            usage_intro: "The contract exposes serializers, routers and schema generation through the same plugin lifecycle hooks as the rest of Umbra.",
            usage_code: "[plugins]\nenabled = [\"umbra.orm\", \"umbra.rest\"]\n\n[plugins.umbra.rest]\nopenapi = true\nplayground = true\npagination = \"cursor\"",
            audit_label: "Audited",
            audit_date: "Aug 2026",
            audit_kind: "ok",
            compatibility: vec![
                shipped("Umbra v0.1.x"),
                shipped("PostgreSQL"),
                partial("SQLite"),
                planned("MySQL"),
            ],
            comments: vec![
                CommentPreview {
                    initials: "AM",
                    name: "Amina M.",
                    role: "API lead / verified project",
                    badge: "works in production",
                    badge_kind: "ok",
                    body: "The generated schema was the fastest route to getting our internal docs aligned with implementation. Cursor pagination needed almost no glue.",
                },
                CommentPreview {
                    initials: "KL",
                    name: "Kai L.",
                    role: "solo builder",
                    badge: "watch filtering",
                    badge_kind: "warn",
                    body: "Filtering is solid, but document the custom lookup syntax early. I only found it after reading examples.",
                },
            ],
        }),
        "umbra-admin" => Some(simple_official_plugin(
            "umbra-admin",
            "umbra-admin",
            "Dashboards, CRUD, filters, sheets, bulk actions and per-user preferences generated from your models.",
            "ad",
            "umbra add umbra-admin",
            "plugins += [\"umbra.admin\"]",
            vec!["admin", "crud", "moderation", "dashboard"],
        )),
        "umbra-auth" => Some(simple_official_plugin(
            "umbra-auth",
            "umbra-auth",
            "Sessions, password hashing, permissions and groups, replaceable by OAuth or SSO plugins.",
            "au",
            "umbra add umbra-auth",
            "plugins += [\"umbra.auth\"]",
            vec!["auth", "sessions", "permissions"],
        )),
        "umbra-sessions" => Some(simple_official_plugin(
            "umbra-sessions",
            "umbra-sessions",
            "Cookie-backed session storage and request helpers for authenticated Umbra applications.",
            "ss",
            "umbra add umbra-sessions",
            "plugins += [\"umbra.sessions\"]",
            vec!["sessions", "cookies", "auth"],
        )),
        "umbra-orm" => Some(simple_official_plugin(
            "umbra-orm",
            "umbra-orm",
            "Model declaration, query ergonomics and the database-facing layer every higher-level Umbra plugin builds on.",
            "db",
            "umbra add umbra-orm",
            "plugins += [\"umbra.orm\"]",
            vec!["orm", "models", "querysets", "database"],
        )),
        "umbra-migrations" => Some(simple_official_plugin(
            "umbra-migrations",
            "umbra-migrations",
            "Schema diffs, reviewable migration files, migrate, rollback direction and applied-state tracking.",
            "mg",
            "umbra add umbra-migrations",
            "plugins += [\"umbra.migrations\"]",
            vec!["migrations", "schema", "database"],
        )),
        "umbra-forms" => Some(simple_official_plugin(
            "umbra-forms",
            "umbra-forms",
            "Validation, friendly errors, spam resistance and server-rendered form handling.",
            "fm",
            "umbra add umbra-forms",
            "plugins += [\"umbra.forms\"]",
            vec!["forms", "validation", "csrf"],
        )),
        "umbra-content" => Some(simple_official_plugin(
            "umbra-content",
            "umbra-content",
            "Blog posts, pages, FAQ, media references, redirects, banners and testimonials.",
            "ct",
            "umbra add umbra-content",
            "plugins += [\"umbra.content\"]",
            vec!["content", "cms", "pages", "blog"],
        )),
        "umbra-tasks" => Some(simple_experimental_plugin(
            "umbra-tasks",
            "umbra-tasks",
            "Background jobs, scheduling and retries behind the same plugin lifecycle as the request stack.",
            "tk",
            "umbra add umbra-tasks",
            "plugins += [\"umbra.tasks\"]",
            vec!["tasks", "jobs", "scheduler"],
        )),
        "umbra-openapi" => Some(simple_official_plugin(
            "umbra-openapi",
            "umbra-openapi",
            "OpenAPI generation for plugin-contributed API routes, including model metadata and schema hints.",
            "oa",
            "umbra add umbra-openapi",
            "plugins += [\"umbra.openapi\"]",
            vec!["openapi", "schema", "docs"],
        )),
        "umbra-security" => Some(simple_official_plugin(
            "umbra-security",
            "umbra-security",
            "Security headers, CSRF protections and request hardening defaults for Umbra projects.",
            "sc",
            "umbra add umbra-security",
            "plugins += [\"umbra.security\"]",
            vec!["security", "csrf", "headers"],
        )),
        "umbra-static" => Some(simple_official_plugin(
            "umbra-static",
            "umbra-static",
            "Static file serving for production builds and plugin-shipped frontend assets.",
            "st",
            "umbra add umbra-static",
            "plugins += [\"umbra.static\"]",
            vec!["static", "assets", "frontend"],
        )),
        "umbra-realtime" => Some(simple_planned_plugin(
            "umbra-realtime",
            "umbra-realtime",
            "SSE and WebSocket primitives planned for live comments, queues and admin state.",
            "rt",
            "umbra add umbra-realtime",
            "plugins += [\"umbra.realtime\"]",
            vec!["realtime", "sse", "websocket"],
        )),
        "umbra-multitenancy" => Some(community_plugin(
            "umbra-multitenancy",
            "umbra-multitenancy",
            "Schema-per-tenant and row-level tenancy with admin-aware scoping, tenant-aware migrations and middleware for request routing.",
            "mt",
            "@kanto",
            "umbra add umbra-multitenancy",
            vec!["tenancy", "postgres", "middleware"],
        )),
        "umbra-oauth-github" => Some(community_plugin(
            "umbra-oauth-github",
            "umbra-oauth-github",
            "GitHub OAuth, avatar import and account-age gates for reviews, submissions and malicious-plugin voting.",
            "gh",
            "@devlin",
            "umbra add umbra-oauth-github",
            vec!["auth", "oauth", "identity"],
        )),
        "umbra-storage-s3" => Some(community_plugin(
            "umbra-storage-s3",
            "umbra-storage-s3",
            "Drop-in S3 and S3-compatible media backend for the content plugin, with signed URLs, lifecycle rules and streaming uploads.",
            "s3",
            "@lumen-labs",
            "umbra add umbra-storage-s3",
            vec!["storage", "s3", "media"],
        )),
        "fast-secrets-vault" => Some(PluginDetail {
            slug: "fast-secrets-vault",
            name: "fast-secrets-vault",
            source: "flagged",
            maintainer: "@anon-9931",
            description: "Claims to manage app secrets. Auditors confirmed it exfiltrates environment variables during migration. Do not install.",
            icon: "!",
            featured: false,
            flagged: true,
            status: "Flagged",
            status_kind: "bad",
            maturity: "Malicious",
            channel: "Delisted from install",
            install: "do not install",
            toml: "security advisory active",
            rating: "0.0",
            installs: "delisted",
            notes: "87",
            tags: vec!["secrets", "do-not-install"],
            features: vec![blocked("Install"), blocked("Migration hook"), blocked("Runtime safety")],
            shipped_features: 0,
            total_features: 3,
            usage_title: "Security advisory",
            usage_intro: "This listing is kept visible for transparency. The package is delisted from install surfaces.",
            usage_code: "Do not install fast-secrets-vault v0.2.x.\nConfirmed behavior: environment variable exfiltration during migration.",
            audit_label: "Malicious",
            audit_date: "Confirmed",
            audit_kind: "bad",
            compatibility: vec![blocked("All supported Umbra versions")],
            comments: vec![CommentPreview {
                initials: "SR",
                name: "Security review",
                role: "Umbra plugin auditors",
                badge: "do not install",
                badge_kind: "bad",
                body: "Confirmed malicious behavior in v0.2.x. The listing remains visible so teams can recognize and remove the package.",
            }],
        }),
        _ => None,
    }
}

fn simple_official_plugin(
    slug: &'static str,
    name: &'static str,
    description: &'static str,
    icon: &'static str,
    install: &'static str,
    toml: &'static str,
    tags: Vec<&'static str>,
) -> PluginDetail {
    PluginDetail {
        slug,
        name,
        source: "official",
        maintainer: "Umbra team",
        description,
        icon,
        featured: false,
        flagged: false,
        status: "Shipped",
        status_kind: "ok",
        maturity: "Stable",
        channel: "Stable channel",
        install,
        toml,
        rating: "4.8",
        installs: "preview",
        notes: "12",
        tags,
        features: vec![
            shipped("Core contract"),
            shipped("Admin integration"),
            shipped("Migration support"),
            beta("Detailed docs"),
        ],
        shipped_features: 3,
        total_features: 4,
        usage_title: "Usage",
        usage_intro: "This official battery can be enabled through the project plugin list and later replaced by any compatible plugin.",
        usage_code: toml,
        audit_label: "Audited",
        audit_date: "Internal",
        audit_kind: "ok",
        compatibility: vec![shipped("Umbra v0.1.x"), shipped("PostgreSQL"), partial("SQLite")],
        comments: default_comments(),
    }
}

fn simple_experimental_plugin(
    slug: &'static str,
    name: &'static str,
    description: &'static str,
    icon: &'static str,
    install: &'static str,
    toml: &'static str,
    tags: Vec<&'static str>,
) -> PluginDetail {
    let mut plugin = simple_official_plugin(slug, name, description, icon, install, toml, tags);
    plugin.status = "Experimental";
    plugin.status_kind = "warn";
    plugin.maturity = "Experimental";
    plugin.channel = "Experimental channel";
    plugin.audit_label = "Internal preview";
    plugin.audit_kind = "warn";
    plugin
}

fn simple_planned_plugin(
    slug: &'static str,
    name: &'static str,
    description: &'static str,
    icon: &'static str,
    install: &'static str,
    toml: &'static str,
    tags: Vec<&'static str>,
) -> PluginDetail {
    let mut plugin = simple_official_plugin(slug, name, description, icon, install, toml, tags);
    plugin.status = "Planned";
    plugin.status_kind = "muted";
    plugin.maturity = "Planned";
    plugin.channel = "Roadmap";
    plugin.audit_label = "Not started";
    plugin.audit_kind = "muted";
    plugin.features = vec![planned("Core contract"), planned("Docs"), planned("Compatibility tests")];
    plugin.shipped_features = 0;
    plugin.total_features = 3;
    plugin
}

fn community_plugin(
    slug: &'static str,
    name: &'static str,
    description: &'static str,
    icon: &'static str,
    maintainer: &'static str,
    install: &'static str,
    tags: Vec<&'static str>,
) -> PluginDetail {
    PluginDetail {
        slug,
        name,
        source: "community",
        maintainer,
        description,
        icon,
        featured: false,
        flagged: false,
        status: "Community",
        status_kind: "warn",
        maturity: "Unverified",
        channel: "Community channel",
        install,
        toml: install,
        rating: "4.6",
        installs: "preview",
        notes: "18",
        tags,
        features: vec![
            shipped("Plugin contract"),
            shipped("Example project"),
            partial("Compatibility matrix"),
            planned("Umbra audit"),
        ],
        shipped_features: 2,
        total_features: 4,
        usage_title: "Usage",
        usage_intro: "Community plugins can honor the same contracts as first-party batteries. Review the audit status before installing.",
        usage_code: install,
        audit_label: "Unverified",
        audit_date: "Pending",
        audit_kind: "warn",
        compatibility: vec![partial("Umbra v0.1.x"), shipped("PostgreSQL"), planned("SQLite")],
        comments: default_comments(),
    }
}

fn default_comments() -> Vec<CommentPreview> {
    vec![CommentPreview {
        initials: "DR",
        name: "Directory preview",
        role: "static data",
        badge: "backend pending",
        badge_kind: "warn",
        body: "This page is served by the plugin directory backend now. The content is static until the database-backed detail view replaces it.",
    }]
}

fn shipped(label: &'static str) -> StatusRow {
    StatusRow {
        label,
        status: "shipped",
        kind: "ok",
    }
}

fn beta(label: &'static str) -> StatusRow {
    StatusRow {
        label,
        status: "beta",
        kind: "warn",
    }
}

fn partial(label: &'static str) -> StatusRow {
    StatusRow {
        label,
        status: "partial",
        kind: "warn",
    }
}

fn planned(label: &'static str) -> StatusRow {
    StatusRow {
        label,
        status: "planned",
        kind: "muted",
    }
}

fn blocked(label: &'static str) -> StatusRow {
    StatusRow {
        label,
        status: "blocked",
        kind: "bad",
    }
}
