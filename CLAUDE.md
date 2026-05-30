# CLAUDE.md

Guidance for Claude Code (claude.ai/code) when working in this repository.

## Project status

Greenfield. No Cargo workspace or source yet. Before scaffolding anything, read `arch.md`; that's the authoritative design spec.

umbra is a Django-equivalent web framework in Rust ("a Django shadow"; *umbra* is Latin for shadow). The goal is to recreate Django's *feeling*: declare data and you get migrations, CRUD, an admin, and an optional REST API almost for free, with Rust's compile-time guarantees. The name is a placeholder; the whole tree can be renamed later with `sed 's/umbra/yourname/g'`.

## The one idea that matters most

**Thin core, plugin-heavy. The framework dogfoods its own plugin system.** Auth, sessions, admin, tasks, and REST are all plugins. Structurally they're identical to a third-party one. A REST-free app has to compile and run with zero serializer code. If a built-in can't be expressed as a plugin, the plugin contract is wrong.

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

- **Dependencies point inward toward core. Control flows outward through the trait.**
- Every plugin depends on the `umbra` facade, never the reverse. `umbra-core` defines the `Plugin` trait but never names a concrete plugin; it touches plugins only as `Box<dyn Plugin>`. That trait object is the dynamic seam standing in for Django's `INSTALLED_APPS`.
- The user's binary crate depends on core plus every chosen plugin and wires them via an explicit builder: `App::builder().plugin(...).build()`.
- `umbra-core` depends on neither `umbra-rest` nor `umbra-openapi`. That's the structural proof that "serializers are a plugin." Cargo's ban on circular deps enforces it for us.
- Plugins import only the facade (`use umbra::prelude::*`), never `umbra-core` internals. The internal crate split can then be refactored without breaking any plugin.

## The Plugin contract

A plugin (Django's "app") implements the `Plugin` trait. It can contribute any subset of: models (which become migrations), routes and views, middleware, management commands, a typed settings schema with defaults, admin registrations, and lifecycle hooks (`on_ready()` is the Rust version of `AppConfig.ready()`).

Each plugin owns its own migrations. `migrate` walks every registered plugin, collects `plugin.migrations()`, orders them by a dependency graph (cross-plugin FKs allowed), and runs only those not yet recorded in an umbra-owned tracking table. The built-in auth, sessions, and tasks tables are created this exact way. Nothing is special-cased.

## Ambient ORM access: decide deliberately

For `Post::objects().filter(...)` to work without threading a pool through every call, store the `DbPool` in a `OnceLock` set during `App::build()`. Managers read it ambiently, but tests can still pass an explicit pool. This is the one intentional global. Don't let others creep in.

## North star: managed migrations from day one

The everyday loop has to work from the first milestone that has models, exactly like Django:

1. Declare or change a model. An autodetected migration is generated.
2. `migrate` applies all pending migrations to the database.
3. Update or delete a model. The diff produces the right `ALTER` / `DROP` migration.

This **declare → migrate → change → migrate** cycle *is* the product. It's not a later feature. Two capabilities make it real:

- **Autodetection.** Diff the current models against the last migration snapshot, emit ordered, reversible operations (create/alter/drop table, add/alter/drop column). Ship the basic cases (new/dropped model, added/removed/altered field) on day one.
- **`inspectdb`.** The porting on-ramp. Introspect an existing DB and generate models so an existing schema drops straight into the same managed-migration loop.

The hard cases Django spent years on (rename vs. drop+add disambiguation, data-preserving alters, complex constraint changes) get iterated on. They don't gate anything.

## Build order

Build the primitives by hand first, then extract abstractions. Managed migrations aren't deferred; the declare → migrate loop lands as soon as models exist. Full rationale in `arch.md §8`.

- **M0.** Workspace, typed settings, sqlx pool, one hand-written route.
- **M1.** QuerySet builder → SQL for one hard-coded model, no macros. *(deepest Rust lesson)*
- **M2–M3.** Implement the `Model` trait by hand, then generate that exact impl via `#[derive(Model)]`. Macros are easy once the target output is known.
- **M4.** Backend abstraction plus a boot-time system check for field/backend compatibility.
- **M5.** The migration engine: model-state snapshot, basic autodetection (create/alter/drop), tracking table, and `migrate`. The full declare → migrate → change → migrate loop works here.
- **M6.** `inspectdb`: introspect an existing DB into models that feed straight into M5.
- **M7.** Extract the `Plugin` trait (the architectural keystone). Routes, migrations, and commands flow through it, and `migrate` extends to walk all registered plugins.
- **M8.** Harden autodetection (rename detection, data migrations, cross-plugin FK ordering), and re-express auth and sessions as plugins. That's the proof of the contract.
- **M9–M13.** `umbra-tasks`, `umbra-rest`, `umbra-admin`, `umbra-openapi`, then polish (generators, autoreload, docs).

## Design principles to uphold

- **Don't reimplement primitives** (HTTP, async, SQL generation, JSON). Reimplement conventions and integration. Stand on crates; the value is the glue. Crate shortlist in `arch.md §9` (axum, sqlx, sea-query/sea-schema, syn/quote, serde, clap, etc.).
- **Make the easy path the safe path** via the type system. A nullable column becomes `Option<T>`. Errors are `Result` values with a framework error enum plus `From` impls so `?` flows. Prefer `sqlx::query!` for compile-time-checked queries.
- **Secure by default.** CSRF, clickjacking/HSTS headers, template autoescaping, always-parameterized SQL.
- **Backend mismatches caught at boot, not in prod.** A field declares which backends it supports (e.g. `ArrayField` is Postgres only); the startup system check fails with a clear message on an incompatible field. **Postgres-first**, SQLite for tests.

## Commands

No build tooling exists yet. Once scaffolded it is a standard Cargo workspace:

```bash
cargo build                      # build all workspace crates
cargo test                       # run all tests
cargo test -p umbra-core         # test a single crate
cargo test <test_name>           # run a single test by name
cargo run -p umbra-cli -- <cmd>  # the manage.py equivalent (migrate, makemigrations, worker, inspectdb, ...)
cargo clippy --all-targets       # lint
cargo fmt                        # format
```

`sqlx::query!` compile-time checks need either a live `DATABASE_URL` or a prepared `.sqlx` offline cache once the DB layer exists.

## Writing conventions

These apply to every internal spec (`arch.md`, `umbra-PRD.md`, `docs/specs/`, the spec-set design under `docs/superpowers/specs/`) and to user-facing MDX in `documentation/`.

### Line wrapping

Don't hard-wrap prose at any column. A sentence or a paragraph stays on a single line; editors handle visual wrapping per the reader's setting. Code blocks, tables, and ASCII diagrams are exempt: their line breaks are meaningful and have to be preserved.

When editing an existing wrapped doc, unwrap the prose lines you touch (and ideally any nearby paragraphs in the same section) so the file converges on the convention rather than carrying mixed styles forever.

## Documentation

Two kinds of documentation live in this repo, and they serve different audiences:

- **Internal design specs.** `arch.md`, `umbra-PRD.md`, `docs/specs/`, `docs/specs/outlines/`. For us and future contributors. Format: plain Markdown. The source of truth for *why* and *how* the framework is built.
- **User-facing docs.** `documentation/` (a SvelteKit + Specra site, served from `documentation/docs/v0.0.1/`). For people using umbra to build apps. Format: **MDX** (`.mdx`, not `.md`) using Specra components. Component catalog: https://specra-docs.com/docs/v1.0.0/en/components/accordion.

### Rule: ship a feature, ship its doc page

When a feature lands (a model field type, a CLI command, a plugin capability, a middleware, an extractor, anything a user can write code against), add a minimal user-facing page in the same commit or PR. Basic info only, **not** a full reference:

- **Purpose.** One paragraph: what it is and when you'd reach for it.
- **One example.** The smallest piece of code that uses it.
- **Link to the spec.** Point at `arch.md` or the relevant `docs/specs/*.md` for the design rationale.

Reference-depth pages come later, once the surface stabilizes.

### Where the page goes

```
documentation/docs/v0.0.1/<area>/<feature>.mdx
```

`<area>` is one of `orm`, `migrations`, `web`, `cli`, `plugins`, `auth`, `sessions`, `tasks`, `rest`, `admin`, `openapi`. The first time you use a new area, create the folder and add a `_category_.json`. Frontmatter is required: `title`, `description`, `sidebar_position`, and optionally `icon`, `tab_group`, `tags`.

### MDX and Specra conventions

- File extension is `.mdx`. Use Markdown for prose, Specra components for structure (`<Callout>`, `<CardGrid>`/`<Card>`, `<Steps>`/`<Step>`, `<Tabs>`/`<Tab>`, `<Accordion>`, `<Badge>`, fenced code blocks).
- Don't import components. Specra makes them globally available.
- A folder needs a `_category_.json` to control sidebar label, order, and collapse behavior.

### What NOT to add

Don't translate internal specs into user-facing docs. The spec is the spec. The user page is the smallest useful slice for someone using the feature. If you find yourself rewriting `arch.md` in MDX, stop and link to it instead.

## Skills: capture what you learn, as you learn it

As you work on umbra, write skills. A skill is a small instruction file the next agent (or you, weeks later) can load to skip the re-discovery you just did. The goal is an incremental library that grows with the codebase.

Three things are worth a skill:

- **How you solved something.** A specific problem and the fix that worked, with enough context that someone hitting the same wall finds the skill.
- **How something works.** Mechanics that aren't obvious from the code: how `sqlx::query!` resolves columns at compile time, how a derive macro expands, how `tower::Layer` composes through the middleware chain.
- **How something is wired together.** Configuration that spans multiple files or crates: how the Plugin trait dispatches, how the autodetector reads model snapshots, how cargo features gate optional plugins.

### Where they go

```
.claude/skills/<slug>.md
```

Use a kebab-case slug that describes the trigger. Examples: `sqlx-offline-cache.md`, `plugin-trait-dispatch.md`, `derive-model-expansion.md`, `migration-snapshot-format.md`.

### Structure

Every skill is markdown with YAML frontmatter:

```markdown
---
name: <slug>
description: <trigger sentence: "Use when X" or "Triggers on Y">
---

# <Title>

## Context
What problem this skill solves and when it applies.

## Approach
The procedure or explanation. Numbered steps if procedural; named sections if explanatory.

## Why
The reasoning behind this approach. What the alternatives were and why they lost.

## Pitfalls
Edge cases, common confusions, things that already bit someone.

## See also
Links to `arch.md` sections, `docs/specs/` files, or other skills.
```

The `description` field matters most. It's what shows up in the skill catalog and what future agents read to decide whether to load the skill. Write it as a trigger ("Use when debugging sqlx compile errors against the offline cache"), not as a summary ("This skill is about sqlx").

### When to write one

Right after you understand or solve something, while the context is fresh. Don't batch this. A skill written days later loses the specifics that made it useful in the first place.

### What NOT to capture

- Things obvious from reading the code.
- One-off trivia that won't recur.
- Restatements of `arch.md`, the PRD, or `docs/specs/`. Link to those instead.
- Opinions without rationale.

## Prior art worth studying

**Cot** (Django-like; builds its own ORM on sea-query + axum, the closest prior art), **Loco** (Rails-style on SeaORM), and **SeaORM** itself (ORM on sea-query).
