# 01 — App and Settings

| | |
|---|---|
| **Status** | Draft |
| **Maps to milestone** | M0 (workspace, typed settings, sqlx pool, one hand-written route) |
| **Companions** | `00-overview.md`, `02-plugin-contract.md`, `05-backends-and-system-check.md`, `arch.md §2`, `arch.md §3` |

## Purpose

How an umbral app is constructed at boot. Covers the two surfaces a user touches at M0:

- **`Settings`** — a typed, environment-aware configuration struct that `figment` loads from defaults plus a file plus env vars.
- **`App::builder().build()`** — the fluent entry point that wires settings, the database pool, and (later) plugins together, runs the system check, fires `on_ready` hooks, and returns a runnable `App`.

The spec also pins down the **ambient-pool decision**: where the `OnceLock<DbPool>` lives, the test override path (write-once is the hard constraint), and the alias hook multi-database routing rides on top of.

## Concepts

### Settings

The framework owns one top-level `Settings` struct. It carries framework-level configuration (database URLs, secret, environment, allowed hosts, log level) and nothing else. Plugins own their own settings structs and take them as constructor arguments. The user composes the two at the builder.

```rust
#[derive(Debug, Deserialize)]
pub struct Settings {
    pub database_url: String,
    pub databases: HashMap<String, String>,   // alias → URL; empty by default
    pub secret_key: SecretString,
    pub environment: Environment,             // Dev | Test | Prod
    pub allowed_hosts: Vec<String>,
    pub log_level: tracing::Level,
}

impl Settings {
    pub fn from_env() -> Result<Self, ConfigError> { /* figment */ }
}
```

`Settings::from_env()` is the only constructor user code calls. It layers, in order of precedence (later wins):

1. The struct's `Default` (built-in defaults: `environment: Environment::Dev`, empty `databases` map, etc.).
2. A TOML file at the configured path (default `umbral.toml`, override via `UMBRAL_CONFIG_PATH`).
3. Environment variables prefixed `UMBRAL_`. Nested fields use `__` as separator: `UMBRAL_DATABASES__REPLICA=postgres://…`.

Plugin settings work the same way at the plugin's own level. `umbral-rest` exposes `RestSettings`; the user instantiates it (typically `RestSettings::from_env()?`) and passes it into the plugin constructor. There is **no** shared global namespace for plugin settings. The "every plugin dumps its config into one shared module" pattern is rejected here.

### App and the builder

```rust
pub struct App { /* opaque; owns plugin registry, router, registered databases */ }

impl App {
    pub fn builder() -> AppBuilder { AppBuilder::default() }
    pub async fn serve(self, addr: impl Into<SocketAddr>) -> Result<()>;
}

pub struct AppBuilder { /* fields */ }

impl AppBuilder {
    pub fn settings(self, settings: Settings) -> Self;
    pub fn database(self, alias: &str, pool: DbPool) -> Self;     // omit and the pool comes from settings.database_url
    pub fn plugin<P: Plugin + 'static>(self, plugin: P) -> Self;
    pub fn router(self, router: Router) -> Self;                  // M0 escape hatch: hand-written routes before the plugin contract exists
    pub fn build(self) -> Result<App, BuildError>;
}
```

`build()` is the single point that can fail at boot. After it returns `Ok`, every ambient handle is live and the app is ready to serve.

### Ambient state via `OnceLock`s

Each concern lives in its own module and owns its own `OnceLock`. The builder is the only writer.

| Module | `OnceLock` content | Read accessor |
|---|---|---|
| `umbral::settings` | `Settings` | `umbral::settings()` |
| `umbral::db` | `HashMap<String, DbPool>` (alias → pool) | `umbral::db::pool()` (default) / `umbral::db::pool_for(alias)` |
| `umbral::plugins` | `Vec<Box<dyn Plugin>>` | internal; consumers go through plugin-specific accessors |
| `umbral::tasks` | `TaskQueue` (set only if `umbral-tasks` was registered) | `umbral::tasks::enqueue(...)` etc. |
| `umbral::cache` | `Cache` (set only if a cache plugin was registered) | `umbral::cache::get(...)` etc. |

Per-module `OnceLock`s mean the internal crate split can refactor without forcing every consumer to update imports. The builder calls `umbral::db::init(pools)`, `umbral::settings::init(settings)`, and so on; those `init` functions are visible across the workspace's internal crates but not callable from user code.

## API-shape sketch

A minimal M0 binary, using the `router(...)` escape hatch since the Plugin contract doesn't exist yet:

```rust
use umbral::prelude::*;
use umbral::web::{Router, get};

#[tokio::main]
async fn main() -> Result<()> {
    let settings = Settings::from_env()?;
    let pool = umbral::db::connect(&settings.database_url).await?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .router(Router::new().route("/hello", get(hello)))
        .build()?;

    app.serve("127.0.0.1:8000").await
}

async fn hello() -> &'static str { "hello, umbral" }
```

At M7, the `router(...)` call is replaced by `.plugin(SomePlugin)` calls; the plugins contribute their own routes. The escape hatch stays on the builder, but its typical use shrinks to "I need one ad-hoc route outside a plugin."

## Mechanics and invariants

### Lifecycle phases

`App::builder().build()` runs five phases in order. Each phase is a fail-loud boundary; a problem at phase N must surface as an error before phase N+1 runs.

1. **Configure.** The builder collects settings, database pools (one or more aliases), plugins, and the hand-written router into builder-local state. Nothing is published to ambient `OnceLock`s yet.
2. **Resolve order.** Plugins declare dependencies (`umbral-admin` depends on `umbral-auth`, etc.). The builder topologically sorts them. A cycle here is a `BuildError`.
3. **Publish ambient state.** `Settings`, pools, plugin registry, task-queue handle, and cache are written into their respective `OnceLock`s, in that order. After this phase, `umbral::settings()` and `umbral::db::pool()` return real values.
4. **System check.** The compatibility check from `05-backends-and-system-check.md` runs: every model's fields are checked against the active backend; every plugin's settings struct validates; routing collisions are detected. A failure here is a `BuildError` that names the offending field, plugin, or route.
5. **`on_ready`.** Each plugin's `Plugin::on_ready(&self, &AppContext)` runs in dependency order. This is where signals get connected, periodic schedules start, and admin registrations are sealed. Failures propagate as `BuildError`.

`App::serve(addr)` is a separate call that binds the axum listener. Tests skip `serve` and drive routes via `umbral::test::Client` (designed in outline `testing.md`).

### Test override of the ambient pool

`OnceLock` is write-once per process, so tests can't swap the pool by calling `init` twice. Two escape hatches close the gap:

**Explicit pool on the Manager.** Every `QuerySet` constructor has a parallel form that takes an explicit pool. `Post::objects().on(&pool).filter(...)` overrides the ambient. This covers tests that drive the ORM directly.

**Task-local scoping.** For tests that exercise framework code that reads the ambient pool (handlers, plugin internals), `umbral::test::with_pool` sets a `tokio::task_local!` and the ambient accessor checks it first:

```rust
umbral::test::with_pool(test_pool, async {
    let response = client.get("/posts").send().await?;
    assert_eq!(response.status(), 200);
}).await
```

The pool accessor falls back to the `OnceLock` if the task-local isn't set:

```rust
pub fn pool() -> DbPool {
    TEST_POOL.try_with(|p| p.clone())
        .unwrap_or_else(|_| POOL.get().expect("umbral: db pool not initialised").clone())
}
```

Cost: every pool access does a task-local probe. In release builds this is a TLS-shaped read; tests don't care about it. The accessor stays sync because `DbPool` itself is an `Arc` under the hood.

### Multi-database routing

Multiple pools register under aliases:

```rust
App::builder()
    .settings(settings)
    .database("default", primary)
    .database("replica", replica)
    .build()?;
```

`umbral::db::pool()` returns the `default` pool; `umbral::db::pool_for("replica")` returns any named one. At the Manager level, `Post::objects().using("replica")` selects a non-default pool without disturbing the ambient-by-default rule. The full call-site design lives in `03-orm-querysets.md`; this spec only owns the registration story.

### Settings dispatch

The framework's `Settings` struct is the only one stored in the `umbral::settings` `OnceLock`. Plugin-level settings live inside the plugin instance itself; `App::builder().plugin(RestPlugin::new(RestSettings::from_env()?))` is the canonical pattern. When code outside a plugin needs to read another plugin's setting (the admin asking the REST plugin for its `default_page_size`, say), it goes through an accessor the plugin exposes, not through the registry directly.

## Trade-offs and alternatives considered

**Why `OnceLock` instead of `State<DbPool>` in every handler signature.** Threading `State` is the idiomatic axum approach but defeats the declarative ORM shape (`Post::objects()` becomes `Post::objects(&state.pool)`). The cross-cutting rule in `arch.md §2.2` makes the call: process-scoped context is ambient, request-scoped context is explicit. This spec implements the rule.

**Why one `OnceLock` per module rather than a single `OnceLock<AppContext>`.** A central context object would force every consumer to import the same type, coupling plugins to a struct whose shape changes whenever a new ambient slot is added. Per-module `OnceLock`s let `umbral::tasks` add itself without recompiling `umbral::db`. The cost is a few extra `init` functions; the benefit is keeping the internal crate split refactorable, which `arch.md §1` requires.

**Why `figment` over hand-rolled env parsing.** figment's provider model (defaults → file → env) matches the lifecycle exactly and gives the user the familiar settings-file + environment-variable layering story without writing merging code.

**Why not auto-register plugins with `inventory` by default.** Decided in `02-plugin-contract.md`. The short version: explicit registration is debuggable; auto-registration is layered on top as an opt-in for plugin authors who want zero-config installation.

**Why not have `App::builder()` consume the `Settings` from a `Default::default()` if none was passed.** A missing `database_url` is a real error, not a silently-defaulted one. Forcing the user to call `Settings::from_env()` (or to construct a `Settings` literal in tests) keeps misconfiguration loud at boot.

## Open questions

- **Concrete `init` boundary.** The five `umbral::*::init(...)` calls need to be reachable from `App::builder().build()` but not from user code. The cleanest path is an internal trait, crate-private to `umbral-core`, that the builder uses. Verify once the workspace skeleton is in place at M0.
- **Settings file format.** TOML matches the Cargo ecosystem; YAML is familiar from other ecosystems. TOML is recommended. Revisit if the admin or REST plugin needs nested config TOML handles awkwardly.
- **Async settings reload.** Out of scope for M0. Settings are immutable after `build()`. A reload mechanism would reopen the ambient-state question and is intentionally deferred.

## Cross-links

- The cross-cutting rule this spec implements: `arch.md §2.2`.
- The plugin contract `.plugin(...)` calls into: `02-plugin-contract.md`.
- The system check that runs in phase 4: `05-backends-and-system-check.md`.
- `umbral::test::with_pool` is owned by: outline `testing.md`.
- The task-queue handle stored in `umbral::tasks`: outline `tasks.md`.
- Multi-DB call-site syntax (`.using("alias")`): `03-orm-querysets.md`.
