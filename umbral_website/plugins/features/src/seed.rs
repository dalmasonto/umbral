//! Idempotent seed for the framework feature catalog (`/features`).
//!
//! A curated, category-grouped map of framework capabilities and their
//! status — editorial facts (like a roadmap), safe to seed. Distinct from
//! the per-plugin feature trackers on `/prebuilt` and `/plugins/{slug}`.

use crate::models::{FeatureCategory, FeatureMaturity, FeatureStatus, FrameworkFeature};
use chrono::Utc;
use umbral::orm::{ForeignKey, slugify};

const SH: FeatureStatus = FeatureStatus::Shipped;
const US: FeatureStatus = FeatureStatus::Usable;
const EX: FeatureStatus = FeatureStatus::Experimental;
const IP: FeatureStatus = FeatureStatus::InProgress;
const PL: FeatureStatus = FeatureStatus::Planned;
const STA: FeatureMaturity = FeatureMaturity::Stable;
const BETA: FeatureMaturity = FeatureMaturity::Beta;
const ALPHA: FeatureMaturity = FeatureMaturity::Alpha;
const DES: FeatureMaturity = FeatureMaturity::Design;

struct Feat {
    name: &'static str,
    summary: &'static str,
    status: FeatureStatus,
    maturity: FeatureMaturity,
}

struct Category {
    name: &'static str,
    slug: &'static str,
    description: &'static str,
    features: &'static [Feat],
}

const CATALOG: &[Category] = &[
    Category {
        name: "ORM & Migrations",
        slug: "orm-migrations",
        description: "Declare models, query them ergonomically, and evolve the schema safely.",
        features: &[
            Feat { name: "One struct, three roles", summary: "The same plain struct is your model, your form, and your serializer — declare data once, reuse it everywhere, no DTOs to keep in sync.", status: SH, maturity: STA },
            Feat { name: "Model derive", summary: "#[derive(Model)] turns a struct into a table, manager, and column set.", status: SH, maturity: STA },
            Feat { name: "QuerySet builder", summary: "filter/exclude/order_by/annotate/aggregate, Q objects, subqueries.", status: SH, maturity: STA },
            Feat { name: "Relations", summary: "ForeignKey, OneToOne, M2M with select_related / prefetch_related.", status: SH, maturity: STA },
            Feat { name: "Managed migrations", summary: "makemigrations diffs models; migrate applies reversible operations.", status: SH, maturity: STA },
            Feat { name: "Soft deletes & signals", summary: "deleted_at auto-filtering and pre/post save/delete hooks.", status: SH, maturity: STA },
        ],
    },
    Category {
        name: "Web & Templates",
        slug: "web-templates",
        description: "Routing, handlers, and server-rendered templates with secure defaults.",
        features: &[
            Feat { name: "Routing & extractors", summary: "axum-based routes, typed extractors, layered middleware.", status: SH, maturity: STA },
            Feat { name: "minijinja templates", summary: "Auto-escaped templates with a markdown filter and plugin dirs.", status: SH, maturity: STA },
            Feat { name: "Forms & validation", summary: "Form derive, field validation, friendly per-field errors.", status: US, maturity: BETA },
            Feat { name: "File & image fields", summary: "Multipart upload to a pluggable Storage backend.", status: SH, maturity: STA },
            Feat { name: "Live reload (dev)", summary: "Save a template, CSS, or asset and the browser refreshes itself over SSE — CSS hot-swaps in place, no manual refresh. Opt-in umbral-livereload plugin, inert in production.", status: SH, maturity: BETA },
        ],
    },
    Category {
        name: "Admin",
        slug: "admin",
        description: "An auto-generated control panel for every model.",
        features: &[
            Feat { name: "Auto CRUD", summary: "List, create, edit, delete for every registered model.", status: SH, maturity: STA },
            Feat { name: "Search, filters & pickers", summary: "Multi-filter dialog, search, FK/M2M/O2O relation pickers.", status: SH, maturity: STA },
            Feat { name: "Dashboard widgets", summary: "KPI cards, charts, donuts, gauges, and tables on the index.", status: IP, maturity: BETA },
            Feat { name: "Bulk actions & inlines", summary: "Act on selected rows; edit related rows on the parent form.", status: PL, maturity: DES },
        ],
    },
    Category {
        name: "Auth & Security",
        slug: "auth-security",
        description: "Users, permissions, sessions, and secure-by-default middleware.",
        features: &[
            Feat { name: "Users & permissions", summary: "Argon2 hashing, groups, RBAC, per-object checks.", status: SH, maturity: STA },
            Feat { name: "Sessions", summary: "Server-side session store + middleware.", status: SH, maturity: STA },
            Feat { name: "OAuth / social login", summary: "Google/GitHub login + account connection (umbral-oauth).", status: SH, maturity: BETA },
            Feat { name: "CSRF, HSTS, headers", summary: "Secure-by-default protections via umbral-security.", status: SH, maturity: STA },
        ],
    },
    Category {
        name: "REST & API",
        slug: "rest-api",
        description: "Expose models as JSON, document them, and try them in a playground.",
        features: &[
            Feat { name: "Auto REST", summary: "Serializers, viewsets, pagination, filtering, auth gates.", status: SH, maturity: BETA },
            Feat { name: "OpenAPI + playground", summary: "Generated spec and a mini-Postman request surface.", status: SH, maturity: BETA },
            Feat { name: "Nested writable serializers", summary: "Create a parent and its children in one request.", status: PL, maturity: DES },
        ],
    },
    Category {
        name: "Background & Realtime",
        slug: "background-realtime",
        description: "Move work off the request path and push updates to clients.",
        features: &[
            Feat { name: "Task queue", summary: "#[task] jobs drained by a worker, with retries.", status: EX, maturity: ALPHA },
            Feat { name: "Health checks", summary: "/healthz and /ready probes for load balancers.", status: SH, maturity: STA },
            Feat { name: "WebSockets / SSE", summary: "User- and room-targeted realtime push.", status: PL, maturity: DES },
            Feat { name: "Email sending", summary: "Transactional email via SMTP / API backends.", status: PL, maturity: DES },
        ],
    },
];

/// Seed the catalog. Idempotent and **self-healing**: each category and
/// feature is get-or-created by slug, so adding a new entry to `CATALOG`
/// surfaces it on the next boot without re-inserting existing rows or
/// needing a DB wipe. Returns `(categories, features)` newly inserted.
pub async fn seed() -> Result<(usize, usize), Box<dyn std::error::Error + Send + Sync>> {
    use crate::models::{feature_category, framework_feature};

    let mut cats = 0;
    let mut feats = 0;
    for (ci, cat) in CATALOG.iter().enumerate() {
        // Get-or-create the category by slug.
        let category = match FeatureCategory::objects()
            .filter(feature_category::SLUG.eq(cat.slug))
            .first()
            .await?
        {
            Some(existing) => existing,
            None => {
                let now = Utc::now();
                let created = FeatureCategory::objects()
                    .create(FeatureCategory {
                        id: 0,
                        name: cat.name.to_string(),
                        slug: cat.slug.to_string(),
                        description: Some(cat.description.to_string()),
                        display_order: (ci as i32) * 10,
                        visible: true,
                        created_at: now,
                        updated_at: now,
                        deleted_at: None,
                    })
                    .await?;
                cats += 1;
                created
            }
        };

        for (fi, f) in cat.features.iter().enumerate() {
            let slug = format!("{}-{}", cat.slug, slugify(f.name));
            // Skip features that already exist (by their unique slug).
            if FrameworkFeature::objects()
                .filter(framework_feature::SLUG.eq(slug.as_str()))
                .exists()
                .await?
            {
                continue;
            }
            let now = Utc::now();
            FrameworkFeature::objects()
                .create(FrameworkFeature {
                    id: 0,
                    category: ForeignKey::new(category.id),
                    name: f.name.to_string(),
                    slug,
                    short_summary: f.summary.to_string(),
                    full_description: f.summary.to_string(),
                    status: f.status,
                    maturity: f.maturity,
                    docs_url: None,
                    example_url: None,
                    related_plugin_slug: None,
                    release_target: None,
                    display_order: (fi as i32) * 10,
                    visible: true,
                    metadata: None,
                    created_at: now,
                    updated_at: now,
                    deleted_at: None,
                })
                .await?;
            feats += 1;
        }
    }
    Ok((cats, feats))
}
