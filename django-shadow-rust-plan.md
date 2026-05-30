# Building a Django Shadow in Rust — Architecture, Features & Build Strategy

The goal is not to clone Django line-for-line, but to recreate the *feeling*: declare your
data, get migrations, CRUD, an admin, and (optionally) an API almost for free — while gaining
Rust's compile-time resilience. The framework is **thin-core + plugin-heavy**, and it
**dogfoods its own plugin system** for every built-in feature.

> **Project name: `umbra`** (Latin for *shadow* — a Django shadow). Placeholder; rename the
> whole tree later with a single `sed 's/umbra/yourname/g'`. Convention: the facade crate is
> `umbra`, internals are `umbra-*`, and third-party plugins follow `umbra-<thing>` the way
> Django plugins are `django-<thing>`.

---

## 0. The North Star: make *porting* easy

Django makes porting an existing API trivial because of two features above all others:

1. **`inspectdb`** — point it at an existing database and it generates models.
2. **Managed migrations** — describe the desired schema; it computes the steps.

If the framework nails introspection + migrations, it captures most of the "just port it"
experience. These are priority features, not afterthoughts.

**Design principle:** Don't reimplement primitives (HTTP, async, SQL generation, JSON).
Reimplement *conventions and integration*. Stand on crates; the value is the glue.

---

## 1. Architectural Pillars

### Pillar 1 — Thin core, everything else is a plugin
The core ships only what every app needs no matter what. Serializers/REST, admin, auth,
sessions, and the task queue are **all plugins** — some shipped in the box, structurally
identical to third-party ones. A REST-free app must work with zero serializer code.

### Pillar 2 — The framework dogfoods its own plugin system
This mirrors Django's `contrib` apps. Auth, admin, sessions, and tasks register through the
same mechanism, own their own migrations, and expose routes/commands the same way a stranger's
plugin would. If the built-ins can't be expressed as plugins, the plugin contract is wrong.

### Pillar 3 — Cargo workspace encodes core-vs-plugin
The crate boundaries *are* the architecture:

```
workspace/
├── umbra-core        # ORM, migrations, routing, DB backends, the Plugin TRAIT. Depends on nothing plugin-related.
├── umbra-macros      # #[derive(Model)], #[task], etc.
├── umbra             # FACADE: re-exports core (+ macros) as one stable surface → `use umbra::prelude::*`
├── umbra-cli         # the `manage.py` equivalent binary
├── plugins/
│   ├── umbra-auth     # built-in plugin: users, permissions, password hashing
│   ├── umbra-sessions # built-in plugin: session store + middleware
│   ├── umbra-admin    # built-in plugin: auto CRUD UI
│   ├── umbra-tasks    # built-in plugin: DB-backed Celery-equivalent
│   ├── umbra-rest     # OPTIONAL plugin: serializers, viewsets, routers (the "DRF")
│   └── umbra-openapi  # OPTIONAL plugin: Swagger UI / schema gen, depends on umbra-rest
```

**Dependency direction (this is the whole game):** every plugin depends on `umbra` (the
facade), never the reverse. `umbra-core` defines the `Plugin` *trait* but never names a
concrete plugin. The user's **binary crate** depends on `umbra` + all chosen plugins and wires
them. So arrows point *inward* toward core; control flows *outward* through the trait. Cargo's
ban on circular crate deps doesn't fight this — it enforces it. REST being a crate core does
not depend on is the structural proof that "serializers are a plugin." OpenAPI depends on REST;
core depends on neither.

---

## 2. The Plugin Contract (the heart of extensibility)

A plugin (Django's "app") is a unit that can contribute any subset of:

- **Models** → and therefore **migrations** (the killer requirement below)
- **Routes / views**
- **Middleware**
- **Management commands** (extend `manage.py`)
- **Settings schema + defaults** (typed config the plugin owns)
- **Admin registrations**
- **Signals / lifecycle hooks** (an `on_ready()` equivalent of Django's `AppConfig.ready()`)

### How a plugin is written (and why there is no cycle)

This is the crux of the whole ecosystem. Django plugins import the framework (`from django.db
import models`) but the framework never imports the plugin — it discovers plugins through
`INSTALLED_APPS` *strings* resolved at runtime. One-directional static dependency; dynamic
config-based discovery. Replicate it with **dependency inversion**:

1. `umbra-core` owns the ORM and **defines the `Plugin` trait** (the contract). Depends on no plugin.
2. A plugin depends on the `umbra` facade, implements `Plugin`, and `use`s the ORM freely —
   the direct equivalent of `from django.db import models`. It "magically" has the ORM because
   it imports it.
3. The user's **binary** depends on core + every plugin and composes them.
4. Core only ever touches plugins as `Box<dyn Plugin>` — the trait object is the dynamic seam
   that stands in for `INSTALLED_APPS`. Core reaches plugin code without statically naming it.

Mantra: **dependencies point inward toward core; control flows outward through the trait.**
Cargo forbids circular crate deps, which simply enforces this architecture.

A complete third-party plugin looks like this — note it imports nothing but the facade:

```rust
use umbra::prelude::*;            // ORM, Plugin trait, routing — one stable surface

#[derive(Model)]                  // "magic" ORM access, because it depends on umbra
pub struct Post { /* fields */ }

pub struct BlogPlugin;

impl Plugin for BlogPlugin {
    fn name(&self) -> &str { "blog" }
    fn migrations(&self) -> Vec<Migration> { generated_migrations!() } // auto-run on `migrate`
    fn routes(&self) -> Router { /* ... */ }
    fn commands(&self) -> Vec<Command> { vec![] }
    fn on_ready(&self, _ctx: &AppContext) {}                           // AppConfig.ready()
}
```

The author experience is: `cargo add umbra-blog`, then register it (see below). The **facade +
prelude** is what keeps this clean — authors never reach into `umbra-core` internals, so you
can refactor the internal crate split without breaking a single plugin.

### Ambient ORM access (the one place to decide deliberately)
For `Post::objects().filter(...)` to work without threading a pool through every call (the
Django feel), managers need ambient access to the DB pool. Django uses a global app registry;
idiomatic Rust threads `State<Pool>` explicitly. The clean compromise: store the pool in a
`OnceLock<DbPool>` set during `App::build()` so managers can read it ambiently, while still
allowing an explicit pool to be passed in tests. Choose this on purpose — don't let globals
creep in by accident.

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

Each plugin implements a `Plugin` trait whose methods return its migrations, routes, commands,
and config defaults. Explicit builder registration is the Django-`INSTALLED_APPS`-like,
debuggable default. For the zero-boilerplate "`cargo add umbra-blog` and it just works"
experience, *optionally* layer in `inventory`/`linkme` distributed slices so a plugin
self-registers at static-init — no `.plugin()` call needed. One caveat to know: the linker
drops crates nothing references, so the binary must still list the plugin as a dependency for
auto-registration to fire (which it does the moment you `cargo add` it).

### Automagic migrations on `migrate`
This is the requirement you called out. Once a plugin is registered:

1. `manage.py migrate` walks every registered plugin and collects `plugin.migrations()`.
2. Migrations are ordered by a dependency graph (cross-plugin FKs allowed).
3. Applied migrations are tracked in a umbra-owned table; only new ones run.
4. A third-party plugin "just works" — drop it in, register it, `migrate`, done.

The framework's own auth/sessions/tasks tables are created this exact way — they are plugin
migrations, not special-cased.

---

## 3. Core Feature Inventory (truly core only)

**reuse** = lean on a crate · **build** = your framework's real work.

### 3.1 Settings & Registry
- **Centralized, typed, environment-aware settings.** *(reuse: figment/config; build conventions)*
- **Plugin registry + builder.** *(build — Pillar 2)*
- **System check framework** — validate config & backend compatibility at boot. *(build)*

### 3.2 ORM / Data Layer *(most difficulty and value)*
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

### 3.3 Migrations *(porting superpower)*
- **`inspectdb`** — introspect existing DB → models. *(build on sqlx/sea-schema introspection)*
- **Autodetection** — diff model snapshot → ops. *(build — the intricate part)*
- **Dependency graph, reversibility, data migrations.** *(build)*
- **Squashing & fake migrations.** *(build, later)*

### 3.4 Routing, Views, Middleware
- **URL routing** — patterns, includes, namespaces, reverse(). *(build on axum)*
- **Views** — function handlers + generic class-equivalents via traits + composition (no inheritance). *(reuse + build)*
- **Middleware stack.** *(reuse: tower; build conventions)*
- **Request/response, file uploads, content negotiation.** *(reuse: axum/http)*

### 3.5 Security (secure by default)
- **CSRF, clickjacking headers, HSTS.** *(reuse: tower-http; build CSRF)*
- **XSS** — template autoescaping. *(reuse: minijinja/askama)*
- **SQL injection** — parameterized always. *(reuse: sqlx — free)*
- **Secret signing.** *(reuse: hmac/ring)*

### 3.6 CLI / Tooling
- **`manage.py` equivalent** — extensible subcommands. *(reuse: clap; build extension point)*
- **`startproject`/`startapp`/generators.** *(build: CLI)*
- **Dev server + autoreload.** *(reuse: cargo-watch/listenfd)*
- **Fixtures, test client, rich error pages.** *(build on serde/axum-test)*

### 3.7 Caching (core utility, pluggable backends)
- **Cache framework** — in-memory/Redis; per-view/fragment/low-level. *(reuse: moka/redis; build API)*

---

## 4. Database Backends & DB-Specific Fields

Django abstracts most differences but also exposes backend-specific power (`django.contrib.postgres`:
`ArrayField`, `HStoreField`, range fields, full-text search). Replicate both halves.

### 4.1 Backend abstraction
- A `DatabaseBackend` trait covering dialect differences: type mapping, identifier quoting,
  upsert syntax, `RETURNING` support, etc. *(reuse: sea-query already abstracts dialects; sqlx abstracts drivers — Postgres/MySQL/SQLite)*

### 4.2 Backend-specific fields with guardrails *(a resilience win)*
- A field declares the backends it supports (e.g. `ArrayField` → `[Postgres]`).
- The **system check at boot** verifies every model's fields are compatible with the active
  backend. Using a Postgres-only field on MySQL fails **at startup with a clear message**,
  not at query time in production.
- Recommended default: target Postgres first (richest feature set), keep SQLite for tests,
  and let the check gate everything else. "Use Postgres to avoid surprises" becomes an
  *enforced* invariant rather than a hope.

---

## 5. Built-in Plugins (shipped in the box, structurally ordinary)

### 5.1 `umbra-auth`
User model (incl. custom user models), authentication backends, permissions & groups,
password hashing *(reuse: argon2)*, login guards. Owns its migrations.

### 5.2 `umbra-sessions`
Session store + middleware. *(reuse: tower-sessions)* Owns its migrations (DB session backend).

### 5.3 `umbra-admin`
Register a model → auto CRUD UI: list display, filters, search, inlines, bulk actions,
permission integration. The flagship "wow" feature. *(build)*

### 5.4 `umbra-tasks` — DB-backed task queue (Celery out of the box)
- **`#[task]` macro / `Task` trait** to define tasks; typed args via serde.
- **DB-backed broker** — owns a `tasks` table via its own migration; no Redis/RabbitMQ required
  to start. *(reuse: `underway` (Postgres-native) or `apalis` (multi-backend) as the engine)*
- **Worker process** — `manage.py worker` polls and executes.
- **Retries, scheduling/periodic ("beat"), result storage.**
- **Pluggable broker later** (Redis, etc.) behind the same task API.
- Because it's DB-backed and registered like any plugin, `migrate` provisions its tables
  automatically — exactly the "plugin owns its migrations" story.

### 5.5 `umbra-rest` — the "DRF" (OPTIONAL)
- **Serializers / ModelSerializer** — struct ↔ JSON + validation. *(reuse: serde; build mapping)*
- **ViewSets & routers** — auto-generate CRUD URL sets. *(build)*
- **Auth/permission/throttle classes, pagination, filtering, ordering.** *(build; reuse tower-governor for throttle)*
- **Renderers / content negotiation; browsable API later.** *(build)*
- Core does **not** depend on this crate. No REST plugin → no serializer overhead.

### 5.6 `umbra-openapi` (OPTIONAL, depends on `umbra-rest`)
- **Auto-generate OpenAPI schema + Swagger UI** from registered viewsets/serializers.
  *(reuse: utoipa; build integration)*

---

## 6. Leaning into Rust's Resilience

Design so users fall into these by default:

- **Illegal states unrepresentable** — nullable column → `Option<T>`; the null *must* be handled.
- **Errors are values** — `Result<T,E>` everywhere, framework error enum + `From` so `?` flows.
- **Compile-time query checks** — `sqlx::query!` fails the build on a bad column, not prod.
- **Backend mismatches caught at boot** — see §4.2; a class of runtime failures becomes a startup error.
- **Fearless concurrency** — `Send`/`Sync` enforced; no GIL → real parallelism for the worker pool.
- **Predictable latency** — no GC pauses.

The framework's job: make the *easy* path the *safe* path.

---

## 7. Revised Build Order (plugin-aware, dogfooding, learning-first)

Each milestone is independently demoable. The plugin contract is extracted *after* you've
built the primitives once, then everything else is re-expressed through it.

**M0 — Foundations.** Workspace skeleton, typed settings, sqlx pool, one hand-written Axum route.
**M1 — QuerySet by hand (no macros).** Builder → SQL for one hard-coded model. *(deepest Rust lesson: ownership, generics, builder pattern)*
**M2 — `Model` trait, implemented manually.** Prove the target before automating.
**M3 — `#[derive(Model)]`.** Generate the M2 impl. Macros are easy once the output is known.
**M4 — Backend abstraction + system check.** sea-query dialects; field/backend compatibility check at boot (§4.2).
**M5 — Migrations: forward-only.** Generate + apply + track `CREATE TABLE` from models.
**M6 — `inspectdb`.** Introspect existing DB → models. *Ship this early — it's the porting payoff.*
**M7 — The Plugin contract.** Extract the `Plugin` trait; routes + migrations + commands flow through it. *(architectural keystone)*
**M8 — Migration autodetection.** Snapshot diff → real migrations, dependency graph; `migrate` walks all plugins.
**M9 — Re-express built-ins as plugins.** Auth + sessions, each owning migrations. Proves the contract.
**M10 — `umbra-tasks` plugin.** DB-backed queue, `#[task]`, `manage.py worker`.
**M11 — `umbra-rest` plugin.** Serializers + viewsets + routers, as an *optional* crate.
**M12 — `umbra-admin` + `umbra-openapi`.** Auto CRUD UI; Swagger from REST.
**M13 — Polish.** Generators, autoreload, browsable API, caching, fixtures.

---

## 8. Crate Shortlist

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

> Worth studying for seams (even while building your own): **SeaORM** (ORM on sea-query),
> **Loco** (Rails-style app framework on SeaORM), and **Cot** (Django-like, builds its own
> ORM on sea-query and sits on axum — closest prior art to this project).