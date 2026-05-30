# CLAUDE.md

Guidance for Claude Code (claude.ai/code) when working in this repository.

## Project status

**Greenfield.** The repo currently contains only the design spec, `arch.md`. There is no Cargo
workspace, source code, or git history yet. **Read `arch.md` before scaffolding or implementing
anything** — it is authoritative.

**umbra** is a Django-equivalent web framework in Rust ("a Django shadow"; *umbra* = Latin for
shadow). The goal is to recreate Django's *feeling* — declare data → get migrations, CRUD, an
admin, and an optional REST API almost for free — while gaining Rust's compile-time guarantees.
The name is a placeholder; the whole tree can be renamed later with `sed 's/umbra/yourname/g'`.

## The one idea that matters most

**Thin core + plugin-heavy, with the framework dogfooding its own plugin system.** Auth,
sessions, admin, tasks, and REST are *all* plugins — structurally identical to third-party
ones. A REST-free app must compile and run with zero serializer code. If a built-in can't be
expressed as a plugin, the plugin contract is wrong.

## Crate layout = the architecture

The Cargo workspace boundaries *encode* the core-vs-plugin split:

```
umbra-core     # ORM, migrations, routing, DB backends, the Plugin TRAIT. No plugin deps.
umbra-macros   # #[derive(Model)], #[task], etc.
umbra          # FACADE: re-exports core + macros as one stable surface (umbra::prelude::*)
umbra-cli      # the `manage.py` equivalent binary
plugins/
  umbra-auth      # built-in: users, permissions, password hashing (argon2)
  umbra-sessions  # built-in: session store + middleware (tower-sessions)
  umbra-admin     # built-in: auto CRUD UI
  umbra-tasks     # built-in: DB-backed task queue (Celery equivalent)
  umbra-rest      # OPTIONAL: serializers, viewsets, routers (the "DRF")
  umbra-openapi   # OPTIONAL: Swagger UI / schema gen; depends on umbra-rest
```

## Dependency inversion is the whole game

- **Dependencies point inward toward core; control flows outward through the trait.**
- Every plugin depends on the `umbra` facade — **never the reverse**. `umbra-core` defines the
  `Plugin` *trait* but never names a concrete plugin; it touches plugins only as
  `Box<dyn Plugin>`. That trait object is the dynamic seam standing in for Django's
  `INSTALLED_APPS`.
- The user's **binary crate** depends on core + every chosen plugin and wires them via an
  explicit builder: `App::builder().plugin(...).build()`.
- `umbra-core` depending on neither `umbra-rest` nor `umbra-openapi` is the *structural proof*
  that "serializers are a plugin." Cargo's ban on circular crate deps enforces this rather than
  fighting it.
- **Plugins import only the facade** (`use umbra::prelude::*`), never `umbra-core` internals —
  so the internal crate split can be refactored without breaking any plugin.

## The Plugin contract

A plugin (Django's "app") implements the `Plugin` trait and may contribute any subset of:
models (→ migrations), routes/views, middleware, management commands, a typed settings schema +
defaults, admin registrations, and lifecycle hooks (`on_ready()` ≈ `AppConfig.ready()`).

Each plugin **owns its migrations**. `migrate` walks every registered plugin, collects
`plugin.migrations()`, orders them by a dependency graph (cross-plugin FKs allowed), and runs
only those not yet recorded in an umbra-owned tracking table. The built-in auth/sessions/tasks
tables are created this exact way — not special-cased.

## Ambient ORM access — decide deliberately

For `Post::objects().filter(...)` to work without threading a pool through every call, store the
`DbPool` in a `OnceLock` set during `App::build()` so managers read it ambiently — while still
allowing an explicit pool in tests. This is the **one** intentional global; don't let others
creep in.

## North star: managed migrations from day one

The everyday loop must work from the first milestone that has models, exactly like Django:

1. Declare or change a model → an autodetected migration is generated.
2. `migrate` applies all pending migrations to the database.
3. Update or delete a model → the diff produces the right `ALTER` / `DROP` migration.

This **declare → migrate → change → migrate** cycle *is* the product, not a later feature. Two
capabilities make it real:

- **Autodetection** — diff the current models against the last migration snapshot, emit ordered,
  reversible operations (create/alter/drop table, add/alter/drop column). Ship the basic cases
  (new/dropped model, added/removed/altered field) on day one.
- **`inspectdb`** — the porting on-ramp: introspect an existing DB → generate models, so an
  existing schema drops straight into the same managed-migration loop.

The genuinely hard cases Django spent years on — rename vs. drop+add disambiguation,
data-preserving alters, complex constraint changes — are **iterated, not gated on**.

## Build order

Build the primitives by hand first, then extract abstractions. Managed migrations are not
deferred — the declare → migrate loop lands as soon as models exist. Full rationale in
`arch.md §7`.

- **M0** — workspace + typed settings + sqlx pool + one hand-written route.
- **M1** — QuerySet builder → SQL for one hard-coded model (no macros). *(deepest Rust lesson)*
- **M2–M3** — implement the `Model` trait by hand, *then* generate that exact impl via
  `#[derive(Model)]`. Macros are easy once the target output is known.
- **M4** — backend abstraction + boot-time system check (field/backend compatibility).
- **M5** — **migration engine**: model-state snapshot, basic autodetection (create/alter/drop),
  tracking table, and `migrate`. The full declare → migrate → change → migrate loop works here.
- **M6** — `inspectdb`: introspect an existing DB → models that feed straight into M5.
- **M7** — extract the `Plugin` trait (architectural keystone); routes/migrations/commands flow
  through it, and `migrate` extends to walk all registered plugins.
- **M8** — harden autodetection (rename detection, data migrations, cross-plugin FK ordering)
  and re-express auth + sessions as plugins (proves the contract).
- **M9–M13** — `umbra-tasks`, `umbra-rest`, `umbra-admin`, `umbra-openapi`, then polish
  (generators, autoreload, docs).

## Design principles to uphold

- **Don't reimplement primitives** (HTTP, async, SQL generation, JSON). Reimplement *conventions
  and integration* — stand on crates; the value is the glue. Crate shortlist in `arch.md §8`
  (axum, sqlx, sea-query/sea-schema, syn/quote, serde, clap, …).
- **Make the easy path the safe path** via the type system: nullable column → `Option<T>`;
  errors are `Result` values with a framework error enum + `From` impls so `?` flows; prefer
  `sqlx::query!` compile-time-checked queries.
- **Secure by default**: CSRF, clickjacking/HSTS headers, template autoescaping, always-
  parameterized SQL.
- **Backend mismatches caught at boot, not in prod**: a field declares which backends it supports
  (e.g. `ArrayField` → Postgres only); the startup system check fails with a clear message on an
  incompatible field. **Postgres-first**, SQLite for tests.

## Commands

No build tooling exists yet. Once scaffolded it is a standard Cargo workspace:

```bash
cargo build                      # build all workspace crates
cargo test                       # run all tests
cargo test -p umbra-core         # test a single crate
cargo test <test_name>           # run a single test by name
cargo run -p umbra-cli -- <cmd>  # the manage.py equivalent (migrate, makemigrations, worker, inspectdb, …)
cargo clippy --all-targets       # lint
cargo fmt                        # format
```

`sqlx::query!` compile-time checks need either a live `DATABASE_URL` or a prepared `.sqlx`
offline cache once the DB layer exists.

## Documentation

The repo has **two** kinds of documentation, and they serve different audiences:

- **Internal design specs** — `arch.md`, `umbra-PRD.md`, `docs/specs/`,
  `docs/specs/outlines/`. Audience: us (and future contributors). Format:
  plain Markdown. Source of truth for *why* and *how* the framework is built.
- **User-facing docs** — `documentation/` (a SvelteKit + Specra site, served
  from `documentation/docs/v0.0.1/`). Audience: people using umbra to build
  apps. Format: **MDX** (`.mdx`, not `.md`) using Specra components. Component
  catalog: https://specra-docs.com/docs/v1.0.0/en/components/accordion.

### Rule: ship a feature, ship its doc page

When a feature lands (a model field type, a CLI command, a plugin capability,
a middleware, an extractor — anything a user can write code against), add a
minimal user-facing page in the *same* commit or PR. Basic info only —
**not** a full reference:

- **Purpose** — one paragraph, what it is and when you'd reach for it.
- **One example** — the smallest piece of code that uses it.
- **Link to the spec** — point at `arch.md` or the relevant `docs/specs/*.md`
  for the design rationale.

Reference-depth pages come later, once the surface stabilizes.

### Where the page goes

```
documentation/docs/v0.0.1/<area>/<feature>.mdx
```

`<area>` is one of `orm`, `migrations`, `web`, `cli`, `plugins`, `auth`,
`sessions`, `tasks`, `rest`, `admin`, `openapi` (create the folder + a
`_category_.json` the first time you use a new area). Frontmatter is required:
`title`, `description`, `sidebar_position`, and optionally `icon`,
`tab_group`, `tags`.

### MDX & Specra conventions

- File extension is `.mdx`. Use Markdown for prose, Specra components for
  structure (`<Callout>`, `<CardGrid>`/`<Card>`, `<Steps>`/`<Step>`,
  `<Tabs>`/`<Tab>`, `<Accordion>`, `<Badge>`, fenced code blocks).
- Don't import components — Specra makes them globally available.
- A folder needs a `_category_.json` to control sidebar label, order, and
  collapse behavior.

### What NOT to add

Don't translate internal specs into user-facing docs. The spec is the spec;
the user page is the smallest useful slice for someone using the feature. If
you find yourself rewriting `arch.md` in MDX, stop — link to it instead.

## Prior art worth studying

**Cot** (Django-like; builds its own ORM on sea-query + axum — closest prior art), **Loco**
(Rails-style on SeaORM), and **SeaORM** itself (ORM on sea-query).