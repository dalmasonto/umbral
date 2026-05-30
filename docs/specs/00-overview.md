# 00 — Overview

| | |
|---|---|
| **Status** | Draft |
| **Maps to milestone** | None (foundation for the spec set) |
| **Companions** | `arch.md`, `umbra-PRD.md`, the per-subsystem specs `01`–`07`, the outlines under `outlines/` |

## Purpose

Index to the umbra deep specs. Read this before the others. It pins down the canonical example app that every spec uses, names the Django ↔ umbra translations, and establishes the naming conventions so the rest of the doc set doesn't drift over time.

## How to read this doc set

The deep specs are numbered in the order someone reading the design from scratch benefits from. The numbering is *not* the build order; build order lives in `arch.md §8`. The spec order is optimised so that each file lands cleanly on top of the concepts the prior one introduced.

1. **`01-app-and-settings.md`.** How an umbra app is constructed: settings, `App::builder`, the lifecycle phases, and the ambient `OnceLock<DbPool>` (including the test-override path).
2. **`02-plugin-contract.md`.** The `Plugin` trait. Read this early. The rest of the framework is plugins — built-in and third-party — and several later specs reference the trait's methods.
3. **`03-orm-querysets.md`.** `QuerySet<T>` and the `Manager`. The user-facing query API: filter, order, annotate, transactions, the raw-SQL escape hatch.
4. **`04-orm-model-and-fields.md`.** The `Model` trait, field types, options, relationships, and what `#[derive(Model)]` expands to.
5. **`05-backends-and-system-check.md`.** The `DatabaseBackend` abstraction and the boot-time field/backend compatibility check.
6. **`06-migration-engine.md`.** Model snapshot, autodetection, tracking table, the declare → migrate → change → migrate loop end-to-end. The north star.
7. **`07-inspectdb.md`.** Introspect an existing database into models that feed straight back into the migration engine. The porting payoff.

Outlines under `outlines/*.md` cover M7–M13 surfaces at half-page depth. Each one is promoted to a deep spec when its milestone is approached. The full coverage audit (every Django capability mapped to a spec or outline) lives in `docs/superpowers/specs/2026-05-30-spec-set-design.md §7`.

## Naming conventions

| Concept | Convention |
|---|---|
| Workspace facade crate | `umbra`. The only stable surface user code imports. |
| Internal crates | `umbra-core`, `umbra-macros`, `umbra-cli`. Refactorable; users never depend on them directly. |
| Built-in plugins | `umbra-auth`, `umbra-sessions`, `umbra-admin`, `umbra-tasks`, `umbra-rest`, `umbra-openapi`. |
| Third-party plugins | `umbra-<thing>`. Mirrors Django's `django-<thing>`. |
| Prelude | `use umbra::prelude::*`. |
| Modules ambient state lives in | One `OnceLock` per concern, owned by the relevant module: `umbra::db` (pool), `umbra::settings` (the `Settings` struct), `umbra::plugins` (the registry), `umbra::tasks` (the task queue handle), etc. Concrete placement is the subject of `01-app-and-settings.md`. |
| File names in `docs/specs/` | `<NN>-<kebab-case>.md`, two-digit prefix. |
| File names in `docs/specs/outlines/` | `<kebab-case>.md`, no prefix (order is determined by the milestone they map to, not a numeric sort). |

## Django ↔ umbra glossary

The translations are deliberate, not literal. The right-hand column captures what umbra calls the concept and points at the file that owns its design. Where the spec doesn't exist yet, the column names the future home.

| Django term | umbra term | Owned by |
|---|---|---|
| Project | App + binary crate | `arch.md §1`, `01-app-and-settings.md` |
| App | Plugin | `02-plugin-contract.md` |
| `INSTALLED_APPS` | `App::builder().plugin(...)` (explicit) plus optional `inventory` auto-registration | `02-plugin-contract.md` |
| `Model` (class) | `#[derive(Model)] struct` | `04-orm-model-and-fields.md` |
| Field type (`CharField`, `IntegerField`, ...) | Rust type plus attribute (`String`, `i64`, `Option<DateTime<Utc>>`, ...) | `04-orm-model-and-fields.md` |
| Manager (`Foo.objects`) | `T::objects()` (free function on the type) | `03-orm-querysets.md` |
| `QuerySet` | `QuerySet<T>` | `03-orm-querysets.md` |
| `F()` / `Q()` expressions | Macro form to be designed in spec 03 | `03-orm-querysets.md` |
| `select_related` / `prefetch_related` | Method form to be designed in spec 03 | `03-orm-querysets.md` |
| `makemigrations` / `migrate` | `cargo run -p umbra-cli -- makemigrations` / `migrate` | `06-migration-engine.md` |
| `inspectdb` | `cargo run -p umbra-cli -- inspectdb` | `07-inspectdb.md` |
| `AppConfig.ready()` | `Plugin::on_ready(&self, &AppContext)` | `02-plugin-contract.md` |
| `settings.py` | `Settings` struct, layered via figment | `01-app-and-settings.md` |
| `manage.py <cmd>` | `cargo run -p umbra-cli -- <cmd>` (binary) plus per-plugin `Command`s | `02-plugin-contract.md` |
| `request` | `Request` (`umbra::web`) plus extractors as handler arguments | outline `web-layer.md` |
| `auth.User` | `User`, with custom-user-model swap path designed in the auth outline | outline `auth-and-sessions.md` |
| `ModelForm` | Designed in outline `forms.md` | outline `forms.md` |
| `ModelSerializer` | Designed in outline `rest.md` | outline `rest.md` |
| `pre_save` / `post_save` signals | Designed in outline `signals.md` | outline `signals.md` |
| `default_storage` (file storage) | Designed in outline `static-and-media.md` | outline `static-and-media.md` |
| `cache_page` (per-view caching) | Designed in outline `caching.md` | outline `caching.md` |
| Django admin | `umbra-admin` (auto CRUD UI) | outline `admin.md` |
| DRF | `umbra-rest` (optional plugin) | outline `rest.md` |

When in doubt, the umbra term is whatever reads naturally to a Rust developer; the Django term is the bridge that helps porters orient. Both columns matter.

## Canonical example app

Every spec uses the same toy app for examples, so the running code doesn't reinvent itself per file. The app is a minimal blog: users write posts, posts can be tagged.

```text
example app: a small blog
├── User      (id, username, email, password_hash, joined_at)    ← built-in umbra-auth
├── Author    (id, user_id → User, display_name, bio)            ← FK to User
├── Post      (id, author_id → Author, title, slug, body,
│              published_at: Option<DateTime<Utc>>)
├── Tag       (id, name, slug)
└── PostTag   (post_id → Post, tag_id → Tag)                     ← M2M through table
```

A spec that needs an example reaches for these models first. A spec that needs something genuinely new invents an ad-hoc model, but the blog comes first — most ORM, migrations, and admin examples should be expressible against these five tables alone.

The example deliberately covers:

- A foreign-key relationship (`Author.user_id → User`).
- A one-to-one-ish relationship (one `Author` per `User`; enforced via `UNIQUE` on `Author.user_id`).
- A nullable column (`Post.published_at`).
- A many-to-many with an explicit through table (`Post ↔ Tag` via `PostTag`), so the through-row can carry its own fields later.
- A `name` + `slug` pattern that the field-options spec gets to demonstrate.
- A boundary with a built-in plugin (`User` lives in `umbra-auth`, but app code references it freely via the facade).

That's enough surface to exercise every concept the M0–M6 specs need without inventing five toy schemas.

## Open questions deferred to per-subsystem specs

Every open question lives inside the spec that owns its resolution. The authoritative list (with current status of each one) is in `docs/superpowers/specs/2026-05-30-spec-set-design.md §10`. Specs flag their own open questions in their final section so reading any one spec surfaces what's still unresolved in that area.

## Conventions a future spec author should follow

The specs in this directory share a skeleton, so reading any one feels predictable. New deep specs use:

1. A short metadata table at the top (Status, Maps to milestone, Companions).
2. **Purpose.** A paragraph naming the problem the spec solves.
3. **Concepts.** The named building blocks — types, traits, modules — explained in prose before any code.
4. **API-shape sketch.** Small illustrative Rust signatures, *not* a frozen API. Enough that a reader can picture the call site; never enough to lock the implementation.
5. **Mechanics and invariants.** The runtime behaviour, ordering rules, edge cases.
6. **Trade-offs.** What other shapes were considered, and why they lost.
7. **Open questions.** Anything left to resolve at the milestone.
8. **Cross-links.** Specs, outlines, and `arch.md` sections that constrain this one.

Keep the Rust code in §4 small. A spec that's mostly code is doing the implementation's job. If a snippet feels obvious from a sentence of prose, cut the snippet.
