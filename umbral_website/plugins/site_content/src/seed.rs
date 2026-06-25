//! Idempotent seed for the blog (`/blog`).
//!
//! Five lengthy, markdown-authored posts about Umbral so the blog renders
//! real long-form content — release-style notes, design notes, and
//! tutorials. The bodies are Markdown (rendered with `| markdown` on the
//! detail page), exercising the custom code-copy + image-lightbox
//! enhancers shipped with this fix.
//!
//! Seeding runs on `on_ready` (see `SiteContentPlugin`), so the user's
//! dev server picks the posts up on its next reload — no `cargo run`
//! needed. Idempotent: short-circuits once any blog post exists.

use chrono::Utc;
use uuid::Uuid;

use crate::models::{
    BlogPost, BlogPostKind, ChangelogEntry, ChangelogKind, PublishStatus,
};

struct Seed {
    slug: &'static str,
    title: &'static str,
    excerpt: &'static str,
    kind: BlogPostKind,
    reading_minutes: i32,
    featured: bool,
    body: &'static str,
}

const POSTS: &[Seed] = &[
    Seed {
        slug: "why-umbral-exists",
        title: "Why Umbral exists",
        excerpt: "Rust has fast routers and solid ORMs. What it didn't have was the Django feeling — declare your data and get migrations, an admin, forms, and a REST API almost for free. Umbral is that feeling, rebuilt on Rust's guarantees.",
        kind: BlogPostKind::DesignNote,
        reading_minutes: 7,
        featured: true,
        body: r#"Rust already has excellent web building blocks. `axum` is a great router, `sqlx` is a great database layer, `serde` is a great serializer. What it didn't have — until now — is the thing that made Django productive: a framework where you **declare your data once** and get migrations, CRUD, an admin, forms, and an optional REST API almost for free.

Umbral ('of the shadow', from Latin *umbra*, shadow — it lives in Django's shadow in shape, not in code) is a deliberate attempt to recreate that feeling on top of Rust's compile-time guarantees.

![The Umbral plugin directory, built with Umbral](/media/91b8f829-2e2e-4ecb-8c53-4e2fec4a7028-adem_preview2-720x405.jpg)

## The one idea that matters most

**Thin core, plugin-heavy.** The framework dogfoods its own plugin system. Auth, sessions, admin, tasks, and REST are all plugins. Structurally they are identical to a third-party one. A REST-free app compiles and runs with zero serializer code. If a built-in can't be expressed as a plugin, the plugin contract is wrong.

That single constraint shapes everything. There is no privileged "core" path that the built-ins get to use and you don't.

## Declare a model, get a migration

The everyday loop works from the first milestone that has models — exactly like Django:

```rust
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(plugin = "blog")]
pub struct Post {
    pub id: i64,
    #[umbral(unique, max_length = 160)]
    pub slug: String,
    pub title: String,
    #[umbral(widget = "markdown")]
    pub body: String,
    #[umbral(auto_now_add)]
    pub created_at: chrono::DateTime<chrono::Utc>,
}
```

Then the two commands that *are* the product:

```bash
cargo run -- makemigrations   # autodetect the diff, write a migration
cargo run -- migrate          # apply pending migrations
```

Change the model, run them again, and the autodetector emits the right `ALTER`/`DROP`. This **declare → migrate → change → migrate** cycle isn't a later feature — it's the north star.

## The type system does the work

A nullable column becomes `Option<T>`. Errors are `Result` values that flow through `?`. SQL is always parameterized. The easy path is the safe path, enforced by the compiler rather than by a linter or a code review checklist.

## Secure by default

CSRF protection, clickjacking and HSTS headers, template auto-escaping, and always-parameterized SQL ship on by default. A field declares which backends it supports, and a boot-time system check fails loudly on an incompatible field rather than at 3am in production.

Umbral is pre-alpha, and honest about it. But the shape is settled, and the everyday loop already works. That's why this whole website is built with it.
"#,
    },
    Seed {
        slug: "building-the-umbral-website-with-umbral",
        title: "Building the Umbral website with Umbral",
        excerpt: "The best proof that a framework can build a real, content-heavy site is to build one. This site — directory, reviews, discussions, admin, forms — is an Umbral project using the same apps, migrations, and plugins it documents.",
        kind: BlogPostKind::Community,
        reading_minutes: 8,
        featured: true,
        body: r#"There's a temptation, when you build a web framework, to ship a thin marketing page and call it a day. We did the opposite: **this website is a full Umbral project**, and the important sections prove the framework rather than describe it.

## Apps, not a monolith

The site is scaffolded with `umbral startapp` into focused plugin crates, each owning its own models and migrations:

```text
umbral_website/plugins/
  site_content/      # blog, pages, FAQ, navigation, contact messages
  features/          # the framework feature catalog
  plugin_directory/  # community + official plugins, comments, submissions
  reviews/           # developer reviews of the framework
  showcase/          # sites built with Umbral
  security_reports/  # malicious-plugin reporting + advisories
  community/         # social links + newsletter config
  public/            # the landing page
```

`main.rs` stays small. It wires settings, plugins, routes, and startup — and contains **no website data models**. Every model lives in its app's `models.rs`.

## Everything is database-backed

The homepage stat strip, the plugin cards, the reviews, the discussion notes — all live. When the database is empty, the page renders an honest em-dash (`—`) or an empty state, never a fabricated `0` or a made-up testimonial.

```rust
// The homepage's curated reviews — pulled live, featured first.
let reviews = reviews::featured_reviews(2).await.unwrap_or_default();
```

## Forms are real forms

Contact, plugin submission, plugin reports, plugin comments, and reviews all use Umbral Forms: server-side validation, friendly errors, and a clear moderation state where it matters. Submitted content doesn't appear publicly until it's approved.

## The admin maintains it

Maintainers manage features, plugins, comments, blog posts, social links, reviews, showcase entries, and security reports from the admin — without touching templates. The dashboard widgets read straight from the directory data.

## Real-time, where it helps

Post a note on a plugin page and other readers see a live banner over SSE — no refresh. The comment data model is designed so a future SSE/WebSocket layer can stream replies and moderation changes without a redesign.

Building the site this way kept us honest. Every rough edge in the framework showed up immediately, because we were the first serious user.
"#,
    },
    Seed {
        slug: "managed-migrations-the-loop-that-is-the-product",
        title: "Managed migrations: the loop that is the product",
        excerpt: "Autodetection diffs your models against the last snapshot and emits ordered, reversible operations. inspectdb ports an existing database in. Here's how the declare → migrate → change → migrate loop actually works.",
        kind: BlogPostKind::Tutorial,
        reading_minutes: 9,
        featured: false,
        body: r#"Migrations are where a lot of Rust web projects quietly fall back to hand-written SQL. Umbral treats the managed loop as the product, not an add-on.

## The loop

1. Declare or change a model. An autodetected migration is generated.
2. `migrate` applies all pending migrations to the database.
3. Update or delete a model. The diff produces the right `ALTER`/`DROP`.

```bash
# 1. you changed a model — autodetect and write the migration
cargo run -- makemigrations

# 2. review the generated file (it's plain, readable JSON), then apply
cargo run -- migrate
```

## Autodetection

The autodetector diffs your current models against the last migration snapshot and emits ordered, reversible operations: create/alter/drop table, add/alter/drop column. The common cases — a new model, a dropped model, an added or removed field — work on day one. The genuinely hard cases (rename vs. drop+add disambiguation, data-preserving alters) get surfaced rather than guessed.

## Existing rows are the test, not an obstacle

A rule we hold hard: **never wipe the database to bypass a migration.** If a `UNIQUE` addition trips a duplicate, or a new `NOT NULL` column needs a backfill default, that failure is the bug you want to find. Deleting the database to "get a clean run" just hides it until production.

```text
umbral makemigrations: rename detected (column-shape match): `body` → `content`
```

## Porting an existing database

Already have a schema? `inspectdb` introspects it and generates models that feed straight back into the same managed loop:

```bash
cargo run -- inspectdb > src/models.rs
```

From there you're in the normal declare → migrate cycle, with the full audit trail of how each column got its shape. Migration history is the schema's record — we never delete entries to "regenerate cleanly," because that makes older deploys un-migratable.

## Cross-plugin foreign keys

Each plugin owns its own migrations. `migrate` walks every registered plugin, collects their migrations, orders them by a dependency graph — cross-plugin foreign keys included — and runs only those not yet recorded in the tracking table. The built-in auth, sessions, and tasks tables are created this exact way. Nothing is special-cased.
"#,
    },
    Seed {
        slug: "the-plugin-contract-batteries-as-real-plugins",
        title: "The plugin contract: batteries as real plugins",
        excerpt: "Dependencies point inward toward the core; control flows outward through a trait. That single rule is what lets you keep the official auth, swap it for your SSO, or build a project-specific plugin — all behind the same boundary.",
        kind: BlogPostKind::DesignNote,
        reading_minutes: 8,
        featured: true,
        body: r#"Umbral's strongest public argument is also its simplest internal rule: **the batteries are real plugins.** The official auth, sessions, admin, tasks, and REST implement the exact same `Plugin` trait a community developer would.

## Dependency inversion is the whole game

> Dependencies point inward toward core. Control flows outward through the trait.

- Every plugin depends on the `umbral` facade, never the reverse.
- `umbral-core` defines the `Plugin` trait but never names a concrete plugin — it touches plugins only as `Box<dyn Plugin>`.
- `umbral-core` depends on neither `umbral-rest` nor `umbral-openapi`. That's the structural proof that "serializers are a plugin." Cargo's ban on circular dependencies enforces it for us.

## What a plugin can contribute

A plugin (Django's "app") can contribute any subset of:

- models (which become migrations)
- routes and views
- middleware
- management commands
- a typed settings schema with defaults
- admin registrations
- lifecycle hooks (`on_ready()` is the Rust version of `AppConfig.ready()`)

## Wired in one line

Adding a plugin to your app is a single builder call — no registry edits, no glue module to maintain:

```rust
App::builder()
    .plugin(AuthPlugin::<AuthUser>::default())
    .plugin(SessionsPlugin::default())
    .plugin(AdminPlugin::default())
    .plugin(RestPlugin::default())
    .plugin(MyOwnPlugin::default())
    .build()?;
```

## Swap a battery without a rewrite

Because the boundary is identical first-party or third, you can swap the default auth for your SSO, drop in a community GraphQL plugin, or build a project-specific plugin with `startapp` — and the admin, the ORM, and the rest of your app keep working unchanged.

```toml
# enable batteries — they're just plugins
umbral-admin = "0.1"
umbral-rest  = "0.1"
acme-graphql = "0.3"   # a community plugin, same contract
```

That's the entire pitch: a productive, Django-shaped app framework where no capability is privileged, and the one you want to replace is always replaceable.
"#,
    },
    Seed {
        slug: "forms-admin-and-rest-without-the-boilerplate",
        title: "Forms, admin, and REST without the boilerplate",
        excerpt: "One model declaration drives the form, the admin CRUD, and the REST resource. Here's how the same struct powers server-rendered forms with validation, an auto admin, and a JSON API — each an opt-in plugin.",
        kind: BlogPostKind::Tutorial,
        reading_minutes: 7,
        featured: false,
        body: r#"The payoff of declaring your data once is that several systems can read that declaration. In Umbral, the same model struct can drive a validated form, an admin CRUD screen, and a REST resource — each one an opt-in plugin.

## One struct, three surfaces

```rust
#[derive(Debug, Clone, Default, sqlx::FromRow, Serialize, Deserialize, Model, umbral::forms::Form)]
#[umbral(plugin = "directory", display = "Plugins", icon = "package")]
pub struct Plugin {
    pub id: i64,

    #[umbral(unique, max_length = 120)]
    #[form(required, length(min = 2, max = 120))]
    pub name: String,

    #[form(required, length(min = 10, max = 400))]
    pub short_description: String,

    // server-managed: skipped by the form, stamped by the admin
    #[umbral(noform, choices, default = "pending")]
    pub moderation: PluginModeration,
}
```

## Forms

The `Form` derive turns the `#[form(...)]` attributes into server-side validation with friendly errors. Fields marked `#[umbral(noform)]` never appear on the public form — a visitor can't set their own moderation status.

```rust
let plugin = Plugin::validate(&submitted_data).await?;
```

## Admin

Register the model and the admin gives you a CRUD UI — list, filters, create, edit — with the `#[umbral(noform)]` fields editable by staff. The dashboard can add widgets that read straight from the model.

## REST

Expose the same model as a JSON resource, hiding sensitive columns, in one line:

```rust
RestPlugin::default()
    .resource(ResourceConfig::for_::<AuthUser>().hide(["password_hash"]))
```

Pair it with the OpenAPI plugin and you get a schema and a request playground for free.

## The ORM is the single interface

Every row-level read or write goes through the ORM — never hand-rolled `sqlx::query("...")` in plugin code. The ORM knows the backend and emits the right SQL, so one path works on Postgres and SQLite alike:

```rust
let pending = Plugin::objects()
    .filter(plugin::MODERATION.eq("pending"))
    .order_by(plugin::CREATED_AT.desc())
    .fetch()
    .await?;
```

Declare once, and the form, the admin, the API, and your queries all read the same truth. That's the boilerplate you didn't write.
"#,
    },
];

/// Seed the blog posts. Idempotent: short-circuits if any post exists.
pub async fn seed() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    if BlogPost::objects().count().await? > 0 {
        return Ok(0);
    }
    let now = Utc::now();
    let mut n = 0;
    for s in POSTS {
        let post = BlogPost {
            id: 0,
            public_id: Uuid::new_v4(),
            slug: s.slug.to_string(),
            title: s.title.to_string(),
            excerpt: Some(s.excerpt.to_string()),
            body: s.body.to_string(),
            status: PublishStatus::Published,
            kind: s.kind,
            author: None,
            category: None,
            tags: Default::default(),
            cover_image_url: None,
            attachment_url: None,
            seo_title: None,
            seo_description: Some(s.excerpt.to_string()),
            reading_minutes: s.reading_minutes,
            view_count: 0,
            featured: s.featured,
            published_at: Some(now),
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };
        BlogPost::objects().create(post).await?;
        n += 1;
    }
    Ok(n)
}

/// Seed the changelog entries (the shipped v0.0.1 row + the roadmap row).
/// Idempotent: short-circuits once any entry exists. Runs on `on_ready`
/// alongside the blog seed.
pub async fn seed_changelog() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    if ChangelogEntry::objects().count().await? > 0 {
        return Ok(0);
    }
    let now = Utc::now();
    let released = chrono::DateTime::parse_from_rfc3339("2026-06-10T00:00:00+00:00")
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or(now);

    let entries = [
        ChangelogEntry {
            id: 0,
            version: "v0.0.1".to_string(),
            title: "The core loop, end to end".to_string(),
            body: "- ORM with managed migrations — declare → migrate → change → migrate.\n\
                   - Auto admin: CRUD, search, multi-filters, FK/M2M/O2O pickers, permissions, dashboard widgets.\n\
                   - REST + OpenAPI + an interactive playground; auth-gated resources.\n\
                   - Auth, sessions, OAuth (Google/GitHub) + account connection.\n\
                   - Secure-by-default: CSRF, HSTS, clickjacking headers, auto-escaping.\n\
                   - File/image fields with pluggable storage; soft deletes; signals; fixtures."
                .to_string(),
            kind: ChangelogKind::Released,
            current: true,
            released_at: Some(released),
            display_order: 0,
            published: true,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        },
        ChangelogEntry {
            id: 0,
            version: "toward v0.1".to_string(),
            title: "On the road to v0.1".to_string(),
            body: "- Email sending (SMTP / API backends) and a hardened background task queue.\n\
                   - WebSockets / SSE for realtime push — user- and room-targeted.\n\
                   - REST nested writable serializers; CSV / Excel import-export.\n\
                   - A testing & factory library for models and pages.\n\
                   - Caching, rate limiting, structured logging, and metrics."
                .to_string(),
            kind: ChangelogKind::Roadmap,
            current: false,
            released_at: None,
            display_order: 1,
            published: true,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        },
    ];

    let mut n = 0;
    for e in entries {
        ChangelogEntry::objects().create(e).await?;
        n += 1;
    }
    Ok(n)
}
