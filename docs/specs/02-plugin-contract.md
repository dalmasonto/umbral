# 02 — Plugin contract

| | |
|---|---|
| **Status** | Draft |
| **Maps to milestone** | M7 (extraction). Specced early because every built-in spec depends on its shape. |
| **Companions** | `00-overview.md`, `01-app-and-settings.md`, `06-migration-engine.md`, `arch.md §3`, all built-in plugin outlines |

## Purpose

The `Plugin` trait is umbra's only mechanism for extending the framework. Auth, sessions, admin, tasks, REST, and OpenAPI are all plugins; so is every third-party crate that wants to ship models, routes, or commands. This spec defines the contract those plugins implement, the registration mechanism (explicit, plus opt-in auto-registration via `inventory`), the `on_ready` lifecycle hook, and the prelude surface plugins author against.

The contract is the architectural keystone. If a built-in can't be expressed as a plugin without special-casing, the contract is wrong; that's why every built-in is structurally identical to a third-party plugin.

## Concepts

### The trait

```rust
pub trait Plugin: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    fn dependencies(&self) -> &'static [&'static str] { &[] }

    fn migrations(&self) -> Vec<Migration> { vec![] }

    fn routes(&self) -> Router { Router::new() }

    fn middleware(&self) -> Vec<BoxedLayer> { vec![] }

    fn commands(&self) -> Vec<BoxedCommand> { vec![] }

    fn system_checks(&self) -> Vec<SystemCheck> { vec![] }

    fn on_ready(&self, _ctx: &AppContext) -> Result<()> { Ok(()) }
}
```

Every method except `name()` has a default that returns the empty contribution. A plugin opts in only to what it contributes. A pure-middleware plugin overrides `middleware()` and nothing else; a pure-model plugin overrides `migrations()` only; the auth plugin overrides almost all of them.

### What a plugin can contribute

| Method | Contribution | Collected by |
|---|---|---|
| `migrations()` | Plugin-owned migrations. Each plugin owns its tables; no special-casing. | `06-migration-engine.md`'s tracking table, ordered by `dependencies()`. |
| `routes()` | An axum-shape `Router` that mounts under the plugin's path (e.g. `/auth/...`). | The `App`'s top-level Router. |
| `middleware()` | tower layers added to the global middleware chain. | The middleware chain (see open questions for cross-plugin ordering). |
| `commands()` | clap-shape subcommands extending the `manage.py`-equivalent CLI. | The `umbra-cli` binary. |
| `system_checks()` | Boot-time checks the plugin needs to pass before `on_ready` fires (settings validation, custom invariants). | The system-check phase in `01-app-and-settings.md` §Lifecycle phases. |
| `on_ready()` | Wire signals, start background work, seal admin registrations. | Called after system checks pass, in dependency order. |

`dependencies()` lets a plugin declare which other plugins must load first (`umbra-admin` depends on `umbra-auth`; `umbra-openapi` depends on `umbra-rest`). The builder uses it for topological ordering. Cycles are caught at boot as a `BuildError`.

### Registration: explicit by default, inventory by opt-in

The default path is explicit. The user names every plugin in their `App::builder()` call:

```rust
App::builder()
    .settings(settings)
    .plugin(AuthPlugin::default())
    .plugin(SessionsPlugin::default())
    .plugin(MyBlogPlugin::default())
    .build()?;
```

For plugin authors who want the `cargo add`-and-it-works experience, an opt-in mechanism via `inventory`/`linkme` lets a plugin self-register. The author writes:

```rust
// in the plugin's lib.rs
umbra::register_plugin!(BlogPlugin);
```

…and the user's binary calls `App::builder().with_auto_plugins().build()` to walk the inventory slice. The only thing the user still has to do is `use umbra_blog;` somewhere (typically `main.rs`) so the linker keeps the crate.

Default recommendation: **explicit `.plugin(...)`**. Reasons in §Trade-offs. Auto-registration is layered on top, not the default.

### The prelude

Plugins import the facade and nothing else:

```rust
use umbra::prelude::*;
```

`umbra::prelude` re-exports:

- The trait surface: `Plugin`, `AppContext`, `Router`, `Migration`, `BoxedLayer`, `BoxedCommand`, `SystemCheck`.
- The ORM surface: `Model`, `QuerySet`, `Manager`, common field types.
- Common error types: `Result`, `Error`.
- Common request/response and extractor types: `Request`, `Response`, `Json`, `Path`, `Query`, `Form`, `Auth`, `Session`.

The prelude is the single stable import surface. Authors never reach into `umbra-core::*` internals; the internal crate boundaries can refactor without breaking a single plugin (`arch.md §1`).

## API-shape sketch

A complete third-party plugin, end-to-end:

```rust
use umbra::prelude::*;

#[derive(Model)]
pub struct Post {
    pub id: i64,
    pub author_id: i64,
    pub title: String,
    pub body: String,
}

#[derive(Default)]
pub struct BlogPlugin {
    pub settings: BlogSettings,
}

impl Plugin for BlogPlugin {
    fn name(&self) -> &'static str { "blog" }

    fn dependencies(&self) -> &'static [&'static str] {
        &["auth"]   // needs umbra-auth's User table
    }

    fn migrations(&self) -> Vec<Migration> {
        umbra::migrations::generated!("blog")
    }

    fn routes(&self) -> Router {
        Router::new()
            .route("/posts", get(list).post(create))
            .route("/posts/:id", get(detail))
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<()> {
        // wire signals, schedule periodic work, etc.
        Ok(())
    }
}
```

That's the whole plugin-author surface. Models declared with `#[derive(Model)]` get picked up automatically (the macro registers them with the per-plugin model registry that `migrations()` reads); routes return an axum router; everything else has a sensible default.

## Mechanics and invariants

### Dependency ordering

`dependencies()` returns plugin names. At boot, the builder walks the registered plugins, builds a dependency graph, and topologically sorts. A cycle is a `BuildError` that names the cycle.

The order shows up in three places:

1. **Migration collection.** `migrate` collects migrations in plugin-order, so cross-plugin FKs always have their target table created first. Within a single plugin, migrations are ordered by their filename's numeric prefix.
2. **Route mounting.** Routes from earlier-ordered plugins mount first. Later plugins can wrap them via middleware in `on_ready` if they need to.
3. **`on_ready` execution.** `on_ready` runs in topological order, so a dependent plugin can rely on its dependencies' ambient state already being live.

### Plugin-owned migrations

Each plugin returns its own migrations from `migrations()`. They get tracked in the umbra-owned table designed in `06-migration-engine.md`, keyed by `(plugin_name, migration_name)`. No plugin's migrations are special-cased: `umbra-auth`'s `0001_initial.sql` is recorded with the same row shape as `my-blog`'s `0001_initial.sql`.

This is the structural proof of dogfooding. If the built-ins required a parallel migration path, the trait would have to know about them, and dependency-inversion would break.

### `on_ready` and signals

`on_ready` is **synchronous** and takes `&AppContext`. The context carries clones of the ambient handles plus a `tokio::runtime::Handle`, so a plugin can spawn async tasks from inside `on_ready` without making the trait method async:

```rust
fn on_ready(&self, ctx: &AppContext) -> Result<()> {
    ctx.runtime().spawn(async move {
        run_periodic_cleanup().await;
    });
    Ok(())
}
```

Signals (`pre_save`, `post_save`, custom events) are a separate concern owned by outline `signals.md`. They're async because they fire mid-database-operation. Plugins connect their signal handlers inside `on_ready`.

### Auto-registration via inventory

When a plugin author opts in:

```rust
umbra::register_plugin!(BlogPlugin);
```

…the macro expands to an `inventory::submit!` that pushes a `Box<dyn Plugin>` constructor closure into a distributed slice. `App::builder().with_auto_plugins()` walks the slice, calls each constructor, and registers the result like any other plugin.

The linker caveat is unavoidable: Rust's linker drops crates that nothing references. The user's binary must `use umbra_blog;` somewhere so the crate's static items survive. That `use` *is* the registration; the user just doesn't have to name the plugin twice.

## Trade-offs and alternatives considered

**One trait with many default-noop methods, vs separate traits per concern (`MigrationProvider`, `RouteProvider`, …).** Separate traits would be more compositional but require the plugin struct to implement N traits, and the builder to query each one separately. A single trait with default methods reads cleanly at the impl site (the methods are right there to override), keeps the builder code simple, and matches Django's `AppConfig` ergonomically. The trait surface is wide but shallow.

**Explicit registration as the default, not inventory.** Three reasons:

1. **Debuggability.** "Why is plugin X behaving like that" is answered by `grep -r '\.plugin(X' src/` when registration is explicit. Auto-registration moves the answer into the linker.
2. **Order control.** Auto-registered plugins arrive in linker-defined order; explicit registration shows dependency order at the call site, where someone reading the code can see it.
3. **Cargo features.** Conditional registration (`#[cfg(feature = "rest")]`) reads naturally at a call site. Hidden inside a distributed slice, it reads as "magic that may or may not fire."

The opt-in `with_auto_plugins()` mode is for plugin *authors* who want their users to skip a line. The default user experience is the explicit list.

**`name()` returns `&'static str`, not `String`.** Plugin names are keys in dependency graphs, the migration tracking table, and route prefixes. They're known at compile time. `&'static str` makes them compatible with `dependencies()` arrays without forcing allocation. The cost is that plugins can't pick a dynamic name; the benefit is the type system rejecting any plugin that tries.

**Sync `on_ready` with an async runtime handle, vs async-trait `on_ready`.** An async `on_ready` would force `Plugin` to be `async-trait`-ish, leaking the async-trait machinery into every plugin signature. Sync `on_ready` plus `ctx.runtime().spawn(...)` lets plugins kick off async work without paying the trait-shape cost. Once Rust stabilises `async fn` in traits without dyn-compatibility issues for our shape, this might be revisited.

## Open questions

- **Settings schema validation.** `system_checks()` includes settings validation, but the API for declaring a settings schema is unresolved. Two options: the plugin returns a `serde_json::Schema` (works for any settings struct that derives `JsonSchema`), or `Plugin` gains an associated type `type Settings`. Resolve by M9 when `umbra-tasks` is the first plugin to need real validation.
- **Plugin-to-plugin signal subscription.** Once `signals.md` defines the API, a plugin that wants to subscribe to *another plugin's* signal needs the signal type reachable. Simplest path: signal types declared in the publishing plugin's public API. Revisit when the admin needs to listen for auth's post-login signal.
- **Cross-plugin middleware ordering.** Each plugin returns its own middleware layers; the builder concatenates them in topological order. If two plugins both add a rate limiter, there's no way for the user to interleave them. Likely needs a `priority` on the layer or an explicit `App::builder().middleware_order(...)` override. Defer until a real ordering conflict surfaces in M9–M11.

## Cross-links

- The dependency-inversion model this spec rests on: `arch.md §3`.
- The prelude's physical exports: `umbra-core` and the facade crate (no spec; tracked at the workspace level).
- Plugin-owned migrations are collected by: `06-migration-engine.md`.
- The system check `system_checks()` feeds: `05-backends-and-system-check.md`.
- The signal API plugins connect inside `on_ready`: outline `signals.md`.
- The CLI surface `commands()` extends: the CLI section in `arch.md` (promoted to a deep spec when the binary's command list grows past `migrate` / `makemigrations` / `inspectdb`).
- Plugin instantiation in user code: `01-app-and-settings.md` §App and the builder.
