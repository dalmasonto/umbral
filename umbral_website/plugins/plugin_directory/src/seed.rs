//! Seed data for the plugin directory.
//!
//! Populates the first-party Umbral plugin rows so the public
//! landing page (`plugins/public`) can render the plugin map from
//! the database instead of falling back to the static table in
//! `home.html`.
//!
//! Idempotent: short-circuits if any `Plugin` rows already exist.
//! Manual re-seeding: `DELETE FROM plugin;` then trigger the
//! plugin's `on_ready` again (or call this function from a
//! one-off CLI command).

use crate::models::{
    CommentKind, CommentModeration, Plugin, PluginComment, PluginFeature, PluginMaturity,
    PluginModeration, PluginSource, PluginStatus, plugin, plugin_feature,
};
use chrono::Utc;
use umbral::prelude::*;

/// One row of official Umbral plugin data. Hand-curated; the spec
/// for the landing page (`planning/umbral-site.md` §"Plugin map")
/// is the source of truth.
struct OfficialRow {
    crate_name: &'static str,
    name: &'static str,
    slug: &'static str,
    author: &'static str,
    short_description: &'static str,
    full_content: &'static str,
    installation_commands: &'static str,
    version: &'static str,
    status: PluginStatus,
    maturity: PluginMaturity,
    featured: bool,
    display_order: i32,
}

const OFFICIAL: &[OfficialRow] = &[
    OfficialRow {
        crate_name: "umbral-admin",
        name: "Umbral Admin",
        slug: "umbral-admin",
        author: "Umbral contributors",
        short_description: "auto CRUD, dashboards, audit, filters",
        full_content:
            "Auto-generated admin UI for every model in every plugin. Mount the plugin in `main.rs` \
             and every registered model gets a list, filters, sheets, bulk actions, and an audit log.",
        installation_commands: "umbral-admin = { path = \"../plugins/umbral-admin\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: true,
        display_order: 10,
    },
    OfficialRow {
        crate_name: "umbral-auth",
        name: "Umbral Auth",
        slug: "umbral-auth",
        author: "Umbral contributors",
        short_description: "users, groups, argon2, password reset",
        full_content:
            "Built-in authentication: user model, group model, argon2 password hashing, \
             password reset flows, and the `LoggedIn<T>` extractor.",
        installation_commands: "umbral-auth = { path = \"../plugins/umbral-auth\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: true,
        display_order: 20,
    },
    OfficialRow {
        crate_name: "umbral-sessions",
        name: "Umbral Sessions",
        slug: "umbral-sessions",
        author: "Umbral contributors",
        short_description: "session store, middleware",
        full_content:
            "Server-side session store and middleware, layered on tower-sessions. Pairs with \
             umbral-auth to keep the user identity available across requests.",
        installation_commands: "umbral-sessions = { path = \"../plugins/umbral-sessions\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 30,
    },
    OfficialRow {
        crate_name: "umbral-rest",
        name: "Umbral REST",
        slug: "umbral-rest",
        author: "Umbral contributors",
        short_description: "serializers, viewsets, pagination",
        full_content:
            "DRF-equivalent: serializers, viewsets, routers, pagination, filters. The same \
             models that power the admin become API resources.",
        installation_commands: "umbral-rest = { path = \"../plugins/umbral-rest\" }",
        version: "0.1.0",
        status: PluginStatus::Usable,
        maturity: PluginMaturity::Beta,
        featured: true,
        display_order: 40,
    },
    OfficialRow {
        crate_name: "umbral-openapi",
        name: "Umbral OpenAPI",
        slug: "umbral-openapi",
        author: "Umbral contributors",
        short_description: "schema gen, swagger UI",
        full_content:
            "Schema generation and Swagger UI for the REST plugin. Mounts the interactive \
             API explorer at `/openapi/`.",
        installation_commands: "umbral-openapi = { path = \"../plugins/umbral-openapi\" }",
        version: "0.1.0",
        status: PluginStatus::Usable,
        maturity: PluginMaturity::Beta,
        featured: false,
        display_order: 50,
    },
    OfficialRow {
        crate_name: "umbral-tasks",
        name: "Umbral Tasks",
        slug: "umbral-tasks",
        author: "Umbral contributors",
        short_description: "DB-backed job queue, retries, schedules",
        full_content:
            "DB-backed background task queue (Celery-equivalent). Define tasks with `#[task]`, \
             enqueue from handlers, run with `cargo run -- worker`.",
        installation_commands: "umbral-tasks = { path = \"../plugins/umbral-tasks\" }",
        version: "0.0.1",
        status: PluginStatus::Experimental,
        maturity: PluginMaturity::Alpha,
        featured: false,
        display_order: 60,
    },
    OfficialRow {
        crate_name: "umbral-security",
        name: "Umbral Security",
        slug: "umbral-security",
        author: "Umbral contributors",
        short_description: "CSRF, HSTS, headers, escape hatches",
        full_content:
            "Secure-by-default middleware: CSRF protection, HSTS, clickjacking headers, \
             and escape hatches for the cases where the framework defaults are too tight.",
        installation_commands: "umbral-security = { path = \"../plugins/umbral-security\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 70,
    },
    OfficialRow {
        crate_name: "umbral-static",
        name: "Umbral Static",
        slug: "umbral-static",
        author: "Umbral contributors",
        short_description: "prod static file serving",
        full_content:
            "Production static file serving (whitenoise-equivalent). Serves compiled CSS, \
             baked screenshots, and the user-uploaded media dir.",
        installation_commands: "umbral-static = { path = \"../plugins/umbral-static\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 80,
    },
];

/// Idempotent. Returns the number of rows inserted.
pub async fn seed_official_plugins() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    if Plugin::objects().count().await? > 0 {
        return Ok(0);
    }

    let mut inserted = 0;
    for row in OFFICIAL {
        let mut p = Plugin::default();
        p.name = row.name.to_string();
        p.slug = row.slug.to_string();
        p.crate_name = row.crate_name.to_string();
        p.author = row.author.to_string();
        p.short_description = row.short_description.to_string();
        p.full_content = row.full_content.to_string();
        p.installation_commands = row.installation_commands.to_string();
        p.version = Some(row.version.to_string());
        p.license = Some("MIT OR Apache-2.0".to_string());
        p.status = row.status;
        p.maturity = row.maturity;
        // source + moderation are populated by `Default` (community,
        // pending) — override for official/approved rows.
        p.source = PluginSource::Official;
        p.moderation = PluginModeration::Approved;
        p.featured = row.featured;
        p.display_order = row.display_order;
        Plugin::objects().create(p).await?;
        inserted += 1;
    }
    Ok(inserted)
}

/// Editorial audit assessment for each first-party plugin, keyed by
/// crate name. `audit_status` is a curated editorial field (like
/// `status` / `maturity`), NOT an externally-synced metric — so unlike
/// `github_stars` / `downloads` it's legitimate to seed. The values
/// drive the admin "Audit coverage" gauge and the per-plugin audit
/// badge on the public site.
const AUDIT: &[(&str, &str)] = &[
    ("umbral-admin", "umbral_reviewed"),
    ("umbral-auth", "umbral_reviewed"),
    ("umbral-sessions", "umbral_reviewed"),
    ("umbral-rest", "self_reviewed"),
    ("umbral-openapi", "self_reviewed"),
    ("umbral-tasks", "needs_review"),
    ("umbral-security", "third_party_reviewed"),
    ("umbral-static", "self_reviewed"),
];

/// Back-fill `audit_status` on already-seeded rows. Idempotent: only
/// touches rows still at the `not_reviewed` default, so an admin's
/// later hand-edit is never clobbered, and re-running is a no-op once
/// every row has its curated value. This runs every boot (the row
/// insert short-circuits once the table is populated, so without this
/// the existing rows would never gain their audit status).
pub async fn backfill_audit_status() -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let mut updated = 0;
    for (crate_name, audit) in AUDIT {
        let mut values = serde_json::Map::new();
        values.insert(
            "audit_status".to_string(),
            serde_json::Value::String((*audit).to_string()),
        );
        updated += Plugin::objects()
            .filter(plugin::CRATE_NAME.eq(*crate_name))
            .filter(plugin::AUDIT_STATUS.eq("not_reviewed"))
            .update_values(values)
            .await?;
    }
    Ok(updated)
}

/// One demo discussion note: which plugin it hangs off (by crate name),
/// the body, and its kind. Seeds the comment threads so the admin
/// dashboard's Discussion Notes / activity / recent-activity widgets
/// have real engagement data instead of empty zeros.
struct DemoNote {
    crate_name: &'static str,
    body: &'static str,
    kind: CommentKind,
}

const DEMO_NOTES: &[DemoNote] = &[
    DemoNote {
        crate_name: "umbral-admin",
        body: "The auto-generated dashboards saved us about a week of glue code.",
        kind: CommentKind::UsageNote,
    },
    DemoNote {
        crate_name: "umbral-admin",
        body: "Does the changelist support registering custom bulk actions yet?",
        kind: CommentKind::Question,
    },
    DemoNote {
        crate_name: "umbral-auth",
        body: "argon2 defaults are sensible — migrated off bcrypt without surprises.",
        kind: CommentKind::UsageNote,
    },
    DemoNote {
        crate_name: "umbral-rest",
        body: "Pagination + filters are great. Any plan for cursor pagination?",
        kind: CommentKind::Question,
    },
    DemoNote {
        crate_name: "umbral-rest",
        body: "Confirmed working end-to-end on Postgres 16.",
        kind: CommentKind::CompatibilityNote,
    },
    DemoNote {
        crate_name: "umbral-openapi",
        body: "Swagger UI mounts cleanly at /openapi/ — handy for sharing the API.",
        kind: CommentKind::UsageNote,
    },
    DemoNote {
        crate_name: "umbral-tasks",
        body: "Retry backoff is configurable, which covered our flaky-webhook case.",
        kind: CommentKind::General,
    },
    DemoNote {
        crate_name: "umbral-static",
        body: "Serves compiled CSS + uploaded media in prod without reaching for nginx.",
        kind: CommentKind::General,
    },
];

/// Seed the demo discussion notes. Idempotent: short-circuits if any
/// comment already exists. Each note is published (`Visible`) so it
/// counts toward the dashboard's visible-notes metrics, and is bound to
/// its plugin by a `crate_name` lookup (skipped if the plugin is
/// missing). Returns the number of notes inserted.
pub async fn seed_demo_comments() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    if PluginComment::objects().count().await? > 0 {
        return Ok(0);
    }

    let mut inserted = 0;
    for note in DEMO_NOTES {
        let Some(plugin) = Plugin::objects()
            .filter(plugin::CRATE_NAME.eq(note.crate_name))
            .first()
            .await?
        else {
            continue;
        };
        let mut comment = PluginComment {
            plugin: ForeignKey::new(plugin.id),
            body: note.body.to_string(),
            kind: note.kind,
            moderation: CommentModeration::Visible,
            ..Default::default()
        };
        // The Form-derived Default leaves `author` None (a visitor note);
        // the dashboard widgets key off the body + plugin + created_at,
        // none of which need an author.
        comment.author = None;
        PluginComment::objects().create(comment).await?;
        inserted += 1;
    }
    Ok(inserted)
}

// ---------------------------------------------------------------------------
// Per-plugin feature tracker rows.
// ---------------------------------------------------------------------------

/// One curated feature row for an official plugin. `status`/`maturity` are
/// editorial facts about the framework (like `audit_status`), not external
/// metrics — legitimate to seed. Powers the `/prebuilt` feature grid and
/// the `/plugins/{slug}` tracker.
struct FeatureSeed {
    name: &'static str,
    description: &'static str,
    status: PluginStatus,
    maturity: PluginMaturity,
}

/// The feature set for one official plugin, keyed by crate name.
struct PluginFeatureSet {
    crate_name: &'static str,
    features: &'static [FeatureSeed],
}

const S: PluginStatus = PluginStatus::Shipped;
const U: PluginStatus = PluginStatus::Usable;
const E: PluginStatus = PluginStatus::Experimental;
const IP: PluginStatus = PluginStatus::InProgress;
const PL: PluginStatus = PluginStatus::Planned;
const STA: PluginMaturity = PluginMaturity::Stable;
const BETA: PluginMaturity = PluginMaturity::Beta;
const ALPHA: PluginMaturity = PluginMaturity::Alpha;
const DES: PluginMaturity = PluginMaturity::Design;

/// Hand-curated feature tracker per official plugin. Mirrors the real
/// status of each capability in the framework (see `planning/features.md`).
const PLUGIN_FEATURES: &[PluginFeatureSet] = &[
    PluginFeatureSet {
        crate_name: "umbral-admin",
        features: &[
            FeatureSeed { name: "Auto CRUD views", description: "List, create, edit, delete generated from every registered model.", status: S, maturity: STA },
            FeatureSeed { name: "Search and multi-filter", description: "Toolbar search plus combinable `list_filter` facets.", status: S, maturity: STA },
            FeatureSeed { name: "FK / M2M / O2O pickers", description: "Async relation pickers with search-as-you-type.", status: S, maturity: STA },
            FeatureSeed { name: "Per-model permissions", description: "Per-action `view/add/change/delete` gating via umbral-permissions.", status: S, maturity: STA },
            FeatureSeed { name: "File and image widgets", description: "Multipart upload with image thumbnail preview.", status: S, maturity: STA },
            FeatureSeed { name: "Markdown / RTE field widgets", description: "`#[umbral(widget = ...)]` renders rich editors in the form.", status: S, maturity: STA },
            FeatureSeed { name: "Dashboard widgets", description: "KPI cards, charts, and recent-activity panels on the index.", status: IP, maturity: BETA },
            FeatureSeed { name: "Bulk actions", description: "Select rows then act — delete, publish, export.", status: PL, maturity: DES },
            FeatureSeed { name: "Inline editing", description: "Edit related rows on the parent form (tabular / stacked).", status: PL, maturity: DES },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-auth",
        features: &[
            FeatureSeed { name: "User and group models", description: "Built-in `AuthUser` plus groups and roles.", status: S, maturity: STA },
            FeatureSeed { name: "Argon2 password hashing", description: "Modern password hashing with sensible defaults.", status: S, maturity: STA },
            FeatureSeed { name: "Permissions and RBAC", description: "Group/permission M2M checks via umbral-permissions.", status: S, maturity: STA },
            FeatureSeed { name: "Bearer tokens", description: "Opaque DB-backed API tokens, hashed at rest.", status: S, maturity: STA },
            FeatureSeed { name: "OAuth / social login", description: "Sign in with Google/GitHub and connect accounts (umbral-oauth).", status: S, maturity: BETA },
            FeatureSeed { name: "Password reset", description: "Token-based reset flow (email delivery pending umbral-email).", status: IP, maturity: BETA },
            FeatureSeed { name: "SSO / OIDC", description: "Enterprise single sign-on.", status: PL, maturity: DES },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-sessions",
        features: &[
            FeatureSeed { name: "DB-backed session store", description: "Server-side sessions persisted through the ORM.", status: S, maturity: STA },
            FeatureSeed { name: "Session middleware", description: "Cookie handling with secure defaults.", status: S, maturity: STA },
            FeatureSeed { name: "Login / logout flow", description: "Establish and tear down the authenticated session.", status: S, maturity: STA },
            FeatureSeed { name: "Redis-backed sessions", description: "Shared session store for horizontal scaling.", status: PL, maturity: DES },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-rest",
        features: &[
            FeatureSeed { name: "Serializers and viewsets", description: "Models become JSON resources with zero config.", status: S, maturity: BETA },
            FeatureSeed { name: "Routers and pagination", description: "Collection/detail routes with page slicing.", status: S, maturity: BETA },
            FeatureSeed { name: "Filtering and search", description: "Query-string filters and free-text search per resource.", status: S, maturity: BETA },
            FeatureSeed { name: "Authentication and permissions", description: "Session/bearer auth chain with per-resource permission gates.", status: S, maturity: BETA },
            FeatureSeed { name: "Endpoint discovery", description: "`GET /api/` API root listing resources and plugin endpoints.", status: S, maturity: BETA },
            FeatureSeed { name: "Custom @action endpoints", description: "Collection/detail actions beyond CRUD.", status: U, maturity: BETA },
            FeatureSeed { name: "Nested writable serializers", description: "Create a parent and its children in one request.", status: PL, maturity: DES },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-openapi",
        features: &[
            FeatureSeed { name: "OpenAPI 3 schema generation", description: "Auto-generated spec from registered resources.", status: S, maturity: BETA },
            FeatureSeed { name: "Playground UI", description: "Mini-Postman request/response surface (umbral-playground).", status: S, maturity: BETA },
            FeatureSeed { name: "Vendor extensions", description: "FK targets, enums, nullable/readOnly surfaced in the schema.", status: S, maturity: BETA },
            FeatureSeed { name: "securitySchemes publishing", description: "Auth requirements per endpoint for auto-detect in the playground.", status: IP, maturity: BETA },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-tasks",
        features: &[
            FeatureSeed { name: "#[task] macro", description: "Annotate a function as an enqueueable background job.", status: U, maturity: ALPHA },
            FeatureSeed { name: "DB-backed queue", description: "Jobs persisted to a table and drained by a worker.", status: E, maturity: ALPHA },
            FeatureSeed { name: "Worker process", description: "`cargo run -- worker` consumes and executes jobs.", status: E, maturity: ALPHA },
            FeatureSeed { name: "Retries and backoff", description: "Failed jobs retry with exponential backoff.", status: E, maturity: ALPHA },
            FeatureSeed { name: "Scheduled tasks", description: "Run a job at a future `eta`.", status: PL, maturity: DES },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-security",
        features: &[
            FeatureSeed { name: "CSRF protection", description: "Double-submit token enforced on every POST.", status: S, maturity: STA },
            FeatureSeed { name: "HSTS and secure headers", description: "Strict-Transport-Security and friends by default.", status: S, maturity: STA },
            FeatureSeed { name: "Clickjacking protection", description: "X-Frame-Options / frame-ancestors headers.", status: S, maturity: STA },
            FeatureSeed { name: "Template auto-escaping", description: "Output escaped by default; opt out explicitly.", status: S, maturity: STA },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-static",
        features: &[
            FeatureSeed { name: "Production static serving", description: "Serve compiled assets and uploaded media in prod.", status: S, maturity: STA },
            FeatureSeed { name: "collectstatic command", description: "Gather every plugin's static dir into one output tree.", status: S, maturity: STA },
            FeatureSeed { name: "gzip / brotli compression", description: "Compressed responses for static assets.", status: PL, maturity: DES },
        ],
    },
];

/// Slugify a feature name into the `<crate>-<name>` unique-slug tail.
fn feature_slug(crate_name: &str, name: &str) -> String {
    let tail: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let tail = tail
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    format!("{crate_name}-{tail}")
}

/// Seed each official plugin's feature tracker rows. Idempotent per plugin:
/// a plugin that already has features is skipped, so this runs every boot
/// (the plugin rows seed first, then this back-fills their features) and a
/// re-run after adding a new plugin's feature list only inserts the new
/// rows. Returns the number of feature rows inserted.
pub async fn seed_plugin_features() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let mut inserted = 0;
    for set in PLUGIN_FEATURES {
        let Some(plugin) = Plugin::objects()
            .filter(plugin::CRATE_NAME.eq(set.crate_name))
            .first()
            .await?
        else {
            continue;
        };
        if PluginFeature::objects()
            .filter(plugin_feature::PLUGIN.eq(plugin.id))
            .count()
            .await?
            > 0
        {
            continue;
        }
        for (i, f) in set.features.iter().enumerate() {
            let now = Utc::now();
            let row = PluginFeature {
                id: 0,
                plugin: ForeignKey::new(plugin.id),
                name: f.name.to_string(),
                slug: feature_slug(set.crate_name, f.name),
                description: f.description.to_string(),
                status: f.status,
                maturity: f.maturity,
                release_target: None,
                docs_url: None,
                example_url: None,
                display_order: (i as i32) * 10,
                visible: true,
                created_at: now,
                updated_at: now,
                deleted_at: None,
            };
            PluginFeature::objects().create(row).await?;
            inserted += 1;
        }
    }
    Ok(inserted)
}
