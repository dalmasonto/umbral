# 08 — Authoring a third-party plugin

| | |
|---|---|
| **Status** | Draft |
| **Maps to milestone** | Usable from M7 onward (Plugin trait extraction). The guide exists before then so authors know the target. |
| **Companions** | `00-overview.md`, `01-app-and-settings.md`, `02-plugin-contract.md`, `06-migration-engine.md`, outlines `signals.md` / `testing.md` / `dev-experience.md` |

## Purpose

The author-side complement of `02-plugin-contract.md`. That spec says **what the `Plugin` contract is**; this spec says **how you build one**.

The reader is a Rust developer who wants to ship `umbra-cors`, `umbra-audit-log`, an OAuth integration, or anything else that plugs into umbra the way `django-cors-headers`, `django-debug-toolbar`, or DRF plug into Django. They've installed umbra in a project and want to know: how do I package a feature so someone can `cargo add umbra-foo` and `.plugin(FooPlugin::default())`?

The framework owes you a walked path from `cargo new` to `cargo publish`. This is it.

## Concepts

### A plugin is a Cargo crate

One plugin equals one crate. The crate depends on the **`umbra` facade only** — never `umbra-core`, `umbra-macros`, or any other internal crate directly. That's the dependency-inversion rule from `arch.md §1` (Pillar 3) applied at the author level: when umbra refactors its internal crate split, your plugin doesn't break.

### Naming convention

Crate name: **`umbra-<thing>`**. Mirrors Django's `django-<thing>`. Examples: `umbra-cors`, `umbra-audit-log`, `umbra-oauth`. The crate name's suffix matches what your plugin's `Plugin::name()` returns: `umbra-cors` → `fn name(&self) -> &'static str { "cors" }`. That match is what makes "plugin `cors`" in error messages and migration tracking unambiguous.

### Project layout

```text
umbra-cors/
├── Cargo.toml
├── README.md
├── LICENSE                       # MIT/Apache-2.0 dual-license is the Rust convention
├── src/
│   ├── lib.rs                    # re-exports your Plugin struct, settings, and any public types
│   ├── plugin.rs                 # the Plugin trait impl
│   ├── settings.rs               # your typed CorsSettings (if you take config)
│   ├── middleware.rs             # the actual CORS layer (if you ship middleware)
│   └── models.rs                 # (optional) your models, if your plugin owns tables
├── migrations/                   # (optional) one JSON file per migration, only if you own tables
│   └── 0001_initial.json
├── templates/                    # (optional) per-plugin template directory
└── static/                       # (optional) per-plugin static assets
```

`src/` files are conventional, not enforced. The framework looks for: a `Plugin` impl exported from `lib.rs`, a `migrations/` directory at the crate root (if `Plugin::migrations()` returns anything), and `templates/` / `static/` if the templates engine or `collectstatic` discovers them. Per outline `templates.md` and `static-and-media.md`.

### `Cargo.toml` minimum

```toml
[package]
name = "umbra-cors"
version = "0.1.0"
edition = "2021"
license = "MIT OR Apache-2.0"
description = "CORS middleware as an umbra plugin."

[dependencies]
umbra = { version = "0.x" }
serde = { version = "1", features = ["derive"] }
tower-http = { version = "0.5", features = ["cors"] }
```

That's it. The `umbra` facade pulls in everything an author needs through the prelude.

## API-shape sketch: the smallest viable plugin

A "hello plugin" with one route, zero models, no settings, ten lines of Rust:

```rust
// src/lib.rs
use umbra::prelude::*;

#[derive(Default)]
pub struct HelloPlugin;

impl Plugin for HelloPlugin {
    fn name(&self) -> &'static str { "hello" }

    fn routes(&self) -> Router {
        Router::new().route("/hello", get(|| async { "hello from a plugin" }))
    }
}
```

A user adds it to their app:

```rust
App::builder()
    .settings(Settings::from_env()?)
    .plugin(HelloPlugin::default())
    .build()?
    .serve("0.0.0.0:8000").await
```

That's a real working plugin. Every other shape in this spec is "this, plus more methods on the trait."

## Mechanics: building it up

### Adding models and migrations

When your plugin owns data, declare models in `src/models.rs` and return your generated migrations from `Plugin::migrations()`:

```rust
// src/models.rs
use umbra::prelude::*;

#[derive(Model)]
pub struct AuditEntry {
    pub id: i64,
    pub user_id: i64,
    pub action: String,
    pub created_at: DateTime<Utc>,
}
```

```rust
fn migrations(&self) -> Vec<Migration> {
    umbra::migrations::generated!("audit")
}
```

You don't hand-write migration files. `umbra-cli makemigrations` reads your models and emits `migrations/0001_initial.json` per `06-migration-engine.md`. The macro picks up everything under `migrations/` and returns the ordered list.

If your tables FK to another plugin's tables (most commonly `umbra-auth`'s `user`), declare the dependency so the migration engine orders your migrations after theirs:

```rust
fn dependencies(&self) -> &'static [&'static str] {
    &["auth"]
}
```

### Adding typed settings

If your plugin takes configuration, ship a settings struct and accept it in the constructor:

```rust
// src/settings.rs
#[derive(Debug, Deserialize)]
pub struct CorsSettings {
    pub allowed_origins: Vec<String>,
    pub allow_credentials: bool,
    pub max_age: u64,
}

impl Default for CorsSettings { /* sensible defaults */ }
impl CorsSettings { pub fn from_env() -> Result<Self, ConfigError> { /* figment */ } }
```

The user composes:

```rust
.plugin(CorsPlugin::new(CorsSettings::from_env()?))
// or accept your default:
.plugin(CorsPlugin::default())
```

Your settings live with your plugin. They are **not** merged into the framework's `Settings`. Per `01-app-and-settings.md` §Settings dispatch: each plugin owns its own settings struct so the global namespace stays flat.

### Adding routes, middleware, commands

Fill in the trait method that matches what you're contributing:

```rust
fn routes(&self) -> Router { /* axum-shape routes */ }
fn middleware(&self) -> Vec<BoxedLayer> { /* tower-shape layers */ }
fn commands(&self) -> Vec<BoxedCommand> { /* clap-shape subcommands extending the binary */ }
fn system_checks(&self) -> Vec<SystemCheck> { /* boot-time validations */ }
fn on_ready(&self, ctx: &AppContext) -> Result<()> { /* wire signals, spawn periodic work */ }
```

All have default empty impls. You override only what you contribute.

### Integrating with built-in plugins

If your plugin admin-registers a model (uses `umbra-admin`) or exposes a REST viewset (uses `umbra-rest`), declare both the dependency on the built-in **and** make the integration optional via Cargo features so users without that built-in installed aren't forced to pull it in:

```toml
[dependencies]
umbra = { version = "0.x" }
umbra-admin = { version = "0.x", optional = true }
umbra-rest = { version = "0.x", optional = true }

[features]
default = []
admin = ["umbra-admin"]
rest = ["umbra-rest"]
```

```rust
fn dependencies(&self) -> &'static [&'static str] {
    let mut deps: &'static [&'static str] = &[];
    #[cfg(feature = "admin")] { deps = &["admin"]; }
    deps
}

#[cfg(feature = "admin")]
fn admin_registrations(&self) -> Vec<AdminRegistration> { /* ... */ }
```

Users opt in: `cargo add umbra-foo --features admin,rest`.

### Testing your plugin

The test client builds an `App` that registers your plugin and drives it like a real one (outline `testing.md`):

```rust
#[tokio::test]
async fn hello_route_works() {
    let app = App::builder()
        .settings(test_settings())
        .plugin(HelloPlugin::default())
        .build().unwrap();

    let client = umbra::test::Client::new(app);
    let res = client.get("/hello").send().await.unwrap();
    assert_eq!(res.status(), 200);
}
```

For tests that exercise models, scope a test pool with `with_pool` so the ambient `OnceLock` doesn't trap you across tests (per `01-app-and-settings.md` §Test override):

```rust
umbra::test::with_pool(test_pool, async {
    AuditPlugin::default().log("login", user_id).await?;
    assert_eq!(AuditEntry::objects().count().await?, 1);
    Ok(())
}).await
```

### Naming, semver, publishing

- **Plugin name** (`Plugin::name()`): kebab-case, short, matches the crate-name suffix.
- **Semver**: track the `umbra` facade major version in your README. The facade is the contract; internal crates can refactor without breaking you. When umbra moves `0.x → 0.y`, plan a release.
- **License**: MIT or Apache-2.0 (dual-license is the Rust ecosystem convention).
- **README**: include a 5-line example showing the `App::builder().plugin(YourPlugin::default())` line — that's all most users want to see before adopting.
- **`cargo publish`**: as normal. crates.io is the registry. Once published, anyone can `cargo add umbra-foo`.
- **User-facing docs**: follow the umbra docs rule from CLAUDE.md (purpose + one example + link to spec). You can host them anywhere; many third-party crates use docs.rs plus a short README.

## Real-world sketches

Three shapes that cover most of the design space:

**`umbra-cors`** (middleware-only). Zero models, zero migrations, zero settings dispatch beyond the constructor:

```rust
pub struct CorsPlugin { settings: CorsSettings }
impl Plugin for CorsPlugin {
    fn name(&self) -> &'static str { "cors" }
    fn middleware(&self) -> Vec<BoxedLayer> { vec![BoxedLayer::new(self.cors_layer())] }
}
```

**`umbra-audit-log`** (models + signals). Subscribes to `umbra-auth`'s `POST_LOGIN` signal (outline `signals.md`), writes audit rows on each event:

```rust
pub struct AuditPlugin;
impl Plugin for AuditPlugin {
    fn name(&self) -> &'static str { "audit" }
    fn dependencies(&self) -> &'static [&'static str] { &["auth"] }
    fn migrations(&self) -> Vec<Migration> { umbra::migrations::generated!("audit") }
    fn on_ready(&self, ctx: &AppContext) -> Result<()> {
        ctx.signals().connect(umbra_auth::POST_LOGIN, log_login);
        Ok(())
    }
}
```

**`umbra-oauth`** (depends on `umbra-auth`). Layered on top of an existing built-in's `User` model. Adds external-auth tables, OAuth flow routes:

```rust
pub struct OAuthPlugin { settings: OAuthSettings }
impl Plugin for OAuthPlugin {
    fn name(&self) -> &'static str { "oauth" }
    fn dependencies(&self) -> &'static [&'static str] { &["auth"] }
    fn migrations(&self) -> Vec<Migration> { umbra::migrations::generated!("oauth") }
    fn routes(&self) -> Router { /* /oauth/google, /oauth/github callbacks */ }
}
```

## Trade-offs and alternatives considered

**Author depends on the facade only, vs depending on `umbra-core` directly.** Some plugin systems let "advanced" authors drop down to internal types. umbra deliberately doesn't: the facade is the stable surface, and dropping the facade-only rule would couple plugins to umbra's internal crate split. If a plugin author legitimately needs something the facade doesn't re-export, the right fix is to add the re-export, not bypass it.

**Per-plugin settings struct, vs one global settings registry.** Django's `settings.py` is one flat namespace where every contrib app dumps its config. That works because Python's settings are dynamically typed. In Rust, one global struct would have to know every plugin's settings shape at compile time — impossible. The per-plugin struct lives with the plugin, takes `from_env()` for layering, and gets passed to the constructor.

**One JSON file per migration, vs one Rust module per migration.** This is the framework's choice from `06-migration-engine.md` and it benefits plugin authors: you don't write migration code by hand. `makemigrations` produces JSON; you commit it; consumers `migrate` and it works. A Rust-module approach would force authors to compile-test every migration; JSON keeps the author surface declarative.

**Cargo features for optional admin/REST integration, vs always-pulled deps.** Always-pulling `umbra-admin` would force users of `umbra-foo` to compile the admin even if they don't use it. Cargo features let the user opt in. The cost is one extra line in the `Plugin` trait impl (the `#[cfg(feature = ...)]` guard); the win is "you can install my plugin without the admin's surface area."

## Open questions

- **`umbra::migrations::generated!()` macro shape.** Its concrete API isn't pinned by spec 06 yet. Authors get "give the macro your plugin name and it returns the migration list" as the contract; the spec confirms the form when M5 lands. Worst case: a `migrations()` helper function the author writes by hand against `umbra::migrations::MigrationFile`.
- **Third-party plugin scaffolding.** Outline `dev-experience.md` covers `startapp` for in-tree plugins. A `cargo umbra new-plugin <name>` generator that scaffolds the third-party crate layout (this spec's §Project layout) would shave real friction. Follow-up to that outline.
- **Templates and static assets at publish time.** The §Project layout shows `templates/` and `static/` as plugin-owned directories. The runtime story is whether they're read from the installed crate's source files (only works when the consumer keeps the source) or embedded at compile time via `include_str!` / `include_dir!`. The deep specs for `templates.md` and `static-and-media.md` settle this; this spec carries the open question so authors know to check.
- **Auto-registration via `inventory`.** `02-plugin-contract.md` §Registration covers the `umbra::register_plugin!` macro. From an author's side: ship the macro call, and instruct your README to add `use umbra_foo;` somewhere in the consumer's `main.rs` so the linker keeps the crate. The auto-registration's UX is a documented quirk, not a defect.

## Cross-links

- The `Plugin` trait contract this spec builds on: `02-plugin-contract.md`.
- Settings layering and the per-plugin dispatch rule: `01-app-and-settings.md` §Settings dispatch.
- Migration file format and what `umbra::migrations::generated!()` returns: `06-migration-engine.md`.
- Cross-cutting design principles authors inherit (hide axum and friends; ambient vs explicit context): `arch.md §2.1` and `§2.2`.
- The test client and `with_pool`: outline `testing.md`.
- Plugin-to-plugin signal subscription patterns: outline `signals.md`.
- Cargo features for optional integration with `umbra-admin` and `umbra-rest`: this spec §Integrating with built-in plugins.
- Generator scaffolding for the third-party crate layout: outline `dev-experience.md` (follow-up promotion likely).
- The naming convention (`umbra-<thing>` for third-party plugins): `00-overview.md` §Naming conventions.
