//! Seed data for the plugin directory.
//!
//! Populates the first-party Umbral plugin rows so the public
//! landing page (`plugins/public`) can render the plugin map from
//! the database instead of falling back to the static table in
//! `home.html`.
//!
//! Idempotent AND self-healing: `seed_official_plugins` get-or-creates each
//! row by slug (never re-inserting), and `backfill_plugin_content` re-asserts
//! the curated `full_content` / `setup_notes` on already-seeded rows so a
//! copy edit ships on the next boot without a DB wipe. Manual full re-seed:
//! `DELETE FROM plugin_directory_plugin;` then trigger `on_ready` again.

use crate::models::{
    CommentKind, CommentModeration, Plugin, PluginComment, PluginFeature, PluginMaturity,
    PluginModeration, PluginSource, PluginStatus, plugin, plugin_feature,
};
use chrono::Utc;
use umbral::prelude::*;

/// One row of official Umbral plugin data. Hand-curated; the spec
/// for the landing page (`planning/umbral-site.md` §"Plugin map")
/// is the source of truth.
///
/// `full_content` is Markdown — it renders through `| markdown` into the
/// `.pd-prose` "About" card on the detail page, so it carries the full
/// write-up (a story hook, an install line, an `App::builder()` wiring
/// preview, a targeted usage snippet, and a what-you-get list). `setup_notes`
/// is the one-line intro shown above the "Usage" terminal (`usage_intro`).
struct OfficialRow {
    crate_name: &'static str,
    name: &'static str,
    slug: &'static str,
    author: &'static str,
    short_description: &'static str,
    full_content: &'static str,
    setup_notes: &'static str,
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
        full_content: r#"You declared your models to get a database. The admin is the receipt: mount one plugin and every model in every plugin you've installed grows a full back office — list views, search, combinable filters, relation pickers, bulk actions, an audit trail, and a dashboard you can shape with your own widgets. No scaffolding command, no generated files to maintain. It reads the same model metadata the ORM and migrations already use, so the admin is never out of sync with your schema.

## Install

```bash
umbral add umbral-admin
```

## Wire it up

Mount `AdminPlugin` **after** the plugins whose models you want to manage — it discovers everything registered before it.

```rust
use umbral::prelude::*;
use umbral_admin::AdminPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(BlogPlugin::default())   // your models
    .plugin(
        AdminPlugin::default()
            .site_title("Acme Admin".into()),
    )
    .build()?;
```

Visit `/admin` and sign in — every registered model is already there.

## Target: a dashboard, not just tables

Group KPI cards, charts, and tables into named sections, and add bespoke analytics pages with `.view(...)`.

```rust
use umbral_admin::{AdminPlugin, WidgetSection};

AdminPlugin::default()
    .site_title("Acme".into())
    .dashboard_section(
        WidgetSection::new("Overview")
            .subtitle("Headline counts")
            .widget(total_orders_card())
            .widget(revenue_chart()),
    )
    .view(reports_view());  // a custom page at /admin/custom-views/reports/
```

## What you get

- List / create / edit / delete for every registered model
- Toolbar search plus combinable `list_filter` facets
- Async FK / M2M / O2O pickers with search-as-you-type
- Per-action `view / add / change / delete` gating (via umbral-permissions)
- File & image widgets, Markdown / rich-text field widgets
- Soft-delete + trash, bulk actions, and a per-row audit log
"#,
        setup_notes: "Mount `AdminPlugin` after the plugins whose models you want to manage — it discovers every model registered before it and builds the CRUD UI. Give it a title, and optionally dashboard widgets and custom views.",
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
        full_content: r#"Every app hits the same wall on day two: who is this request, and are they allowed? Umbral Auth answers it out of the box — a battle-tested `AuthUser` model, groups, argon2 password hashing with sane defaults, opaque DB-backed API tokens, and password-reset flows. Its `LoggedIn<T>` extractor turns "is the caller authenticated?" into a function argument the compiler enforces: a handler that takes `LoggedIn<AuthUser>` simply cannot run for an anonymous request.

## Install

```bash
umbral add umbral-auth
umbral add umbral-sessions   # auth needs a place to keep the session
```

## Wire it up

```rust
use umbral::prelude::*;
use umbral_auth::{AuthPlugin, AuthUser};
use umbral_sessions::SessionsPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(SessionsPlugin::default())
    .plugin(
        AuthPlugin::<AuthUser>::default()
            .with_default_routes()        // /login, /logout, /signup
            .with_user_in_templates(),    // `user` available in every template
    )
    .build()?;
```

## Target: lock a route to signed-in users

The extractor is the gate. If it can't build an authenticated user, the handler never runs.

```rust
use umbral_auth::{LoggedIn, AuthUser};

async fn dashboard(user: LoggedIn<AuthUser>) -> Html<String> {
    Html(format!("Welcome back, {}", user.username))
}
```

## What you get

- `AuthUser` + groups/roles, ready to migrate
- Argon2 password hashing (drop-in from bcrypt without surprises)
- Opaque, hashed-at-rest bearer tokens for API clients
- `LoggedIn<T>` / `login_required_html("/login")` guards
- Token-based password reset (email delivery via umbral-email)
- Pairs with umbral-oauth for Google / GitHub social login
"#,
        setup_notes: "Pair with `SessionsPlugin` (auth stores its session there). `with_default_routes()` mounts /login, /logout and /signup; `with_user_in_templates()` injects `user` into every template so your base layout's nav can branch on `user.is_authenticated`.",
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
        full_content: r#"HTTP forgets you the moment the response is sent. Sessions are how your app remembers — the signed-in user, a half-filled cart, a flash message that should survive one redirect. Umbral Sessions gives you a server-side store (persisted through the ORM, so no extra infrastructure to stand up) plus the cookie middleware that ties each browser to its row, with secure defaults already switched on.

## Install

```bash
umbral add umbral-sessions
```

## Wire it up

Add it before anything that reads the session — auth, in particular.

```rust
use umbral::prelude::*;
use umbral_sessions::SessionsPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(SessionsPlugin::default())
    .plugin(AuthPlugin::<AuthUser>::default())  // reads the session
    .build()?;
```

## Target: stash something across requests

The session is a typed key/value bag that outlives a single request.

```rust
use umbral_sessions::Session;

async fn add_to_cart(session: Session) {
    let mut cart: Vec<i64> = session.get("cart").await.unwrap_or_default();
    cart.push(product_id);
    session.insert("cart", &cart).await;
}
```

## What you get

- DB-backed session store — no Redis required to start
- Cookie middleware with secure, HttpOnly defaults
- The foundation umbral-auth's login / logout builds on
- Redis-backed store on the roadmap for horizontal scaling
"#,
        setup_notes: "Register `SessionsPlugin` before any plugin that reads the session (auth reads it on every request). The default store persists sessions through the ORM, so there's nothing extra to deploy.",
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
        full_content: r#"The same models that power your admin can become a JSON API without a second data layer. Umbral REST is the DRF-equivalent: register a resource and get list / create / retrieve / update / delete, pagination, filtering, search, and an auth + permission chain — all wired to the model you already declared. And it's safe by default: a resource with no explicit permission is read-only, so a stray `POST` returns 403 until you opt writes in.

## Install

```bash
umbral add umbral-rest
```

## Wire it up

```rust
use umbral::prelude::*;
use umbral_rest::{RestPlugin, ResourceConfig};

let app = App::builder()
    .database("default", pool)
    .plugin(
        RestPlugin::default()
            // Expose a model, hiding a sensitive column from the wire.
            .resource(ResourceConfig::for_::<AuthUser>().hide(["password_hash"])),
    )
    .build()?;
```

`GET /api/` lists every resource; `GET /api/authuser/` returns a paginated page.

## Target: writable, but only for the right callers

Flip a resource to writable and gate it behind a permission — the default stays read-only for everything you don't opt in.

```rust
use umbral_rest::{ResourceConfig, permissions::IsAuthenticated};

RestPlugin::default()
    .resource(
        ResourceConfig::for_::<Article>()
            .default_permission(IsAuthenticated),  // anonymous reads, members write
    );
```

## What you get

- Model → JSON resource with zero serializer code
- Pagination, query-string filters, and free-text search
- Session / bearer auth chain with per-resource permission gates
- `GET /api/` discovery root listing resources and endpoints
- Custom `@action` endpoints beyond CRUD
- Safe by default: writes 403 until a permission opts them in; list capped
"#,
        setup_notes: "Register a `ResourceConfig::for_::<Model>()` per model you want on the wire. Resources are read-only until you attach a write permission via `.default_permission(...)`, so a bare `POST` is a 403, not an open door.",
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
        full_content: r#"An API nobody can read is an API nobody adopts. Umbral OpenAPI reads your registered REST resources and emits an OpenAPI 3 schema — FK targets, enums, nullable and read-only fields and all — then mounts an interactive explorer so a frontend teammate can try endpoints in the browser instead of guessing from a wiki page. The docs are generated from the resources themselves, so they can't drift from the API.

## Install

```bash
umbral add umbral-openapi   # depends on umbral-rest
```

## Wire it up

Add it after `RestPlugin`; it introspects whatever resources REST registered.

```rust
use umbral::prelude::*;
use umbral_rest::RestPlugin;
use umbral_openapi::OpenApiPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(RestPlugin::default().resource(/* ... */))
    .plugin(OpenApiPlugin::new())
    .build()?;
```

The generated spec is served for tooling, and the Swagger explorer mounts alongside it.

## Target: share a live API with a frontend dev

Point them at the explorer — they get every route, its parameters, and a "try it" button, no Postman collection to maintain.

## What you get

- OpenAPI 3 schema generated from registered resources
- FK targets, enums, and nullable / read-only surfaced in the schema
- Interactive Swagger UI for click-to-try requests
- `securitySchemes` published so the playground can auto-detect auth
- Pairs with umbral-playground for an in-app request console
"#,
        setup_notes: "Add `OpenApiPlugin::new()` after `RestPlugin` — it introspects the resources REST already registered, so there's nothing to annotate by hand. The schema and Swagger UI update themselves as you add resources.",
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
        full_content: r#"Some work shouldn't happen inside the request — sending the welcome email, resizing the upload, calling the flaky third-party webhook. Umbral Tasks is the Celery-equivalent: annotate an async function with `#[task]`, enqueue it from a handler, and a separate worker drains the queue durably. The queue is a DB table, so a crash doesn't lose jobs, and retries back off exponentially instead of hammering a service that's already down.

## Install

```bash
umbral add umbral-tasks
```

## Wire it up

```rust
use umbral::prelude::*;
use umbral_tasks::TasksPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(TasksPlugin::default())
    .build()?;
```

Run the worker beside your web process:

```bash
cargo run -- worker        # drains the queue
cargo run -- tasks-beat    # fires periodic / cron schedules
```

## Target: offload the welcome email

Define the job once, enqueue it from anywhere, and let the worker deliver it.

```rust
use umbral_tasks::{enqueue, EnqueueOptions};

#[umbral::task]
async fn send_welcome(payload: WelcomePayload) -> Result<(), String> {
    email_user(payload.user_id).await.map_err(|e| e.to_string())
}

// From a signup handler:
register_send_welcome();  // once at startup
enqueue("send_welcome", &WelcomePayload { user_id }, EnqueueOptions::default()).await?;
```

## What you get

- `#[task]` macro turns an async fn into an enqueueable job
- DB-backed queue that survives a process crash
- Retries with exponential backoff + per-task timeouts
- Priority queues and future `eta` / `delay` scheduling
- Periodic "beat" tasks (cron or fixed interval)
- Read-only queue browser in the admin, with "Retry selected"
"#,
        setup_notes: "Register `TasksPlugin` in the app, annotate handlers with `#[umbral::task]`, and call `register_<fn>()` at startup so the worker knows them. Enqueue with `enqueue(...)` from any handler, then run `cargo run -- worker` (and `tasks-beat` for schedules) beside the web process.",
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
        full_content: r#"The security features you forget to add are the ones that bite you. Umbral Security ships them on by default — CSRF protection on every POST, HSTS, clickjacking headers, and template auto-escaping — so a fresh app is hardened before you write a line of security code. When a default is genuinely too tight (a JSON API that authenticates by bearer token, say), the escape hatches are explicit and narrow, not a global off switch.

## Install

```bash
umbral add umbral-security
```

## Wire it up

The defaults need no configuration — just mount it.

```rust
use umbral::prelude::*;
use umbral_security::SecurityPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(SecurityPlugin::new())
    .build()?;
```

## Target: exempt a token-authed API from CSRF

CSRF protects cookie-authenticated form posts; a bearer-token API doesn't need it. Exempt the prefix without weakening the rest of the site.

```rust
use umbral_security::{SecurityPlugin, SecurityConfig};

SecurityPlugin::with_config(SecurityConfig {
    csrf_exempt_paths: vec!["/api".into()],
    ..Default::default()
})
.with_hsts(true);
```

## What you get

- Double-submit CSRF token enforced on every POST
- HSTS + a full set of secure response headers
- Clickjacking protection (X-Frame-Options / frame-ancestors)
- Template output auto-escaped by default; opt out explicitly
- Per-path escape hatches instead of a global kill switch
"#,
        setup_notes: "Mount `SecurityPlugin::new()` for hardened defaults, or `SecurityPlugin::with_config(...)` to narrow a single default — e.g. `csrf_exempt_paths` for a bearer-token API. Exemptions are per-path, never a global off switch.",
        installation_commands: "umbral-security = { path = \"../plugins/umbral-security\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 70,
    },
    OfficialRow {
        crate_name: "umbral-storage",
        name: "Umbral Storage",
        slug: "umbral-storage",
        author: "Umbral contributors",
        short_description: "static assets + user uploads, one trait",
        full_content: r#"One plugin, two jobs that every app needs and nobody wants to hand-roll: serving your compiled static assets, and storing user uploads. Both go through a single pluggable `Storage` trait — the local filesystem in dev, S3 in prod, the *same handler code* either way. Declare an `ImageField` or `FileField` on a model and you get multipart upload, streaming size caps, background processing behind a concurrency gate, and an access-control hook so private files aren't world-readable by guessing the URL.

## Install

```bash
umbral add umbral-storage
```

## Wire it up

```rust
use umbral::prelude::*;
use umbral_storage::StoragePlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(
        StoragePlugin::new()
            .static_files("/static", "./static")  // compiled CSS/JS
            .media("/media", "./media"),          // user uploads
    )
    .build()?;
```

## Target: let users upload an avatar

Add the field; the admin and forms render an upload widget with a thumbnail, and the file is stored through whichever backend is configured.

```rust
#[derive(Model)]
struct Profile {
    #[umbral(primary_key)]
    id: i64,
    avatar: Option<ImageField>,  // multipart upload, thumbnailed
}
```

## What you get

- One `Storage` trait: local filesystem in dev, S3 in prod, same code
- `FileField` / `ImageField` model fields with upload widgets
- Streaming size caps so a huge upload can't exhaust memory
- Background image processing behind a concurrency gate
- Access-control hook so private uploads aren't public by URL
- Static-asset serving in production (no nginx required to start)
"#,
        setup_notes: "One `StoragePlugin` serves both sides: `.static_files(url, dir)` for compiled assets and `.media(url, dir)` for user uploads. The same `File`/`Image` model fields work against the filesystem in dev and S3 in prod — no handler change.",
        installation_commands: "umbral-storage = { path = \"../plugins/umbral-storage\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 80,
    },
    OfficialRow {
        crate_name: "umbral-permissions",
        name: "Umbral Permissions",
        slug: "umbral-permissions",
        author: "Umbral contributors",
        short_description: "RBAC, groups, per-object checks",
        full_content: r#""Can this user do this?" is a question the admin and REST both ask on every request — so answer it once, in a language they already speak. Umbral Permissions is role-based access control wired straight into the framework: groups, per-model `view / add / change / delete` permissions, and per-object ownership checks for "you may edit *your* posts, not everyone's". A deactivated account is denied at the permission layer, not just bounced from the login form, so disabling someone actually disables them.

## Install

```bash
umbral add umbral-permissions
```

## Wire it up

On boot it walks every registered model and provisions the four standard permissions for each.

```rust
use umbral::prelude::*;
use umbral_permissions::PermissionsPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(BlogPlugin::default())          // models to protect
    .plugin(PermissionsPlugin::default())   // provisions their permissions
    .build()?;
```

## Target: editors can publish, authors only draft

Put users in groups, grant the group the model permissions it needs, and both the admin and the REST API enforce it automatically.

## What you get

- Groups + per-model `view / add / change / delete` permissions
- Per-object ownership checks ("edit your own rows only")
- The same checks the admin and umbral-rest already consult
- Deactivated accounts denied at the permission layer, not just login
- Auto-provisioned permissions for every registered model
"#,
        setup_notes: "Mount `PermissionsPlugin::default()` after your model plugins — on boot it provisions the four standard permissions (view/add/change/delete) for every registered model. The admin and umbral-rest consult these checks automatically.",
        installation_commands: "umbral-permissions = { path = \"../plugins/umbral-permissions\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 25,
    },
    OfficialRow {
        crate_name: "umbral-oauth",
        name: "Umbral OAuth",
        slug: "umbral-oauth",
        author: "Umbral contributors",
        short_description: "Google / GitHub social login",
        full_content: r#""Sign in with Google" removes the single biggest signup drop-off: the password. Umbral OAuth drops social login in without the callback-URL-and-token-exchange boilerplate — register a provider, and the login route, the redirect dance, and account linking are handled. Credentials come from the environment, so a provider with no keys simply isn't registered: you can leave the wiring in place with nothing configured and it stays inert until you add the keys.

## Install

```bash
umbral add umbral-oauth   # builds on umbral-auth
```

## Wire it up

Read the keys from the environment and register each provider only when both halves are present.

```rust
use umbral::prelude::*;
use umbral_oauth::OAuthPlugin;
use umbral_oauth::providers::{GoogleProvider, GitHubProvider};

let app = App::builder()
    .database("default", pool)
    .plugin(
        OAuthPlugin::new("https://acme.dev")   // public origin for callbacks
            .login_redirect("/dashboard")
            .provider(GoogleProvider::new(google_id, google_secret))
            .provider(GitHubProvider::new(github_id, github_secret)),
    )
    .build()?;
```

## Target: let an existing user connect GitHub

Beyond first-time login, a signed-in user can link a provider to their existing account, so they can use either path next time.

## What you get

- Google and GitHub providers, callback flow handled
- Connect a provider to an already-signed-in account
- Credentials read from env — unconfigured providers just don't register
- Safe to leave wired with nothing set; inert until keys appear
- Builds on umbral-auth's user model and session
"#,
        setup_notes: "Give `OAuthPlugin::new(base)` your public origin (for callback URLs), then `.provider(...)` each provider. Read client id/secret from the environment and register a provider only when both are present — an unconfigured provider is simply skipped.",
        installation_commands: "umbral-oauth = { path = \"../plugins/umbral-oauth\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Beta,
        featured: true,
        display_order: 90,
    },
    OfficialRow {
        crate_name: "umbral-realtime",
        name: "Umbral Realtime",
        slug: "umbral-realtime",
        author: "Umbral contributors",
        short_description: "SSE / WebSocket push, user- and room-targeted",
        full_content: r#"Polling is the sound of a UI that doesn't trust the server to tell it anything. Umbral Realtime pushes instead: target a single user or a whole room, and let an ORM `post_save` signal fan a change out to everyone watching — a new comment appears, a dashboard counter ticks, a moderation queue lights up — without the browser asking "anything yet?" every two seconds. A default connection cap and per-connection message-rate cap keep one client from flooding the server.

## Install

```bash
umbral add umbral-realtime
```

## Wire it up

Hang a handler off a model's lifecycle and push when it fires.

```rust
use umbral::prelude::*;
use umbral_realtime::{RealtimePlugin, ModelEvent, ModelAction};

let app = App::builder()
    .database("default", pool)
    .plugin(
        RealtimePlugin::default()
            .on_model::<Comment, _, _>(|ev: ModelEvent| async move {
                if ev.action == ModelAction::Created {
                    // notify watchers of this thread
                }
            }),
    )
    .build()?;
```

Browsers subscribe over SSE at `/realtime/sse`.

## Target: a live, superuser-only notification

Target by user id (never a public group) and only superusers ever receive the event — a normal visitor's connection is never sent it.

```rust
use umbral_realtime::Realtime;

Realtime::to_user(admin_id.to_string())
    .send("admin_notification", &payload).await;
```

## What you get

- Push over SSE (WebSocket on the roadmap)
- Target a single user *or* a named room
- ORM `post_save` signals fan out changes automatically
- Default connection cap + per-connection message-rate cap
- User-targeted events = private channels with no group to guess
"#,
        setup_notes: "Register `RealtimePlugin::default()` and hang `.on_model::<T, _, _>(...)` handlers off the models whose changes should push. Browsers connect at `/realtime/sse`; send to a room or to a specific user id with `Realtime::to_user(...)`.",
        installation_commands: "umbral-realtime = { path = \"../plugins/umbral-realtime\" }",
        version: "0.1.0",
        status: PluginStatus::Usable,
        maturity: PluginMaturity::Beta,
        featured: false,
        display_order: 100,
    },
    OfficialRow {
        crate_name: "umbral-cache",
        name: "Umbral Cache",
        slug: "umbral-cache",
        author: "Umbral contributors",
        short_description: "process-wide cache + page caching",
        full_content: r#"The fastest query is the one you don't run twice. Umbral Cache installs a process-wide in-memory cache as an ambient handle — reach for it from any handler to memoise an expensive aggregate, a rendered fragment, or a third-party API response. When a whole page is safe to share, an opt-in `cache_page` layer caches the entire response. No client to configure, no keys to manage in dev.

## Install

```bash
umbral add umbral-cache
```

## Wire it up

```rust
use umbral::prelude::*;
use umbral_cache::{CachePlugin, Cache};

let app = App::builder()
    .database("default", pool)
    .plugin(CachePlugin::new(Cache::memory()))
    .build()?;
```

## Target: memoise an expensive dashboard query

Read from the ambient handle, compute on a miss, store for next time.

```rust
use umbral_cache::ambient;

async fn stats() -> Stats {
    if let Some(hit) = ambient().get::<Stats>("dashboard:stats").await {
        return hit;
    }
    let fresh = compute_expensive_stats().await;   // the slow path, once
    ambient().set("dashboard:stats", &fresh, /* ttl */ 60).await;
    fresh
}
```

## What you get

- Process-wide in-memory cache as an ambient handle
- Reach it from any handler — no wiring through call stacks
- Opt-in `cache_page` layer for whole-response caching
- A pluggable backend so a shared store can slot in later
"#,
        setup_notes: "Install `CachePlugin::new(Cache::memory())` to register the ambient cache handle, then reach it from any handler with `umbral_cache::ambient()`. Layer `cache_page` only on responses that are safe to share (anonymous, non-per-user).",
        installation_commands: "umbral-cache = { path = \"../plugins/umbral-cache\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 110,
    },
    OfficialRow {
        crate_name: "umbral-health",
        name: "Umbral Health",
        slug: "umbral-health",
        author: "Umbral contributors",
        short_description: "/healthz + /ready probes",
        full_content: r#"Every load balancer and orchestrator asks the same two questions: are you alive, and are you ready for traffic? Umbral Health answers both with zero config — a liveness probe and a readiness probe that actually checks the database is reachable, not just that the process is running. Mount the plugin and your deploy target stops guessing whether to route to this instance.

## Install

```bash
umbral add umbral-health
```

## Wire it up

```rust
use umbral::prelude::*;
use umbral_health::HealthPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(HealthPlugin::default())   // /healthz + /ready
    .build()?;
```

## Target: gate readiness on a dependency

Add your own check — a cache ping, a queue connection — and `/ready` only reports green when it passes.

```rust
use umbral_health::HealthPlugin;
use std::time::Duration;

HealthPlugin::default()
    .check(RedisPing)                       // your HealthCheck impl
    .check_timeout(Duration::from_secs(2)); // fail fast, don't hang the probe
```

## What you get

- `/healthz` liveness probe (is the process up?)
- `/ready` readiness probe that verifies the DB is reachable
- Pluggable custom `HealthCheck`s for your own dependencies
- Per-check timeouts so a hung dependency doesn't hang the probe
- Zero config to start — mount and deploy
"#,
        setup_notes: "Mount `HealthPlugin::default()` for `/healthz` (liveness) and `/ready` (readiness, which verifies the DB). Add `.check(...)` for extra dependencies and `.check_timeout(...)` so a hung dependency fails the probe fast instead of hanging it.",
        installation_commands: "umbral-health = { path = \"../plugins/umbral-health\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 120,
    },
    OfficialRow {
        crate_name: "umbral-livereload",
        name: "Umbral Live Reload",
        slug: "umbral-livereload",
        author: "Umbral contributors",
        short_description: "dev browser reload over SSE",
        full_content: r#"The edit-save-alt-tab-refresh loop is a tax you pay hundreds of times a day. Umbral Live Reload deletes it: save a template, some CSS, or an asset and the browser refreshes itself — and CSS hot-swaps in place without even a full reload. A file watcher pushes events over SSE and the client script injects itself into HTML responses. It's completely inert in production, so there's nothing to strip before you ship.

## Install

```bash
umbral add umbral-livereload
```

## Wire it up

Point it at the directories you edit.

```rust
use umbral::prelude::*;
use umbral_livereload::LiveReloadPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(LiveReloadPlugin::new().watch("plugins"))  // watch plugin templates too
    .build()?;
```

## Target: a monorepo with per-plugin templates

`.watch(dir)` adds a directory to the watch set, so a framework built out of plugins reloads on edits anywhere in the tree, not just the top-level templates.

## What you get

- Auto browser refresh on template / asset save
- CSS hot-swap in place — no full reload for a style tweak
- SSE-driven; the client script auto-injects into HTML
- Add extra watch roots with `.watch(dir)`
- Fully inert outside dev mode — nothing to remove for prod
"#,
        setup_notes: "Add `LiveReloadPlugin::new()` and chain `.watch(dir)` for each extra directory to watch (the site templates + static are watched by default). It's automatically inert outside dev mode, so it's safe to leave wired in.",
        installation_commands: "umbral-livereload = { path = \"../plugins/umbral-livereload\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Beta,
        featured: false,
        display_order: 130,
    },
    OfficialRow {
        crate_name: "umbral-analytics",
        name: "Umbral Analytics",
        slug: "umbral-analytics",
        author: "Umbral contributors",
        short_description: "product analytics (PostHog)",
        full_content: r#"You can't improve what you can't see. Umbral Analytics auto-captures pageviews and lets you fire custom product events to PostHog, so "which features do people actually use?" stops being a guess. The outbound sends are bounded — an analytics burst can't fan out unbounded connections and take the request path down with it — so instrumenting your app never becomes the reason it falls over.

## Install

```bash
umbral add umbral-analytics
```

## Wire it up

Read the key from the environment and exclude paths you don't want counted.

```rust
use umbral::prelude::*;
use umbral_analytics::AnalyticsPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(
        AnalyticsPlugin::from_env()                  // UMBRAL_ANALYTICS_* keys
            .with_exclude_prefixes(vec!["/static".into(), "/healthz".into()]),
    )
    .build()?;
```

## Target: track a conversion, not just a pageview

Auto-capture gives you traffic; a custom event gives you the funnel step that matters.

## What you get

- Auto-captured pageviews to PostHog
- Custom product events for the funnel steps you care about
- Path-prefix exclusions (skip `/static`, health checks, etc.)
- Bounded outbound sends — a burst can't exhaust connections
- Key from env via `from_env()`, or explicit with `new(api_key)`
"#,
        setup_notes: "Configure with `AnalyticsPlugin::from_env()` (reads the PostHog key from the environment) or `AnalyticsPlugin::new(api_key)`, and `.with_exclude_prefixes(...)` to skip assets and probes. Outbound sends are bounded so instrumentation can't overwhelm the request path.",
        installation_commands: "umbral-analytics = { path = \"../plugins/umbral-analytics\" }",
        version: "0.1.0",
        status: PluginStatus::Usable,
        maturity: PluginMaturity::Beta,
        featured: false,
        display_order: 140,
    },
    OfficialRow {
        crate_name: "umbral-email",
        name: "Umbral Email",
        slug: "umbral-email",
        author: "Umbral contributors",
        short_description: "transactional email, SMTP / API",
        full_content: r#"Password resets, email verification, receipts — half your flows dead-end without somewhere to send mail. Umbral Email gives them that destination behind one interface: compose a message, call send, and whether it goes out over SMTP or a provider API is a config choice, not a rewrite. In dev it falls back to a console backend that prints to stderr, so you can build the whole flow before you own a single API key.

## Install

```bash
umbral add umbral-email
```

## Wire it up

```rust
use umbral::prelude::*;
use umbral_email::EmailPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(EmailPlugin)   // backend selected from the environment
    .build()?;
```

## Target: send a verification email

Build the message and send it — the configured backend does the rest.

```rust
use umbral_email::EmailMessage;

let msg = EmailMessage::new("Verify your email", vec![user.email.clone()])
    .from("noreply@acme.dev")
    .text_body(&format!("Confirm: https://acme.dev/verify/{token}"));

msg.send().await?;   // SMTP, API, or console — same call
```

## What you get

- One `send` interface over SMTP, provider API, or console
- Swap providers with a config change, not a code change
- Console backend in dev — build the flow before you have keys
- Attachments and HTML / text bodies
- The delivery layer umbral-auth's password reset plugs into
"#,
        setup_notes: "Register `EmailPlugin`; the backend (SMTP, provider API, or the dev console) is chosen from the environment. Compose with `EmailMessage::new(subject, recipients)` and `.send().await` — swapping providers is a config change, never a code change.",
        installation_commands: "umbral-email = { path = \"../plugins/umbral-email\" }",
        version: "0.1.0",
        status: PluginStatus::Usable,
        maturity: PluginMaturity::Beta,
        featured: false,
        display_order: 150,
    },
    OfficialRow {
        crate_name: "umbral-logs",
        name: "Umbral Logs",
        slug: "umbral-logs",
        author: "Umbral contributors",
        short_description: "structured request logging",
        full_content: r#"When something breaks at 3am, the difference between a five-minute fix and a two-hour hunt is the log line. Umbral Logs gives you structured, per-request logging with the *real* client IP resolved from your trusted-proxy setup — so behind nginx you log the caller, not the proxy, and never a header an attacker can forge to poison your logs. Sampling and status filters keep the volume sane on a busy service.

## Install

```bash
umbral add umbral-logs
```

## Wire it up

```rust
use umbral::prelude::*;
use umbral_logs::LogsPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(
        LogsPlugin::default()
            .exclude_prefix("/static")   // don't log asset traffic
            .min_status(400),            // only capture errors on this route set
    )
    .build()?;
```

## Target: sample a high-traffic endpoint

On an endpoint doing thousands of req/s you don't want every line — sample a fraction and keep the signal without the cost.

```rust
LogsPlugin::default().sample_rate(0.05);  // log 5% of requests
```

## What you get

- Structured, per-request log lines
- Real client IP from your trusted-proxy config (not a forgeable header)
- Exclude noisy prefixes (`/static`, health checks)
- Sample rate + minimum-status filters to control volume
- Pairs with the framework's trusted-proxy client-IP resolution
"#,
        setup_notes: "Mount `LogsPlugin::default()` and tune it: `.exclude_prefix(...)` to drop asset noise, `.min_status(n)` to capture only errors, and `.sample_rate(f)` to thin a high-traffic route. The client IP comes from your trusted-proxy config, so it's the caller — never a forgeable header.",
        installation_commands: "umbral-logs = { path = \"../plugins/umbral-logs\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 160,
    },
    OfficialRow {
        crate_name: "umbral-signals",
        name: "Umbral Signals",
        slug: "umbral-signals",
        author: "Umbral contributors",
        short_description: "pub/sub lifecycle hooks",
        full_content: r#"Some reactions don't belong in the code that triggers them. When an order is placed you might want to bust a cache, write an audit line, and ping a channel — but none of that should clutter the "save the order" path. Umbral Signals lets you hang behaviour off events without touching the write code: subscribe to pre/post save/update/delete, or emit your own named events, and react elsewhere. Handlers run outside the registry lock, so a slow subscriber can't throttle every write.

## Install

```bash
umbral add umbral-signals
```

## Wire it up

```rust
use umbral::prelude::*;
use umbral_signals::SignalsPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(SignalsPlugin)
    .build()?;
```

## Target: react to a domain event

Emit a named event where it happens; subscribe wherever the reaction lives.

```rust
use umbral_signals::{emit, subscribe_async};

// at startup:
subscribe_async("order_placed", |payload| async move {
    let order_id = payload["id"].as_i64().unwrap_or(0);
    bust_cache(order_id).await;
});

// in the handler:
emit("order_placed", serde_json::json!({ "id": order.id })).await;
```

For work that must survive a crash, have the handler enqueue an `umbral-tasks` job.

## What you get

- Pre/post save/update/delete model lifecycle signals
- Emit and subscribe to your own named events
- Handlers run outside the registry lock — slow ones don't block writes
- `#[umbral(signal_skip)]` keeps secrets / PII out of the payload
- Pair with umbral-tasks for durable, crash-safe reactions
"#,
        setup_notes: "Register `SignalsPlugin` and, at startup, `subscribe_async(\"event\", handler)`; emit with `emit(\"event\", json).await` or let the ORM's post_save/update/delete signals fire automatically. Signals are in-process — pair with umbral-tasks when a reaction must survive a crash.",
        installation_commands: "umbral-signals = { path = \"../plugins/umbral-signals\" }",
        version: "0.1.0",
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        featured: false,
        display_order: 170,
    },
    OfficialRow {
        crate_name: "umbral-rls",
        name: "Umbral RLS",
        slug: "umbral-rls",
        author: "Umbral contributors",
        short_description: "Postgres row-level security",
        full_content: r#"The safest tenant isolation is the kind your application code can't accidentally forget. Umbral RLS pushes it down into Postgres itself: `FORCE` row-level security with a per-request GUC set through the connection pool, so one request's tenant context can never leak into another's — even a buggy handler that omits a `WHERE tenant_id = ...` is caught by the database. It's the last line of defence, below your code, where a forgotten filter turns into zero rows instead of someone else's data.

## Install

```bash
umbral add umbral-rls
```

*Postgres only — RLS is a Postgres feature. On SQLite the plugin skips with a warning rather than silently diverging.*

## Wire it up

Declare a policy per protected table; the plugin sets the per-request context on the pool.

```rust
use umbral::prelude::*;
use umbral_rls::RlsPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(
        RlsPlugin::new()
            .policy("invoice", "tenant_id = current_setting('app.tenant')::bigint"),
    )
    .build()?;
```

## Target: defence in depth for a multi-tenant SaaS

Pair it with umbral-tenants: tenants routes and scopes the request; RLS makes the database enforce the boundary even if the routing layer has a bug.

## What you get

- `FORCE` row-level security so table owners aren't exempt
- Per-request GUC set through the pool — no cross-request leak
- Policies declared in Rust, applied as migrations
- The database enforces isolation even when a handler forgets
- Pairs with umbral-tenants for layered multi-tenancy
"#,
        setup_notes: "Postgres only. Declare `.policy(table, sql_predicate)` per protected table on `RlsPlugin::new()`; the plugin sets a per-request GUC through the pool so a tenant's context can't leak across requests. On SQLite it skips with a warning rather than diverging.",
        installation_commands: "umbral-rls = { path = \"../plugins/umbral-rls\" }",
        version: "0.1.0",
        status: PluginStatus::Usable,
        maturity: PluginMaturity::Beta,
        featured: false,
        display_order: 180,
    },
    OfficialRow {
        crate_name: "umbral-tenants",
        name: "Umbral Tenants",
        slug: "umbral-tenants",
        author: "Umbral contributors",
        short_description: "multi-tenancy (schema- or row-per-tenant)",
        full_content: r#"Turn one app into a multi-tenant SaaS without forking it per customer. Umbral Tenants routes each tenant to its own Postgres schema (or scopes them by a tenant column), resolves the active tenant from the request, and binds it to the caller through a membership check so nobody reads across the wall. Pick the isolation strategy that fits — a schema per tenant for hard separation, a shared table for density — and the framework handles the routing.

## Install

```bash
umbral add umbral-tenants
```

## Wire it up

Choose which apps are tenant-scoped, the isolation strategy, and how the tenant is identified.

```rust
use umbral::prelude::*;
use umbral_tenants::{TenantsPlugin, TenantStrategy};

let app = App::builder()
    .database("default", pool)
    .plugin(
        TenantsPlugin::new()
            .strategy(TenantStrategy::Schema)   // one DB, a schema per tenant
            .tenant_apps(["billing", "projects"])
            .tenant_header("X-Tenant"),
    )
    .build()?;
```

## Target: hard isolation with a safety net

Run `TenantStrategy::Schema` for separation and layer umbral-rls underneath, so the database enforces the tenant boundary even if request routing has a bug.

## What you get

- Schema-per-tenant *or* row-per-tenant (shared column) strategies
- Tenant resolved from the request (header or your own resolver)
- Membership binding so a caller can't read another tenant's data
- Choose which apps are tenant-scoped and which stay shared
- Pairs with umbral-rls for database-enforced defence in depth
"#,
        setup_notes: "On `TenantsPlugin::new()` pick a `.strategy(...)` (schema-per-tenant or shared-column), list the `.tenant_apps([...])` that are scoped, and set how the tenant is resolved (`.tenant_header(...)` or a membership guard). Layer umbral-rls for database-enforced isolation.",
        installation_commands: "umbral-tenants = { path = \"../plugins/umbral-tenants\" }",
        version: "0.1.0",
        status: PluginStatus::Usable,
        maturity: PluginMaturity::Beta,
        featured: false,
        display_order: 190,
    },
    OfficialRow {
        crate_name: "umbral-playground",
        name: "Umbral Playground",
        slug: "umbral-playground",
        author: "Umbral contributors",
        short_description: "in-app API playground",
        full_content: r#"A mini-Postman baked right into your app. Umbral Playground lets anyone browse your REST resources, build a request, send it, and read the response — no external tool, no copy-pasting curl commands into a terminal. It's the fastest way to hand an API to a frontend teammate: point them at a URL and they're exploring live endpoints in seconds, against the real running server.

## Install

```bash
umbral add umbral-playground   # sits alongside umbral-rest / umbral-openapi
```

## Wire it up

Give it a title and the path to mount at.

```rust
use umbral::prelude::*;
use umbral_playground::PlaygroundPlugin;

let app = App::builder()
    .database("default", pool)
    .plugin(RestPlugin::default().resource(/* ... */))
    .plugin(OpenApiPlugin::new())
    .plugin(PlaygroundPlugin::new("Acme API").at("/api/playground/"))
    .build()?;
```

## Target: onboard a frontend dev in one link

Instead of maintaining a Postman collection, send the playground URL — it discovers resources from the running server, so it's never stale.

## What you get

- Browse resources, build and send requests in the browser
- Read live responses against the real server
- Auth auto-detected from the published OpenAPI securitySchemes
- Mount anywhere with `.at("/path/")`
- Zero external tooling to install or keep in sync
"#,
        setup_notes: "Mount `PlaygroundPlugin::new(title).at(\"/path/\")` alongside umbral-rest and umbral-openapi — it discovers resources from the running server and auto-detects auth from the published securitySchemes, so the console is never out of date.",
        installation_commands: "umbral-playground = { path = \"../plugins/umbral-playground\" }",
        version: "0.1.0",
        status: PluginStatus::Usable,
        maturity: PluginMaturity::Beta,
        featured: false,
        display_order: 200,
    },
];

/// Idempotent AND self-healing. Get-or-creates each official plugin by slug, so
/// adding a new entry to `OFFICIAL` surfaces it on the next boot without
/// re-inserting the rows already there or needing a DB wipe. Returns the number
/// of rows newly inserted.
///
/// Content updates to an *existing* row don't happen here (this short-circuits
/// on a present slug) — [`backfill_plugin_content`] re-asserts `full_content` /
/// `setup_notes` on already-seeded rows.
pub async fn seed_official_plugins() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let mut inserted = 0;
    for row in OFFICIAL {
        // Skip a plugin that already exists (by its unique slug) — never clobber
        // an admin's later hand-edit here.
        if Plugin::objects()
            .filter(plugin::SLUG.eq(row.slug))
            .exists()
            .await?
        {
            continue;
        }
        let mut p = Plugin::default();
        p.name = row.name.to_string();
        p.slug = row.slug.to_string();
        p.crate_name = row.crate_name.to_string();
        p.author = row.author.to_string();
        p.short_description = row.short_description.to_string();
        p.full_content = row.full_content.to_string();
        p.setup_notes = Some(row.setup_notes.to_string());
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

    // umbral-static was merged into umbral-storage. If an older seed already
    // inserted a `umbral-static` row, retire it (mark Deprecated) so the
    // directory shows one current storage plugin, not a stale pair. Idempotent:
    // only touches a still-non-deprecated row.
    let mut dep = serde_json::Map::new();
    dep.insert(
        "status".to_string(),
        serde_json::Value::String("deprecated".to_string()),
    );
    Plugin::objects()
        .filter(plugin::SLUG.eq("umbral-static"))
        .filter(plugin::STATUS.ne("deprecated"))
        .update_values(dep)
        .await?;

    Ok(inserted)
}

/// Re-assert the curated Markdown `full_content` and `setup_notes` on
/// already-seeded official rows. The row insert in [`seed_official_plugins`]
/// short-circuits once a slug exists, so a copy edit to `OFFICIAL` would never
/// reach the live rows without this — same pattern as [`backfill_audit_status`].
///
/// Only writes a row whose stored content actually differs, so this is a no-op
/// once the DB matches the seed (no write amplification, quiet logs on a normal
/// boot). `full_content` / `setup_notes` are seed-owned editorial copy for the
/// first-party plugins; re-asserting them is intentional. Returns the number of
/// rows updated.
pub async fn backfill_plugin_content() -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let mut updated = 0;
    for row in OFFICIAL {
        let Some(existing) = Plugin::objects()
            .filter(plugin::SLUG.eq(row.slug))
            .first()
            .await?
        else {
            continue;
        };
        let content_matches = existing.full_content == row.full_content;
        let notes_match = existing.setup_notes.as_deref() == Some(row.setup_notes);
        if content_matches && notes_match {
            continue;
        }
        let mut values = serde_json::Map::new();
        values.insert(
            "full_content".to_string(),
            serde_json::Value::String(row.full_content.to_string()),
        );
        values.insert(
            "setup_notes".to_string(),
            serde_json::Value::String(row.setup_notes.to_string()),
        );
        updated += Plugin::objects()
            .filter(plugin::SLUG.eq(row.slug))
            .update_values(values)
            .await?;
    }
    Ok(updated)
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
    ("umbral-permissions", "umbral_reviewed"),
    ("umbral-rest", "umbral_reviewed"),
    ("umbral-openapi", "self_reviewed"),
    ("umbral-tasks", "self_reviewed"),
    ("umbral-security", "third_party_reviewed"),
    ("umbral-storage", "umbral_reviewed"),
    ("umbral-oauth", "self_reviewed"),
    ("umbral-realtime", "umbral_reviewed"),
    ("umbral-cache", "self_reviewed"),
    ("umbral-health", "self_reviewed"),
    ("umbral-livereload", "self_reviewed"),
    ("umbral-analytics", "umbral_reviewed"),
    ("umbral-email", "needs_review"),
    ("umbral-logs", "umbral_reviewed"),
    ("umbral-signals", "umbral_reviewed"),
    ("umbral-rls", "umbral_reviewed"),
    ("umbral-tenants", "umbral_reviewed"),
    ("umbral-playground", "self_reviewed"),
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
            FeatureSeed {
                name: "Auto CRUD views",
                description: "List, create, edit, delete generated from every registered model.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Search and multi-filter",
                description: "Toolbar search plus combinable `list_filter` facets.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "FK / M2M / O2O pickers",
                description: "Async relation pickers with search-as-you-type.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Per-model permissions",
                description: "Per-action `view/add/change/delete` gating via umbral-permissions.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "File and image widgets",
                description: "Multipart upload with image thumbnail preview.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Markdown / RTE field widgets",
                description: "`#[umbral(widget = ...)]` renders rich editors in the form.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Dashboard widgets",
                description: "KPI cards, charts, and recent-activity panels on the index.",
                status: IP,
                maturity: BETA,
            },
            FeatureSeed {
                name: "Bulk actions",
                description: "Select rows then act — delete, publish, export.",
                status: PL,
                maturity: DES,
            },
            FeatureSeed {
                name: "Inline editing",
                description: "Edit related rows on the parent form (tabular / stacked).",
                status: PL,
                maturity: DES,
            },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-auth",
        features: &[
            FeatureSeed {
                name: "User and group models",
                description: "Built-in `AuthUser` plus groups and roles.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Argon2 password hashing",
                description: "Modern password hashing with sensible defaults.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Permissions and RBAC",
                description: "Group/permission M2M checks via umbral-permissions.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Bearer tokens",
                description: "Opaque DB-backed API tokens, hashed at rest.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "OAuth / social login",
                description: "Sign in with Google/GitHub and connect accounts (umbral-oauth).",
                status: S,
                maturity: BETA,
            },
            FeatureSeed {
                name: "Password reset",
                description: "Token-based reset flow (email delivery pending umbral-email).",
                status: IP,
                maturity: BETA,
            },
            FeatureSeed {
                name: "SSO / OIDC",
                description: "Enterprise single sign-on.",
                status: PL,
                maturity: DES,
            },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-sessions",
        features: &[
            FeatureSeed {
                name: "DB-backed session store",
                description: "Server-side sessions persisted through the ORM.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Session middleware",
                description: "Cookie handling with secure defaults.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Login / logout flow",
                description: "Establish and tear down the authenticated session.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Redis-backed sessions",
                description: "Shared session store for horizontal scaling.",
                status: PL,
                maturity: DES,
            },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-rest",
        features: &[
            FeatureSeed {
                name: "Serializers and viewsets",
                description: "Models become JSON resources with zero config.",
                status: S,
                maturity: BETA,
            },
            FeatureSeed {
                name: "Routers and pagination",
                description: "Collection/detail routes with page slicing.",
                status: S,
                maturity: BETA,
            },
            FeatureSeed {
                name: "Filtering and search",
                description: "Query-string filters and free-text search per resource.",
                status: S,
                maturity: BETA,
            },
            FeatureSeed {
                name: "Authentication and permissions",
                description: "Session/bearer auth chain with per-resource permission gates.",
                status: S,
                maturity: BETA,
            },
            FeatureSeed {
                name: "Endpoint discovery",
                description: "`GET /api/` API root listing resources and plugin endpoints.",
                status: S,
                maturity: BETA,
            },
            FeatureSeed {
                name: "Custom @action endpoints",
                description: "Collection/detail actions beyond CRUD.",
                status: U,
                maturity: BETA,
            },
            FeatureSeed {
                name: "Nested writable serializers",
                description: "Create a parent and its children in one request.",
                status: PL,
                maturity: DES,
            },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-openapi",
        features: &[
            FeatureSeed {
                name: "OpenAPI 3 schema generation",
                description: "Auto-generated spec from registered resources.",
                status: S,
                maturity: BETA,
            },
            FeatureSeed {
                name: "Playground UI",
                description: "Mini-Postman request/response surface (umbral-playground).",
                status: S,
                maturity: BETA,
            },
            FeatureSeed {
                name: "Vendor extensions",
                description: "FK targets, enums, nullable/readOnly surfaced in the schema.",
                status: S,
                maturity: BETA,
            },
            FeatureSeed {
                name: "securitySchemes publishing",
                description: "Auth requirements per endpoint for auto-detect in the playground.",
                status: IP,
                maturity: BETA,
            },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-tasks",
        features: &[
            FeatureSeed {
                name: "#[task] macro",
                description: "Annotate a function as an enqueueable background job.",
                status: U,
                maturity: ALPHA,
            },
            FeatureSeed {
                name: "DB-backed queue",
                description: "Jobs persisted to a table and drained by a worker.",
                status: E,
                maturity: ALPHA,
            },
            FeatureSeed {
                name: "Worker process",
                description: "`cargo run -- worker` consumes and executes jobs.",
                status: E,
                maturity: ALPHA,
            },
            FeatureSeed {
                name: "Retries and backoff",
                description: "Failed jobs retry with exponential backoff.",
                status: E,
                maturity: ALPHA,
            },
            FeatureSeed {
                name: "Scheduled tasks",
                description: "Run a job at a future `eta`.",
                status: PL,
                maturity: DES,
            },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-security",
        features: &[
            FeatureSeed {
                name: "CSRF protection",
                description: "Double-submit token enforced on every POST.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "HSTS and secure headers",
                description: "Strict-Transport-Security and friends by default.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Clickjacking protection",
                description: "X-Frame-Options / frame-ancestors headers.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "Template auto-escaping",
                description: "Output escaped by default; opt out explicitly.",
                status: S,
                maturity: STA,
            },
        ],
    },
    PluginFeatureSet {
        crate_name: "umbral-static",
        features: &[
            FeatureSeed {
                name: "Production static serving",
                description: "Serve compiled assets and uploaded media in prod.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "collectstatic command",
                description: "Gather every plugin's static dir into one output tree.",
                status: S,
                maturity: STA,
            },
            FeatureSeed {
                name: "gzip / brotli compression",
                description: "Compressed responses for static assets.",
                status: PL,
                maturity: DES,
            },
        ],
    },
];

/// Slugify a feature name into the `<crate>-<name>` unique-slug tail.
fn feature_slug(crate_name: &str, name: &str) -> String {
    let tail: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
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
