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
    Plugin, PluginMaturity, PluginModeration, PluginSource, PluginStatus,
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
