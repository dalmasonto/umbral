//! Idempotent seed for the `/showcase` gallery.
//!
//! Honesty matters here: Umbra is greenfield, so we do NOT fabricate
//! third-party adopters. We seed only the framework's OWN properties that
//! are genuinely built with Umbra (dogfooding) — this website, the docs,
//! and the shop example. Real third-party submissions land via the form.

use crate::models::{
    ShowcaseDatabase, ShowcaseDeployment, ShowcaseEntry, ShowcaseProjectType, ShowcaseStatus,
};

const REPO: &str = "https://github.com/dalmasonto/umbra";

struct Seed {
    project_name: &'static str,
    url: &'static str,
    owner: &'static str,
    short_description: &'static str,
    plugins_used: &'static str,
    project_type: ShowcaseProjectType,
    database: ShowcaseDatabase,
    deployment: ShowcaseDeployment,
    featured: bool,
}

const ENTRIES: &[Seed] = &[
    Seed {
        project_name: "Umbra Plugin Directory",
        url: "https://github.com/dalmasonto/umbra/tree/main/umbra_website",
        owner: "Umbra team",
        short_description: "The official umbra.dev site — a plugin directory, moderation admin, OAuth login, and REST API, all built on Umbra itself.",
        plugins_used: "admin, auth, sessions, oauth, rest, openapi, security, static, media",
        project_type: ShowcaseProjectType::Website,
        database: ShowcaseDatabase::Postgres,
        deployment: ShowcaseDeployment::SelfHosted,
        featured: true,
    },
    Seed {
        project_name: "Shop Example",
        url: "https://github.com/dalmasonto/umbra/tree/main/examples/shop",
        owner: "Umbra team",
        short_description: "A reference e-commerce app that exercises the ORM, migrations, admin, and REST end to end — the canonical 'build a real app' walkthrough.",
        plugins_used: "admin, auth, rest, openapi",
        project_type: ShowcaseProjectType::Demo,
        database: ShowcaseDatabase::Sqlite,
        deployment: ShowcaseDeployment::SelfHosted,
        featured: true,
    },
    Seed {
        project_name: "Umbra Documentation",
        url: "https://github.com/dalmasonto/umbra/tree/main/documentation",
        owner: "Umbra team",
        short_description: "The framework's documentation site, built with Specra — guides for the ORM, migrations, admin, auth, and REST.",
        plugins_used: "static",
        project_type: ShowcaseProjectType::Website,
        database: ShowcaseDatabase::Sqlite,
        deployment: ShowcaseDeployment::SelfHosted,
        featured: false,
    },
];

/// Seed the dogfooding showcase entries. Idempotent: short-circuits if any
/// entry exists. Returns the number inserted.
pub async fn seed() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    if ShowcaseEntry::objects().count().await? > 0 {
        return Ok(0);
    }
    let mut n = 0;
    for s in ENTRIES {
        let mut e = ShowcaseEntry::default();
        e.project_name = s.project_name.to_string();
        e.url = s.url.to_string();
        e.owner = s.owner.to_string();
        e.short_description = s.short_description.to_string();
        e.plugins_used = Some(s.plugins_used.to_string());
        e.source_url = Some(REPO.to_string());
        e.project_type = s.project_type;
        e.database_backend = s.database;
        e.deployment_platform = s.deployment;
        e.verified = true;
        e.featured = s.featured;
        e.status = if s.featured {
            ShowcaseStatus::Featured
        } else {
            ShowcaseStatus::Verified
        };
        ShowcaseEntry::objects().create(e).await?;
        n += 1;
    }
    Ok(n)
}
