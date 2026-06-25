# Building a Batteries-Included Web Framework in Rust - Architecture, Features & Build Strategy

The goal is a particular *feeling*: declare your data, get migrations, CRUD, an admin, and (optionally) an API almost for free, while gaining Rust's compile-time resilience. umbral is a self-contained Rust framework with its own ORM, migration engine, and plugin system. The framework is **thin-core + plugin-heavy**, and it **dogfoods its own plugin system** for every built-in feature.

> **Project name: `umbral`** ('of the shadow', from Latin *umbra*, shadow). Placeholder; rename the whole tree later with a single `sed 's/umbral/yourname/g'`. Convention: the facade crate is `umbral`, internals are `umbral-*`, and third-party plugins follow the `umbral-<thing>` naming pattern.

---

## 0. The North Star: make *porting* easy

Porting an existing API becomes trivial because of two features above all others:

1. **`inspectdb`.** Point it at an existing database and it generates models.
2. **Managed migrations.** Describe the desired schema; the framework computes the steps. The everyday loop is **declare → migrate → change → migrate**: declare or change a model, an autodetected migration is generated, `migrate` applies it, and the diff between consecutive snapshots produces the right `ALTER` / `DROP` next time.

If the framework nails introspection and migrations, it captures most of the "just port it" experience. These are priority features, not afterthoughts. The full declare → migrate → change → migrate cycle lands as soon as models exist, at M5; `inspectdb` follows at M6.

**Design principle.** Don't reimplement primitives (HTTP, async, SQL generation, JSON). Reimplement conventions and integration. Stand on crates; the value is the glue.

---

## 1. Architectural Pillars

### Pillar 1 — Thin core, everything else is a plugin

The core ships only what every app needs no matter what. Serializers/REST, admin, auth, sessions, and the task queue are **all plugins**, some shipped in the box, structurally identical to third-party ones. A REST-free app must work with zero serializer code.

### Pillar 2 — The framework dogfoods its own plugin system

The built-in features ship as ordinary plugins. Auth, admin, sessions, and tasks register through the same mechanism, own their own migrations, and expose routes and commands the same way a stranger's plugin would. If the built-ins can't be expressed as plugins, the plugin contract is wrong.

### Pillar 3 — Cargo workspace encodes core-vs-plugin

The crate boundaries *are* the architecture:

```
workspace/
├── umbral-core        # ORM, migrations, routing, DB backends, the Plugin TRAIT. Depends on nothing plugin-related.
├── umbral-macros      # #[derive(Model)], #[task], etc.
├── umbral             # FACADE: re-exports core (+ macros) as one stable surface → `use umbral::prelude::*`
├── umbral-cli         # the project management CLI binary
├── plugins/
│   ├── umbral-auth     # built-in plugin: users, permissions, password hashing
│   ├── umbral-sessions # built-in plugin: session store + middleware
│   ├── umbral-admin    # built-in plugin: auto CRUD UI
│   ├── umbral-tasks    # built-in plugin: DB-backed task queue
│   ├── umbral-rest     # OPTIONAL plugin: serializers, viewsets, routers (the REST layer)
│   └── umbral-openapi  # OPTIONAL plugin: Swagger UI / schema gen, depends on umbral-rest
```

**Dependency direction (this is the whole game).** Every plugin depends on `umbral` (the facade), never the reverse. `umbral-core` defines the `Plugin` *trait* but never names a concrete plugin. The user's **binary crate** depends on `umbral` plus all chosen plugins and wires them. So arrows point *inward* toward core; control flows *outward* through the trait. Cargo's ban on circular crate deps doesn't fight this — it enforces it. REST being a crate core does not depend on is the structural proof that "serializers are a plugin." OpenAPI depends on REST; core depends on neither.

---

## 2. Cross-cutting conventions

Two rules apply across every subsystem and shape umbral's *feel*. They sit here, ahead of the Plugin Contract, because the Plugin Contract starts naming concrete public surface (the prelude) and both rules need to be in scope before that point.

### 2.1 Visibility of underlying crates

**Does an umbral developer see axum?** Rule of thumb: if a crate is a way to build the framework, hide it; if it is how the user describes their own data and behavior, surface it.

| Crate | Visibility | Notes |
|---|---|---|
| **axum** | **Hidden** by default. `umbral::web::{Router, Request, Response, Json, Path, Query, Form}`. Escape hatch: `umbral::axum::*`. | Day-to-day umbral looks declarative and framework-native. |
| **sqlx** | **Hidden** behind `QuerySet` / `Manager`. Escape hatch: `umbral::db::query!` is `sqlx::query!`. | Compile-time-checked SQL remains available. |
| **sea-query** | **Fully hidden.** | Pure implementation detail. |
| **tower / tower-http** | **Mixed.** Middleware is configured through umbral's chain, but the underlying type is a tower service so standard layers compose. | Contract reads as umbral; ecosystem still works. |
| **serde** | **Visible.** Users `#[derive(Serialize, Deserialize)]` on their own types. | Ecosystem fluency, not infrastructure. |
| **clap** | **Visible at the extension seam.** Custom `Command`s use clap derives. | Same reason as serde. |
| **tracing** | **Visible.** Users add their own spans/logs. | Observability is the user's. |
| **figment / config** | **Hidden** behind `Settings`. | Users see typed structs, not a config library. |

### 2.2 Handler-visible context: ambient vs explicit

**Does a handler signature carry `State<DbPool>`?** No, and the same rule extends to every other kind of context. The first table answered *what types* show up; this one answers *what context* shows up.

| Kind of context | Examples | Visibility in a handler |
|---|---|---|
| **App-wide / process-scoped** | DB pool, `Settings`, plugin registry, task-queue handle, cache, template engine | **Ambient.** Set during `App::build()` (stored in `OnceLock`s inside the relevant module). Reached via accessors: `Post::objects()`, `umbral::settings()`, `umbral::tasks::enqueue(...)`. **No `State<…>` in the handler signature.** |
| **Per-request / request-scoped** | The Request, parsed body, path/query params, the session, the authenticated user, an active transaction handle | **Explicit arguments.** Extracted into the handler signature: `Request`, `Path<T>`, `Json<T>`, `Form<T>`, `Query<T>`, `Session`, `Auth<User>`. Uses axum extractors under the hood; the user sees umbral types only. |

A framework-native umbral handler, with no `State`, no `axum`, and an ambient ORM:

```rust
use umbral::prelude::*;

async fn create_post(
    auth: Auth<User>,
    Json(payload): Json<NewPost>,
) -> Result<Json<Post>> {
    let post = Post::objects()           // ambient pool via OnceLock
        .create(NewPost { author_id: auth.user.id, ..payload })
        .await?;
    Ok(Json(post))
}
```

**Edge cases the rule has to survive:**

- **Tests.** `OnceLock` is write-once per process. The override path is a `Manager::on(&pool)` explicit-pool escape hatch plus a `test_with_pool(pool, async { ... })` helper that scopes the override for a test future. Designed concretely in `docs/specs/01-app-and-settings.md`.
- **Multi-database routing.** Default pool is ambient; explicit alias via `Post::objects().using("replica")` keeps the rule.
- **Per-request transactions.** `Db::tx(|tx| async { ... })` passes `tx` into the closure as a request-scoped argument without leaking it through the handler signature.

---

## 3. The Plugin Contract (the heart of extensibility)

A plugin (the framework's unit of pluggable functionality, the equivalent of a self-contained "app") can contribute any subset of:

- **Models** → and therefore **migrations** (the killer requirement below)
- **Routes / views**
- **Middleware**
- **Management commands** (extend the CLI)
- **Settings schema + defaults** (typed config the plugin owns)
- **Admin registrations**
- **Signals / lifecycle hooks** (an `on_ready()` hook that runs once at boot)

### How a plugin is written (and why there is no cycle)

This is the crux of the whole ecosystem. A plugin imports the framework (its ORM, the `Plugin` trait) but the framework never imports the plugin; it reaches plugin code only through a trait object resolved at runtime. One-directional static dependency, dynamic registration. The mechanism is **dependency inversion**:

1. `umbral-core` owns the ORM and **defines the `Plugin` trait** (the contract). Depends on no plugin.
2. A plugin depends on the `umbral` facade, implements `Plugin`, and `use`s the ORM freely. It "magically" has the ORM because it imports it.
3. The user's **binary** depends on core plus every plugin and composes them.
4. Core only ever touches plugins as `Box<dyn Plugin>`. The trait object is the dynamic seam: the binary lists which plugins it uses and the framework wires them in without core ever statically naming a concrete plugin.

Mantra: **dependencies point inward toward core; control flows outward through the trait.** Cargo forbids circular crate deps, which simply enforces this architecture.

A complete third-party plugin looks like this — note it imports nothing but the facade:

```rust
use umbral::prelude::*;            // ORM, Plugin trait, routing — one stable surface

#[derive(Model)]                  // "magic" ORM access, because it depends on umbral
pub struct Post { /* fields */ }

pub struct BlogPlugin;

impl Plugin for BlogPlugin {
    fn name(&self) -> &str { "blog" }
    fn migrations(&self) -> Vec<Migration> { generated_migrations!() } // auto-run on `migrate`
    fn routes(&self) -> Router { /* ... */ }
    fn commands(&self) -> Vec<Command> { vec![] }
    fn on_ready(&self, _ctx: &AppContext) {}                           // runs once at boot
}
```

The author experience is: `cargo add umbral-blog`, then register it (see below). The **facade + prelude** is what keeps this clean — authors never reach into `umbral-core` internals, so you can refactor the internal crate split without breaking a single plugin.

### Ambient ORM access (the one place to decide deliberately)

For `Post::objects().filter(...)` to work without threading a pool through every call (the declarative ORM feel), managers need ambient access to the DB pool. Idiomatic Rust threads `State<Pool>` explicitly. The clean compromise: store the pool in a `OnceLock<DbPool>` set during `App::build()` so managers can read it ambiently, while still allowing an explicit pool to be passed in tests. Choose this on purpose; don't let globals creep in by accident. See §2.2 for the broader rule this fits inside.

### Registration

Rust has no import-time side effects, so registration is explicit (and clearer for it):

```rust
App::builder()
    .settings(settings)
    .plugin(AuthPlugin::default())
    .plugin(SessionsPlugin::default())
    .plugin(TasksPlugin::default())
    .plugin(RestPlugin::default())   // omit this and you have a REST-free app
    .plugin(MyBlogPlugin::default())
    .build();
```

Each plugin implements a `Plugin` trait whose methods return its migrations, routes, commands, and config defaults. Explicit builder registration is the debuggable default: the binary spells out exactly which plugins it uses, in order. For the zero-boilerplate "`cargo add umbral-blog` and it just works" experience, *optionally* layer in `inventory`/`linkme` distributed slices so a plugin self-registers at static-init — no `.plugin()` call needed. One caveat: the linker drops crates nothing references, so the binary must still list the plugin as a dependency for auto-registration to fire (which it does the moment you `cargo add` it).

### Automagic migrations on `migrate`

Once a plugin is registered:

1. `migrate` walks every registered plugin and collects `plugin.migrations()`.
2. Migrations are ordered by a dependency graph (cross-plugin FKs allowed).
3. Applied migrations are tracked in a umbral-owned table; only new ones run.
4. A third-party plugin "just works": drop it in, register it, `migrate`, done.

The framework's own auth/sessions/tasks tables are created this exact way; they are plugin migrations, not special-cased.

---

## 4. Core Feature Inventory (truly core only)

**reuse** = lean on a crate · **build** = your framework's real work.

### 4.1 Settings & Registry
- **Centralized, typed, environment-aware settings.** *(reuse: figment/config; build conventions)*
- **Plugin registry + builder.** *(build — Pillar 2)*
- **System check framework** — validate config & backend compatibility at boot. *(build)*

### 4.2 ORM / Data Layer *(most difficulty and value)*
- **Declarative models** — struct + `#[derive(Model)]`. *(build: proc macro)*
- **Field types** — text, int, float, bool, datetime, decimal, UUID, JSON, binary. *(build on sea-query types)*
- **Relationships** — ForeignKey, OneToOne, ManyToMany (through tables). *(build)*
- **Field options** — optional, default, unique, indexed, choices, validators. *(build via attrs + `Option<T>`)*
- **Model Meta** — table name, ordering, composite unique, indexes, constraints. *(build)*
- **QuerySet API** — filter/exclude/order_by/limit/values, lazy eval. *(build on sea-query)*
- **Expressions** — F() (field refs), Q() (boolean composition), aggregates, annotations. *(build)*
- **Relation loading** — select_related (joins), prefetch_related (N+1 fix). *(build)*
- **Managers** — `.objects` entry point, custom default querysets. *(build)*
- **Transactions** — atomic blocks, savepoints. *(reuse: sqlx)*
- **Multiple databases / routing.** *(reuse: sqlx pools; build router)*
- **Raw SQL escape hatch.** *(reuse: sqlx::query)*
- **Lifecycle hooks** — save/delete overrides, computed properties. *(build via traits)*
- **Signals** — pre_save/post_save, decoupled events. *(build: event bus)*
- **Validation** — full_clean equivalent. *(reuse: validator)*

### 4.3 Migrations *(porting superpower)*
- **`inspectdb`** — introspect existing DB → models. *(build on sqlx/sea-schema introspection)*
- **Autodetection** — diff model snapshot → ops. *(build — the intricate part)*
- **Dependency graph, reversibility, data migrations.** *(build)*
- **Squashing & fake migrations.** *(build, later)*

### 4.4 Routing, Views, Middleware
- **URL routing** — patterns, includes, namespaces, reverse(). *(build on axum)*
- **Views** — function handlers + generic class-equivalents via traits + composition (no inheritance). *(reuse + build)*
- **Middleware stack.** *(reuse: tower; build conventions)*
- **Request/response, file uploads, content negotiation.** *(reuse: axum/http)*

### 4.5 Security (secure by default)
- **CSRF, clickjacking headers, HSTS.** *(reuse: tower-http; build CSRF)*
- **XSS** — template autoescaping. *(reuse: minijinja/askama)*
- **SQL injection** — parameterized always. *(reuse: sqlx — free)*
- **Secret signing.** *(reuse: hmac/ring)*

### 4.6 CLI / Tooling
- **Project management CLI** — extensible subcommands. *(reuse: clap; build extension point)*
- **`startproject`/`startapp`/generators.** *(build: CLI)*
- **Dev server + autoreload.** *(reuse: cargo-watch/listenfd)*
- **Fixtures, test client, rich error pages.** *(build on serde/axum-test)*

### 4.7 Caching (core utility, pluggable backends)
- **Cache framework** — in-memory/Redis; per-view/fragment/low-level. *(reuse: moka/redis; build API)*

---

## 5. Database Backends & DB-Specific Fields

Abstract most dialect differences but also expose backend-specific power (Postgres extras: `ArrayField`, `HStoreField`, range fields, full-text search). Provide both halves.

### 5.1 Backend abstraction
- A `DatabaseBackend` trait covering dialect differences: type mapping, identifier quoting, upsert syntax, `RETURNING` support, etc. *(reuse: sea-query already abstracts dialects; sqlx abstracts drivers — Postgres/MySQL/SQLite)*

### 5.2 Backend-specific fields with guardrails *(a resilience win)*
- A field declares the backends it supports (e.g. `ArrayField` → `[Postgres]`).
- The **system check at boot** verifies every model's fields are compatible with the active backend. Using a Postgres-only field on MySQL fails **at startup with a clear message**, not at query time in production.
- Recommended default: target Postgres first (richest feature set), keep SQLite for tests, and let the check gate everything else. "Use Postgres to avoid surprises" becomes an *enforced* invariant rather than a hope.

---

## 6. Built-in Plugins (shipped in the box, structurally ordinary)

### 6.1 `umbral-auth`
User model (incl. custom user models), authentication backends, permissions & groups, password hashing *(reuse: argon2)*, login guards. Owns its migrations.

### 6.2 `umbral-sessions`
Session store + middleware. *(reuse: tower-sessions)* Owns its migrations (DB session backend).

### 6.3 `umbral-admin`
Register a model → auto CRUD UI: list display, filters, search, inlines, bulk actions, permission integration. The flagship "wow" feature. *(build)*

### 6.4 `umbral-tasks` — DB-backed task queue (background work out of the box)
- **`#[task]` macro / `Task` trait** to define tasks; typed args via serde.
- **DB-backed broker** — owns a `tasks` table via its own migration; no Redis or external message broker required to start. *(reuse: `underway` (Postgres-native) or `apalis` (multi-backend) as the engine)*
- **Worker process** — the `worker` command polls and executes.
- **Retries, scheduling/periodic (a beat-style scheduler), result storage.**
- **Pluggable broker later** (Redis, etc.) behind the same task API.
- Because it's DB-backed and registered like any plugin, `migrate` provisions its tables automatically: exactly the "plugin owns its migrations" story.

### 6.5 `umbral-rest` — the REST layer (OPTIONAL)
- **Serializers / ModelSerializer** — struct ↔ JSON + validation. *(reuse: serde; build mapping)*
- **ViewSets & routers** — auto-generate CRUD URL sets. *(build)*
- **Auth/permission/throttle classes, pagination, filtering, ordering.** *(build; reuse tower-governor for throttle)*
- **Renderers / content negotiation; browsable API later.** *(build)*
- Core does **not** depend on this crate. No REST plugin → no serializer overhead.

### 6.6 `umbral-openapi` (OPTIONAL, depends on `umbral-rest`)
- **Auto-generate OpenAPI schema + Swagger UI** from registered viewsets/serializers. *(reuse: utoipa; build integration)*

---

## 7. Leaning into Rust's Resilience

Design so users fall into these by default:

- **Illegal states unrepresentable** — nullable column → `Option<T>`; the null *must* be handled.
- **Errors are values** — `Result<T,E>` everywhere, framework error enum + `From` so `?` flows.
- **Compile-time query checks** — `sqlx::query!` fails the build on a bad column, not prod.
- **Backend mismatches caught at boot** — see §5.2; a class of runtime failures becomes a startup error.
- **Fearless concurrency** — `Send`/`Sync` enforced; no GIL → real parallelism for the worker pool.
- **Predictable latency** — no GC pauses.

The framework's job: make the *easy* path the *safe* path.

---

## 8. Build order (plugin-aware, dogfooding, learning-first)

Each milestone is independently demoable. Build the primitives by hand first, then extract abstractions. Managed migrations aren't deferred; the **declare → migrate** loop lands as soon as models exist, at M5.

**M0 — Foundations.** Workspace skeleton, typed settings, sqlx pool, one hand-written axum route.

**M1 — QuerySet by hand (no macros).** Builder → SQL for one hard-coded model. The deepest Rust lesson of the project: ownership, generics, builder patterns.

**M2 — `Model` trait, implemented manually.** Prove the target shape before automating.

**M3 — `#[derive(Model)]`.** Generate the M2 impl. Macros are easy once the target output is known.

**M4 — Backend abstraction + system check.** sea-query dialects; field/backend compatibility check at boot (§5.2).

**M5 — Migration engine.** Model-state snapshot, basic autodetection (create/alter/drop), tracking table, `migrate` CLI. The full **declare → migrate → change → migrate** loop works here. Ship the basic ops on day one; rename detection and data-preserving alters are iterated at M8.

**M6 — `inspectdb`.** Introspect an existing DB → models. Ship this immediately after M5 is solid; it's the porting payoff and feeds straight into the same migration loop.

**M7 — The Plugin contract.** Extract the `Plugin` trait. Routes, migrations, and commands flow through it. The architectural keystone. `migrate` extends to walk all registered plugins, ordered by a cross-plugin dependency graph.

**M8 — Harden autodetection + plugin-ify built-ins.** Rename vs. drop+add disambiguation, data migrations, cross-plugin FK ordering. Re-express auth and sessions as plugins. That's the proof of the contract.

**M9 — `umbral-tasks` plugin.** DB-backed queue, `#[task]`, the `worker` and `beat` commands. Owns its tables via its own migration.

**M10 — `umbral-rest` plugin.** Serializers, viewsets, routers, pagination, filtering, throttling, as an *optional* crate.

**M11 — `umbral-admin` plugin.** Auto CRUD UI: list/filter/search, inlines, bulk actions, permission integration.

**M12 — `umbral-openapi` plugin.** Swagger UI and schema generation from registered REST surface.

**M13 — Polish.** Generators (`startproject`, `startapp`), autoreload, browsable API, caching, fixtures, rich error pages.

---

## 9. Crate Shortlist

| Concern            | Crate(s)                                  |
|--------------------|-------------------------------------------|
| HTTP / routing     | axum (tokio + tower)                      |
| Middleware         | tower, tower-http                         |
| DB drivers         | sqlx (compile-time checked)               |
| SQL / dialects     | sea-query (+ sea-schema for introspection)|
| Macros             | syn, quote, proc-macro2                    |
| Plugin auto-reg (opt) | inventory or linkme                    |
| Serialization      | serde, serde_json                         |
| Validation         | validator                                 |
| CLI                | clap                                      |
| Templates          | minijinja (Jinja-like) or askama          |
| Auth               | argon2, jsonwebtoken                      |
| Sessions           | tower-sessions                            |
| Task queue engine  | underway (Postgres) or apalis (multi)     |
| Throttling         | tower-governor                            |
| Caching            | moka, redis                               |
| OpenAPI            | utoipa                                    |
| Logging            | tracing, tracing-subscriber               |
| Dev reload         | cargo-watch, listenfd                     |
| Testing            | axum-test                                 |

> Worth studying for seams (even while building your own): **SeaORM** (ORM on sea-query), **Loco** (full-stack app framework on SeaORM), and **Cot** (a batteries-included Rust framework that builds its own ORM on sea-query and sits on axum, the closest prior art to this project).
