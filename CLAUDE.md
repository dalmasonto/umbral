# CLAUDE.md

Guidance for Claude Code (claude.ai/code) when working in this repository.

## Project status

Greenfield. No Cargo workspace or source yet. Before scaffolding anything, read `arch.md`; that's the authoritative design spec.

umbra is a Django-inspired web framework in Rust (*umbra* is Latin for shadow — the framework lives in Django's shadow in shape, not in code). It's a separate project that shares no code with Django; the goal is to recreate Django's *feeling*: declare data and you get migrations, CRUD, an admin, and an optional REST API almost for free, with Rust's compile-time guarantees. The name is a placeholder; the whole tree can be renamed later with `sed 's/umbra/yourname/g'`.

## The one idea that matters most

**Thin core, plugin-heavy. The framework dogfoods its own plugin system.** Auth, sessions, admin, tasks, and REST are all plugins. Structurally they're identical to a third-party one. A REST-free app has to compile and run with zero serializer code. If a built-in can't be expressed as a plugin, the plugin contract is wrong.

## Crate layout = the architecture

The Cargo workspace boundaries *encode* the core-vs-plugin split:

```
crates/
  umbra-core     # ORM, migrations, routing, DB backends, the Plugin TRAIT. No plugin deps.
  umbra-macros   # #[derive(Model)], #[task], etc.
  umbra          # FACADE: re-exports core + macros as one stable surface (umbra::prelude::*)
  umbra-cli      # the `manage.py` equivalent binary
plugins/         # built-in plugins, each its own crate (from M9 onward)
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

### Never wipe the database (or migration files) to bypass a migration

The whole declare → migrate → change → migrate loop is the product. Running it against a freshly-deleted DB proves nothing — it just bypasses the very test that surfaces real bugs (a UNIQUE addition trips an existing duplicate; a new NOT NULL column needs a default for the backfill; a cross-plugin FK orders wrong against existing rows). **Existing rows are the test, not an obstacle.**

Concretely, when a model change needs a schema migration in any example app or local DB:

- **Never** `rm -f *.db` (or `shop.db`, or any backing store) to "get a clean run." If the migration fails on existing data, that failure is the bug you want to find — diagnose it, fix the model, or write a data migration. The user almost certainly has rows you can't see; deleting their DB silently destroys real state.
- **Never** delete files under `migrations/` to "regenerate cleanly." Migration history is the schema's audit trail; removing entries makes the DB un-migratable from older deploys and erases the record of how each column got its shape.
- The correct flow is two commands in order: `cargo run -- makemigrations` to autodetect the diff and write a new migration file, then `cargo run -- migrate` to apply it against the live DB. If `makemigrations` writes a migration that can't apply (UNIQUE on a column with duplicates, NOT NULL with no default, etc.), that's exactly the surface area the migration engine exists to expose.
- The only legitimate reason to delete an example app's DB is when the user explicitly asks for a fresh demo state. Even then, **ask first** — the user may have populated rows that look like demo data but aren't.

Wiping the DB to bypass a migration is a destructive shortcut in the same category as `git reset --hard` or `--no-verify`: it makes the immediate obstacle go away while hiding the bug that the obstacle was warning about.

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

## Plugins use the ORM. Not raw SQL.

**The ORM is the single database interface.** Plugin code never writes `sqlx::query("INSERT INTO ...")` or `sqlx::query_as("SELECT ...")` directly. Every row-level read or write goes through the ORM, which knows the backend and emits the right SQL.

Why: plugin code that hand-rolls `sqlx::query(...)` ends up SQLite-only (the `?` placeholders the SQLite driver expects don't work on Postgres) and pool-routing-aware (the plugin has to know whether the active pool is `SqlitePool` or `PgPool`). Pushing that routing into the ORM means a plugin author writes one path that works on every backend the framework supports.

What this looks like:

| Operation | Use this | Not this |
|---|---|---|
| Read one row by id | `Session::objects().filter(session::ID.eq(&id)).first().await?` | `sqlx::query_as("SELECT * FROM session WHERE id = ?").bind(&id).fetch_optional(&pool).await?` |
| Insert a row | `Session::objects().create(session).await?` | `sqlx::query("INSERT INTO session ...").bind(...).execute(&pool).await?` |
| Update by predicate | `Session::objects().filter(...).update_values(map).await?` | `sqlx::query("UPDATE session SET ... WHERE ...")...` |
| Delete by predicate | `Session::objects().filter(...).delete().await?` | `sqlx::query("DELETE FROM session WHERE ...")...` |
| Count / exists | `Session::objects().filter(...).count().await?` | `sqlx::query_scalar("SELECT COUNT(*) ...")...` |
| Late-bound model (admin) | `DynQuerySet::for_meta(&meta).filter(...).fetch(...).await?` | — |

The ambient pool resolution is already wired: every QuerySet terminal calls `pool_dispatched()` internally and dispatches per `DbPool::Sqlite | DbPool::Postgres`. Plugin code never types `umbra::db::pool()` or `sqlx::SqlitePool`.

**The narrow exceptions.** Two kinds of raw SQL are allowed because the ORM can't model them at the row level:

1. **Schema DDL** (`CREATE TABLE`, `ALTER TABLE`, `CREATE INDEX`). Owned by the migration engine. A plugin that creates its own tables outside the migration system (e.g. `ensure_tables_for_tests` in umbra-admin) is the lone allowed pattern, and only because tests bypass `make`/`run`.
2. **Backend-specific features the ORM doesn't model** (Postgres RLS policies, full-text indexes, custom triggers). Gate these with `match pool_dispatched() { DbPool::Postgres(_) => ..., DbPool::Sqlite(_) => skip-with-warn }`. Never use the SQLite branch as a fallback path that quietly diverges from the Postgres behaviour.

If the ORM can't express a row-level operation you need, **the right fix is to add the operation to the ORM**, not to write raw SQL in the plugin. The 80% that's already there: filter, order_by, limit, offset, first, fetch, get, count, exists, delete, update_values, update_expr, create, bulk_create, select_related, transactions. If your use case isn't on that list, it's a gap to fix — file a deferred-spec entry and discuss before shipping the raw-SQL workaround.

**Reviewing this rule:** every PR touching a plugin gets a grep for `sqlx::query` / `sqlx::query_as` in `plugins/<name>/src/`. New hits need a comment justifying which exception applies; otherwise the change is rewritten through the ORM.

## Fix, don't patch. Root cause over symptom.

When code is broken because a framework piece doesn't exist yet, **build the framework piece**. Don't paper over the missing surface with a defensive template guard, a `try { ... } catch { swallow }`, a `#[allow(unused)]`, or a "TODO: wire this later" comment that lets the code compile while the underlying behaviour stays broken.

The litmus test: **does the workaround you're about to write hide the bug from the next developer who hits it?** If yes, stop and fix the real thing.

Concretely, in this codebase, these are workarounds — not fixes:

- A template that writes `{% if user is defined and user.is_authenticated and user.is_staff %}` to hide a 500 when the `user_context_layer` middleware isn't mounted. The fix is to mount the middleware (build the `AuthPlugin::with_user_in_templates()` builder + the `Plugin::wrap_router` hook) so `user` IS defined; then the template stays `{% if user.is_staff %}` like the docs claim.
- A `.ok()` that silently discards a secondary error because the recovery path itself failed. The fix is to log the secondary error AND make the recovery path correct; not to keep the silent fallback. See gaps2.md #9 for an example.
- A `Result<T, ()>` that drops the error type because the caller can't be bothered to plumb it. The fix is to plumb it; an error you can't reproduce is one you'll never fix.
- An `unwrap_or_default()` on a value that's never legitimately `None` in production. The fix is to make the type non-optional; `unwrap_or_default()` is a silent data-loss bug waiting for the wrong row to land.
- A `cfg(not(test))` that hides broken-in-test behaviour. The fix is to make it work in tests; otherwise tests prove nothing about production.
- Renaming an unused variable with a `_` prefix to silence a warning instead of asking why it was unused. Often the original author forgot to wire it; the `_` prefix preserves the bug.

The rule applies to the doc-comment surface too. If `plugins/umbra-auth/src/session_user.rs:261` says "opt in via `AuthPlugin::with_user_in_templates`" and the method doesn't exist, **the fix is to write the method**, not to delete the doc-comment claim or to manually wire the middleware in the consumer's main.rs. The docstring describes the framework's intended surface; making the surface match the docstring IS the fix.

When you're tempted to patch:

1. **Name the thing that's broken at the right level.** Not "the template threw" — "the framework promised `user` would be in templates and no middleware mounts it." Patches address symptoms; fixes address contracts.
2. **Find where the contract should live.** Is there already a Plugin trait method, builder hook, or extension point that the broken piece SHOULD have used? If yes, implement it. If no, the gap is "this contract doesn't exist yet" — log it and decide whether to ship the contract now or open a focused PR for it.
3. **If the proper fix is out of scope for this turn**, log a gaps2.md entry with the exact file + line + the contract that's missing, and write the *narrowest possible* workaround at the call site with a `// TODO(gaps2 #N): proper fix lives in <file>` comment that NAMES the gap and points to the file the proper fix would touch. The comment is the breadcrumb the next developer follows.

Workarounds without a logged gap entry are how frameworks accumulate the kind of debt that takes a year to pay down. Every workaround is one entry in the backlog; treat the backlog as load-bearing.

## Commands

The Cargo workspace lives at `crates/Cargo.toml`, **not** at the repo root. Every `cargo` command runs from inside `crates/`:

```bash
cd crates

cargo build                      # build all workspace crates
cargo test                       # run all tests
cargo test -p umbra-core         # test a single crate
cargo test <test_name>           # run a single test by name
cargo run -p umbra-cli -- <cmd>  # the manage.py equivalent (migrate, makemigrations, worker, inspectdb, ...)
cargo clippy --all-targets       # lint
cargo fmt                        # format
```

Build artefacts (`crates/target/`, `crates/Cargo.lock`) are produced inside the same directory and are gitignored. The repo root deliberately has no `Cargo.toml`: this is a multi-purpose tree (framework + docs + Specra site + example apps) rather than a single cargo project.

For an example app outside the framework workspace:

```bash
cd examples/<name>
cargo build                      # standalone Cargo project, ignores the framework workspace
```

`sqlx::query!` compile-time checks need either a live `DATABASE_URL` or a prepared `.sqlx` offline cache once the DB layer exists.

## Working in the workspace

The four core crates connect via a single dependency arrow pointing inward toward the framework's centre:

```
umbra-cli  →  umbra (facade)  →  { umbra-core, umbra-macros }
                  ↑
            plugins/* (from M9 onward, each a separate crate)
```

`umbra` is the **facade**, the only stable surface user code and plugin authors should import. Internal types live in `umbra-core` and `umbra-macros`; the facade re-exports the subset that's stable. When you add something a plugin author needs (a trait, a field type, an extractor, a derive), it goes in `umbra-core` (or `umbra-macros` for a macro) **and** gets a re-export from the facade. The prelude (`umbra::prelude`) re-exports the common subset so `use umbra::prelude::*;` brings in everything a typical handler / model / plugin author needs.

| Adding ... | Goes in ... |
|---|---|
| ORM types, field types, QuerySet methods | `umbra-core` |
| Proc macros (`#[derive(Model)]`, `#[task]`, etc.) | `umbra-macros` |
| CLI binary subcommands (`migrate`, `inspectdb`, …) | `umbra-cli`; per-plugin commands extend it via `Plugin::commands()` from M7+ |
| Built-in plugin logic (auth, sessions, admin, tasks, REST, openapi) | `plugins/<name>/` from M9 onward. Each is its own crate that depends only on the `umbra` facade. |
| A test or smoke-test app that exercises umbra as a consumer would | `examples/<name>/`. Each example is a standalone Cargo project (NOT a workspace member) that path-deps the local umbra. See `examples/README.md`. |
| Helpers used inside one crate only | `pub(crate)` in that crate; do NOT add them to the facade |

**Cargo's ban on circular crate deps enforces the architecture.** That `umbra-core` doesn't depend on `umbra-rest` (or anything under `plugins/*`) is the structural proof that "serializers are a plugin." Don't ever add a dep from `umbra-core` to a plugin; if you find yourself wanting to, the plugin contract is wrong and needs the fix instead.

**Where to expose a new public type.** Three categories:

- **Core surface** (`Plugin`, `Model`, `AppContext`, `Router`, `Request`, `Response`, common field types, common extractors). Add to `umbra-core` (or `umbra-macros`), re-export from `umbra`, include in `umbra::prelude`.
- **Power-user surface** (raw SQL query builders, `DatabaseBackend` trait, the migration engine's operation enum). Add to `umbra-core`, re-export from `umbra` under a module (e.g. `umbra::db::query!`, `umbra::backends::*`), but **not** in the prelude. The prelude stays free of ambiguity.
- **Internal-only**. `pub(crate)` in the originating crate. Never appears in the facade.

## Never stash the user's working tree

`git stash`, `git stash push`, `git stash --keep-index`, `git stash -u`, and the equivalent "park changes elsewhere" moves — `git reset --soft`, `git checkout -- <file>` to clear a dirty path, copying changed files aside before reverting them — **are off-limits without explicit consent for the specific operation**, regardless of permission mode. This applies even on auto-mode / autopilot.

Why: the user works on multiple things in parallel. Their dirty tree IS state — half-finished refactors, in-flight migrations, paste-buffer notes left in a file. `git stash` looks innocuous but the stash is then a needle in a haystack: it doesn't show up in any history view by default, and a later `git stash drop` (or any `git gc --prune` after the stash falls off `git stash list`) silently destroys real work. We already lost a dashboard pass that way; recovering it took `git fsck --unreachable` and SHA-by-SHA blob fishing.

Concretely:

- **Don't** `git stash` to "clear the tree" so a `cargo` command works. Investigate why the dirty state is in the way; usually the right move is to leave it alone.
- **Don't** stash to switch contexts within a session ("let me try something else for a minute"). The user is the one steering context; if you need a clean tree, ask them first.
- **Don't** quietly undo dirty changes via `git checkout -- <file>` or `git restore <file>` either — same family of "silently throw the user's work away" anti-patterns.
- If a `git` command genuinely needs a clean tree (`rebase`, `bisect`, `pull --rebase`), surface the conflict and let the user decide whether to stash, commit, or abandon. Don't pick for them.

The exception is when the user says, in this session, "stash this" or "park these changes" or otherwise explicitly authorizes the move. That authorization doesn't carry across sessions or to a different set of changes — every stash is one explicit ask.

This rule is in the same family as [never wipe the database to bypass a migration](#never-wipe-the-database-or-migration-files-to-bypass-a-migration): a destructive shortcut that makes the immediate obstacle go away while hiding the work it just destroyed.

## Commit cadence

**One feature, one fix, one commit.** Don't batch unrelated changes. If a feature took multiple WIP commits during development, squash them before merging into the public history.

**Before every commit, verify the whole workspace** (not just the crate you changed; a change in `umbra-core` can silently break the facade's re-exports):

```bash
cargo fmt
cargo clippy --all-targets
cargo build
cargo test
```

If any of those fail, fix them or back out the change. Don't commit broken code, and never use `--no-verify` to skip pre-commit hooks. Investigate and fix the underlying issue instead.

**Multi-crate commits are fine when they're one logical change.** A feature that adds a new field type genuinely touches `umbra-core` (the type and `FieldSpec`), `umbra-macros` (so `#[derive(Model)]` handles it), and `umbra` (the re-export). That's one commit, not three, because reverting it as a unit is the only sensible undo.

**Commit message form.**

- First line ≤ 72 characters, imperative voice, with an optional `<type>(<scope>):` prefix. Types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `perf`. Scopes: a crate name (`orm`, `migrate`, `plugin-contract`) or `workspace` for cross-crate. Examples:
  - `feat(orm): add F-expression support to QuerySet`
  - `fix(migrate): handle nullable column rename safely`
  - `docs(specs): clarify the Manager::on(&pool) escape hatch`
- Body explains *why*, not what. The diff shows what; future readers want to know the reason.
- For a commit that closes an open question from a spec, name the spec and the question number in the body (`Closes spec 02 open question #2`).

**When in doubt, ask before committing.** Especially for cross-crate refactors, destructive operations (deleting code, renaming public types), or anything that would force a downstream plugin to change. The cost of pausing to confirm is low; the cost of an unwanted commit on a shared branch can be high.

## Writing conventions

These apply to every internal spec (`arch.md`, `umbra-PRD.md`, `docs/specs/`, the design notes under `docs/decisions/`) and to user-facing MDX in `documentation/`.

### Line wrapping

Don't hard-wrap prose at any column. A sentence or a paragraph stays on a single line; editors handle visual wrapping per the reader's setting. Code blocks, tables, and ASCII diagrams are exempt: their line breaks are meaningful and have to be preserved.

When editing an existing wrapped doc, unwrap the prose lines you touch (and ideally any nearby paragraphs in the same section) so the file converges on the convention rather than carrying mixed styles forever.

## Documentation

Two kinds of documentation live in this repo, and they serve different audiences:

- **Internal design specs.** `arch.md`, `umbra-PRD.md`, `docs/specs/` (deep specs + `outlines/` for M7–M13 + `deferred.md` for the post-M13 backlog), `docs/decisions/` (ADR-style design notes). For us and future contributors. Format: plain Markdown. The source of truth for *why* and *how* the framework is built.
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

## Gap trackers: numbers are identifiers, order is ascending

The trackers under `planning/` (`gaps.md`, `gaps2.md`, `features.md`, plus `planning/archive/*-done.md`) use entry numbers as stable identifiers — commits, code comments, and memories cite them as `gaps2 #N`. Rules, all of them hard:

- **New entry = max + 1, appended at the END of the file.** The max is taken across the active file AND its archive (a closed entry's number stays taken forever). Never insert an entry mid-file; the file reads top-to-bottom in ascending numeric order.
- **Never renumber or reuse a number that has been committed or cited.** If two parallel writers collide on a number, the committed/cited entry keeps it and the uncommitted one takes the next free number.
- **Never reorder existing entries** except to repair an ordering violation — and a repair moves blocks verbatim, changing zero content.
- **Closing an entry**: the full shipped write-up goes verbatim to `planning/archive/<file>-done.md` under the same number; the active entry shrinks to a one-line `[x] <title> — archived` stub in place (it does NOT move).

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

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **umbra** (11892 symbols, 25823 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> If any GitNexus tool warns the index is stale, run `npx gitnexus analyze` in terminal first.

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `gitnexus_impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `gitnexus_detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `gitnexus_query({query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `gitnexus_context({name: "symbolName"})`.

## Never Do

- NEVER edit a function, class, or method without first running `gitnexus_impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `gitnexus_rename` which understands the call graph.
- NEVER commit changes without running `gitnexus_detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/umbra/context` | Codebase overview, check index freshness |
| `gitnexus://repo/umbra/clusters` | All functional areas |
| `gitnexus://repo/umbra/processes` | All execution flows |
| `gitnexus://repo/umbra/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->
