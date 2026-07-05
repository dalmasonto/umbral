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
    BlogPost, BlogPostKind, ChangelogEntry, ChangelogKind, PublishStatus, blog_post,
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
        excerpt: "Rust has fast routers and solid ORMs. What it didn't have was the batteries-included feeling: declare your data and get migrations, an admin, forms, and a REST API almost for free. Umbral is that feeling, rebuilt on Rust's guarantees.",
        kind: BlogPostKind::DesignNote,
        reading_minutes: 7,
        featured: true,
        body: r#"Rust already has excellent web building blocks. `axum` is a great router, `sqlx` is a great database layer, `serde` is a great serializer. What it didn't have, until now, is what makes a framework productive: one where you **declare your data once** and get migrations, CRUD, an admin, forms, and an optional REST API almost for free.

Umbral ('of the shadow', from Latin *umbra*, shadow) is a deliberate attempt to bring that batteries-included feeling to Rust's compile-time guarantees.

![The Umbral plugin directory, built with Umbral](/media/91b8f829-2e2e-4ecb-8c53-4e2fec4a7028-adem_preview2-720x405.jpg)

## The one idea that matters most

**Thin core, plugin-heavy.** The framework dogfoods its own plugin system. Auth, sessions, admin, tasks, and REST are all plugins. Structurally they are identical to a third-party one. A REST-free app compiles and runs with zero serializer code. If a built-in can't be expressed as a plugin, the plugin contract is wrong.

That single constraint shapes everything. There is no privileged "core" path that the built-ins get to use and you don't.

## Declare a model, get a migration

The everyday loop works from the first milestone that has models:

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

A plugin (an "app") can contribute any subset of:

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

That's the entire pitch: a productive, batteries-included app framework where no capability is privileged, and the one you want to replace is always replaceable.
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
    Seed {
        slug: "we-tried-to-break-our-own-framework",
        title: "The week we tried to break our own framework",
        excerpt: "A batteries-included framework makes a quiet promise: the easy path is the safe path. So we spent a week attacking our own defaults — and shipped a stack of fixes. Here's what 'secure by default' actually costs, and why it's the feature you never see.",
        kind: BlogPostKind::DesignNote,
        reading_minutes: 8,
        featured: true,
        body: r#"Every framework that says "batteries included" is really making a promise about *defaults*. The whole pitch — declare your data and get an admin, a REST API, forms, migrations — only holds up if the thing you get for free is also the thing that's safe. The moment the convenient path and the correct path diverge, "batteries included" becomes "footguns included."

So one week, we stopped adding features and tried to break Umbral instead. We read every plugin as if we were the attacker, not the author. The rule was simple: assume nothing, and treat every "this is probably fine" as a bug until proven otherwise.

It was not a comfortable week. But it was the most important one.

## The uncomfortable questions

Good security review is mostly a list of rude questions you'd rather not ask about your own code.

*"What happens if two users hit this at the same time?"* Our signal system ran subscriber callbacks while holding a global lock — so one slow audit-log handler could quietly throttle every write in the whole process, and a handler that re-entered the API would deadlock it forever. We now clone the handler list, drop the lock, and run handlers free.

*"Whose IP is this, really?"* Throttles and logs keyed on `X-Forwarded-For` — a header any client can forge. Behind a reverse proxy that's fine; directly exposed it's a rate-limit bypass and a way to frame another user. So we made the framework ask you how many proxies it should trust, and resolve the real client from there. If you configure a per-IP throttle without a trusted proxy, it now tells you at boot instead of silently sharing one bucket across the whole internet.

*"Can I read a row that isn't mine?"* A REST API that serves every row to anyone who can guess an ID is the single most common web vulnerability there is. We added object-level scoping — `.scope()` and `.owned_by()` — so list and detail endpoints only ever return the rows a caller is allowed to see. And uploaded files, which used to be world-readable by URL, got an access-control hook that runs *before a single byte is served*.

*"What ends up in the logs?"* Signals fanned out the entire row to every subscriber — password hashes, tokens, PII — which an innocent audit-log subscriber would then dutifully copy into permanent storage. Now a field marked `#[umbral(signal_skip)]` never leaves the building.

## The defaults nobody thinks about

Some of the best fixes were the ones that change *nothing* on the happy path and everything under stress.

Argon2 password hashing is deliberately expensive — that's the point. But a flood of logins could spawn hundreds of those hashes at once and simply eat all the memory on the box. We put a concurrency gate in front of it: peak memory is now bounded, and past a threshold the server sheds load with a 503 instead of falling over. You never notice it. An attacker does.

Sessions could live forever if you used them once a fortnight. Now you can set an absolute maximum age and a `SameSite` policy, so "stay logged in" has an outer limit. And on Postgres, tenant isolation is enforced by the database itself with `FORCE` row-level security and a per-request context that can't leak from one request into the next — the last line of defence lives below your handler, where a coding mistake can't reach it.

## Even the migrations

Security isn't only the request path. Deploys are where quiet disasters happen.

Two app replicas deploying at once used to race the same schema change; one would win and the other would abort mid-migration. Now a Postgres advisory lock serializes them — one migrates, the rest wait and skip. A migration that would drop a table (because you deleted one line of model registration) no longer runs on a `makemigrations && migrate` reflex; it stops and makes you type `--allow-destructive`. And tightening a column to `NOT NULL` now backfills the existing nulls instead of failing halfway through against real data.

## Why this is a feature

Here's the thing about all of this: if we did it right, you will never see any of it. There's no dashboard for "the OOM that didn't happen" or "the row you didn't leak." Secure-by-default is the rarest kind of feature — the one whose entire job is to be invisible.

But it's also the reason a batteries-included framework is worth using at all. The value was never that you *can* build auth, or throttling, or multi-tenancy. You can build those anywhere. The value is that you get them already thought through — including the 3 a.m. edge cases you'd never have time to chase on a deadline.

We tried to break our own framework for a week. What we actually did was write down all the hard questions once, so you don't have to ask them every time you ship.
"#,
    },
    Seed {
        slug: "ship-a-saas-this-weekend-in-rust",
        title: "Ship a SaaS this weekend, in Rust, without the boilerplate",
        excerpt: "You have an idea and two free days. Here's how a batteries-included Rust framework takes you from an empty repo to a real multi-tenant app — auth, an admin, a REST API, background jobs — before Sunday night. No DTOs, no glue code, no yak-shaving.",
        kind: BlogPostKind::Tutorial,
        reading_minutes: 9,
        featured: true,
        body: r#"It's Friday evening. You have an idea you can't shake and a weekend with nothing on it. The idea is a small SaaS — a dashboard your customers log into, each seeing only their own data. Nothing exotic. The kind of thing you've built before and remember mostly as *boilerplate*: wiring auth, hand-rolling an admin, writing the same CRUD endpoints, gluing a job queue on.

This time you reach for Umbral, a batteries-included framework for Rust — think the productivity of Django or Rails, on top of Rust's compile-time guarantees. Here's how the weekend goes.

## Friday night: declare your data

You start a project and an app, then write the one thing that actually matters — your model.

```rust
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Project {
    pub id: i64,
    pub owner: ForeignKey<AuthUser>,
    #[umbral(max_length = 120)]
    pub name: String,
    pub status: ProjectStatus,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
}
```

That's not just a table. It's your migration, your admin screen, your form, and — if you want it — your REST resource. You declare the shape of your data once, and every other system reads that same declaration. No DTOs to keep in sync, no serializer that drifts from the model.

Two commands and the schema is real:

```bash
cargo run -- makemigrations   # diffs your models, writes the migration
cargo run -- migrate          # applies it
```

You go to bed having written a struct and gotten a database.

## Saturday morning: the parts you'd normally dread

You wire up the plugins. This is the part that usually eats a Saturday; here it eats a coffee.

```rust
App::builder()
    .plugin(AuthPlugin::<AuthUser>::default().with_default_routes())
    .plugin(SessionsPlugin::default())
    .plugin(OAuthPlugin::new(base).provider(google).provider(github))
    .plugin(AdminPlugin::default().site_title("Acme".into()))
    .plugin(RestPlugin::default())
    .build()?;
```

**Auth** gives you a user model, argon2 hashing, and login/logout routes. **OAuth** adds "Sign in with Google/GitHub" — and reads its keys from the environment, so a provider with no credentials just isn't registered. **Admin** generates a full control panel for *every* model you declared: list views, search, filters, relation pickers, the works. **REST** turns those same models into a JSON API, safe-by-default (writes are refused until you grant permission).

By lunch you have a login page, a social login button, an admin you didn't design, and an API you didn't write. You spent your effort on the model, and everything else fell out of it.

## Saturday afternoon: the multi-tenant part

Your whole idea hinges on isolation — customer A must never see customer B's projects. This is the part that keeps people up at night, so you let the framework carry it.

You scope every REST endpoint to the caller's own rows:

```rust
RestPlugin::default()
    .resource(ResourceConfig::for_::<Project>().owned_by("owner"))
```

Now a list request only ever returns the projects that belong to whoever's asking. For defence in depth on Postgres, you turn on row-level security so the *database itself* enforces the boundary — even a bug in your handler can't read across it. Multi-tenancy, the thing you were dreading, is a couple of lines and a plugin.

## Sunday: the moving parts

Real apps do work off the request path. A customer clicks "export," and you don't want them staring at a spinner while you build a PDF.

```rust
#[task]
async fn build_export(project_id: i64) -> Result<(), TaskError> {
    // ... slow work, retried on failure ...
    Ok(())
}
```

Enqueue it from a handler, run `cargo run -- worker`, and it drains in the background with retries. On Postgres the queue uses `FOR UPDATE SKIP LOCKED`, so you can run ten workers and each grabs a *different* job instead of fighting over the same one — the queue scales sideways for free.

Then you sprinkle in the finishing touches: file uploads for avatars through a storage backend that's local in dev and S3 in prod with the same code; a live toast when something happens, pushed over SSE; a `/healthz` endpoint so your deploy target stops guessing whether you're up.

## Sunday night

You didn't build a framework this weekend. You built *your app* — and the framework quietly handled auth, the admin, the API, migrations, isolation, and background work, each as an opt-in plugin you could swap or drop.

The batteries-included promise was never "you can't do this yourself." Of course you can. The promise is that you shouldn't have to spend a weekend on the parts that are the same in every app — so you can spend it on the part that's only in yours.

Ship it. It's still Sunday.
"#,
    },
    Seed {
        slug: "twenty-one-plugins-one-contract",
        title: "Twenty-one plugins, one contract: a tour of the Umbral toolbox",
        excerpt: "Auth, admin, REST, background jobs, realtime, storage, multi-tenancy — in Umbral they're all plugins, structurally identical to one you'd write yourself. Here's the whole toolbox, and why 'it's just a plugin' is the most important sentence in the framework.",
        kind: BlogPostKind::PluginSpotlight,
        reading_minutes: 8,
        featured: false,
        body: r#"Most frameworks have a *core* and then some *extensions*. The core gets special privileges — hooks the extensions can't reach, a fast path only the built-ins get to use. Extensions are second-class citizens, and you feel it the first time you try to build something the authors didn't anticipate.

Umbral made a different bet, and it's the bet that shapes everything else: **the core is thin, and everything else is a plugin — including the batteries.** Auth, sessions, the admin, REST, the task queue: structurally, each is identical to a plugin you'd write yourself. There is no privileged path. If a built-in couldn't be expressed as a plugin, that would be a bug in the plugin contract, not a reason to cheat.

Cargo enforces this for us. `umbral-core` doesn't depend on the REST plugin — so "serializers are a plugin" isn't a slogan, it's a fact the compiler won't let us break. A REST-free app compiles with zero serializer code in the binary.

Here's the toolbox that contract produced.

## The parts you reach for first

**umbral-admin** turns every model into a control panel — list views, search, combinable filters, relation pickers, dashboards. **umbral-auth** and **umbral-permissions** give you users, groups, argon2 hashing, and role-based access the admin and API already understand. **umbral-sessions** keeps the identity around; **umbral-oauth** adds "sign in with Google/GitHub" and account connection.

Declare a model, mount these four, and you have a login page and an admin before you've written a route.

## The parts that make it an API

**umbral-rest** turns the same models into JSON resources — serializers, viewsets, pagination, filtering — safe-by-default, with object-level scoping so nobody reads across a boundary. **umbral-openapi** documents them and mounts a Swagger UI, and **umbral-playground** drops a mini-Postman right into your app so you can share an endpoint with a frontend teammate without anyone installing anything.

## The parts that move work off the request

**umbral-tasks** is a database-backed job queue — define work with `#[task]`, enqueue it, drain it with a worker, scale horizontally with `SKIP LOCKED`. **umbral-realtime** pushes updates to the browser over SSE or WebSockets, targeted at a single user or a room, with connection and rate caps so no one client can flood you. **umbral-email** sends the transactional mail your reset flows need, and **umbral-cache** memoises the expensive stuff.

## The parts that keep you safe and sane in production

**umbral-security** ships CSRF, HSTS, and clickjacking protection on by default. **umbral-rls** pushes tenant isolation into Postgres itself, and **umbral-tenants** routes each customer to their own schema and binds the tenant to the caller. **umbral-storage** serves both your static assets and user uploads through one pluggable backend — filesystem in dev, S3 in prod. **umbral-health** answers the probes your load balancer asks for. **umbral-logs** logs the real client IP, and **umbral-analytics** captures product events without dragging down the request path.

## The parts you only notice when they're gone

**umbral-livereload** refreshes your browser the instant you save a template or CSS — inert in production. **umbral-signals** lets you hang audit logs, cache-busting, and notifications off your data without touching the write code.

## Why "it's just a plugin" matters to you

Count them and it's twenty-one first-party plugins. But the number isn't the point. The point is that *your* plugin sits at exactly the same table. The extension point that powers the admin is the one you use to add your billing integration. The signal the framework fires on save is the one your code subscribes to. There's no inside track you're locked out of.

That's the real batteries-included promise: not a fixed menu of features, but a toolbox where the tools you build are indistinguishable from the ones that came in the box. Browse the whole set on the [plugin directory](/plugins) — and then go write the twenty-second.
"#,
    },
];

/// Seed the blog posts. Idempotent AND self-healing: each post is get-or-created
/// by its unique slug, so adding a new entry to `POSTS` publishes it on the next
/// boot without re-inserting or clobbering the posts already there (an admin's
/// later edit is safe). Returns the number of posts newly published.
pub async fn seed() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let now = Utc::now();
    let mut n = 0;
    for s in POSTS {
        if BlogPost::objects()
            .filter(blog_post::SLUG.eq(s.slug))
            .exists()
            .await?
        {
            continue;
        }
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
            // SEO: an explicit <title> plus the excerpt as the meta description.
            seo_title: Some(s.title.to_string()),
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
