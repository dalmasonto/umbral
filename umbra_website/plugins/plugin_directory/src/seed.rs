//! Seed data for the plugin directory.
//!
//! Populates the first-party Umbra plugin rows so the public
//! landing page (`plugins/public`) can render the plugin map from
//! the database instead of falling back to the static table in
//! `home.html`.
//!
//! Idempotent: short-circuits if any `Plugin` rows already exist.
//! Manual re-seeding: `DELETE FROM plugin;` then trigger the
//! plugin's `on_ready` again (or call this function from a
//! one-off CLI command).

use crate::models::{
    CommentKind, CommentModeration, Plugin, PluginComment, PluginMaturity, PluginModeration,
    PluginSource, PluginStatus, plugin,
};
use umbra::prelude::*;

/// One row of official Umbra plugin data. Hand-curated; the spec
/// for the landing page (`planning/umbra-site.md` §"Plugin map")
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
        crate_name: "umbra-admin",
        name: "Umbra Admin",
        slug: "umbra-admin",
        author: "Umbra contributors",
        short_description: "auto CRUD, dashboards, audit, filters",
        full_content:
            "Auto-generated admin UI for every model in every plugin. Mount the plugin in `main.rs` \
             and every registered model gets a list, filters, sheets, bulk actions, and an audit log.",
        installation_commands: "umbra-admin = { path = \"../plugins/umbra-admin\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: true,
        display_order: 10,
    },
    OfficialRow {
        crate_name: "umbra-auth",
        name: "Umbra Auth",
        slug: "umbra-auth",
        author: "Umbra contributors",
        short_description: "users, groups, argon2, password reset",
        full_content:
            "Built-in authentication: user model, group model, argon2 password hashing, \
             password reset flows, and the `LoggedIn<T>` extractor.",
        installation_commands: "umbra-auth = { path = \"../plugins/umbra-auth\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: true,
        display_order: 20,
    },
    OfficialRow {
        crate_name: "umbra-sessions",
        name: "Umbra Sessions",
        slug: "umbra-sessions",
        author: "Umbra contributors",
        short_description: "session store, middleware",
        full_content:
            "Server-side session store and middleware, layered on tower-sessions. Pairs with \
             umbra-auth to keep the user identity available across requests.",
        installation_commands: "umbra-sessions = { path = \"../plugins/umbra-sessions\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 30,
    },
    OfficialRow {
        crate_name: "umbra-rest",
        name: "Umbra REST",
        slug: "umbra-rest",
        author: "Umbra contributors",
        short_description: "serializers, viewsets, pagination",
        full_content:
            "DRF-equivalent: serializers, viewsets, routers, pagination, filters. The same \
             models that power the admin become API resources.",
        installation_commands: "umbra-rest = { path = \"../plugins/umbra-rest\" }",
        version: "0.1.0",
        status: PluginStatus::Usable,
        maturity: PluginMaturity::Beta,
        featured: true,
        display_order: 40,
    },
    OfficialRow {
        crate_name: "umbra-openapi",
        name: "Umbra OpenAPI",
        slug: "umbra-openapi",
        author: "Umbra contributors",
        short_description: "schema gen, swagger UI",
        full_content:
            "Schema generation and Swagger UI for the REST plugin. Mounts the interactive \
             API explorer at `/openapi/`.",
        installation_commands: "umbra-openapi = { path = \"../plugins/umbra-openapi\" }",
        version: "0.1.0",
        status: PluginStatus::Usable,
        maturity: PluginMaturity::Beta,
        featured: false,
        display_order: 50,
    },
    OfficialRow {
        crate_name: "umbra-tasks",
        name: "Umbra Tasks",
        slug: "umbra-tasks",
        author: "Umbra contributors",
        short_description: "DB-backed job queue, retries, schedules",
        full_content:
            "DB-backed background task queue (Celery-equivalent). Define tasks with `#[task]`, \
             enqueue from handlers, run with `cargo run -- worker`.",
        installation_commands: "umbra-tasks = { path = \"../plugins/umbra-tasks\" }",
        version: "0.0.1",
        status: PluginStatus::Experimental,
        maturity: PluginMaturity::Alpha,
        featured: false,
        display_order: 60,
    },
    OfficialRow {
        crate_name: "umbra-security",
        name: "Umbra Security",
        slug: "umbra-security",
        author: "Umbra contributors",
        short_description: "CSRF, HSTS, headers, escape hatches",
        full_content:
            "Secure-by-default middleware: CSRF protection, HSTS, clickjacking headers, \
             and escape hatches for the cases where the framework defaults are too tight.",
        installation_commands: "umbra-security = { path = \"../plugins/umbra-security\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 70,
    },
    OfficialRow {
        crate_name: "umbra-static",
        name: "Umbra Static",
        slug: "umbra-static",
        author: "Umbra contributors",
        short_description: "prod static file serving",
        full_content:
            "Production static file serving (whitenoise-equivalent). Serves compiled CSS, \
             baked screenshots, and the user-uploaded media dir.",
        installation_commands: "umbra-static = { path = \"../plugins/umbra-static\" }",
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
    ("umbra-admin", "umbra_reviewed"),
    ("umbra-auth", "umbra_reviewed"),
    ("umbra-sessions", "umbra_reviewed"),
    ("umbra-rest", "self_reviewed"),
    ("umbra-openapi", "self_reviewed"),
    ("umbra-tasks", "needs_review"),
    ("umbra-security", "third_party_reviewed"),
    ("umbra-static", "self_reviewed"),
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
        crate_name: "umbra-admin",
        body: "The auto-generated dashboards saved us about a week of glue code.",
        kind: CommentKind::UsageNote,
    },
    DemoNote {
        crate_name: "umbra-admin",
        body: "Does the changelist support registering custom bulk actions yet?",
        kind: CommentKind::Question,
    },
    DemoNote {
        crate_name: "umbra-auth",
        body: "argon2 defaults are sensible — migrated off bcrypt without surprises.",
        kind: CommentKind::UsageNote,
    },
    DemoNote {
        crate_name: "umbra-rest",
        body: "Pagination + filters are great. Any plan for cursor pagination?",
        kind: CommentKind::Question,
    },
    DemoNote {
        crate_name: "umbra-rest",
        body: "Confirmed working end-to-end on Postgres 16.",
        kind: CommentKind::CompatibilityNote,
    },
    DemoNote {
        crate_name: "umbra-openapi",
        body: "Swagger UI mounts cleanly at /openapi/ — handy for sharing the API.",
        kind: CommentKind::UsageNote,
    },
    DemoNote {
        crate_name: "umbra-tasks",
        body: "Retry backoff is configurable, which covered our flaky-webhook case.",
        kind: CommentKind::General,
    },
    DemoNote {
        crate_name: "umbra-static",
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
