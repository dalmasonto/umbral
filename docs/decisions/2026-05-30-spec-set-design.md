# Umbral Spec Set — Design

| | |
|---|---|
| **Date** | 2026-05-30 |
| **Status** | Approved (brainstorming) — implementation plan pending |
| **Authors** | Dalmas Ogembo + Claude |
| **Scope** | A meta-design: which specs to write before umbral implementation starts, at what depth, in what order. |

---

## 1. Context

Umbral is a greenfield, Django-equivalent web framework for Rust. The repository contains three pre-existing documents:

- **`CLAUDE.md`** — the most recent thinking; treats the *declare → migrate → change → migrate* loop as a day-one (M5) target and treats *thin core + plugin-heavy* as the one idea that matters most.
- **`umbral-PRD.md`** — product requirements; downgrades autodetection to P1 and does not name the declare→migrate loop. **Drifts from CLAUDE.md.**
- **`django-shadow-rust-plan.md`** — architecture and build order; schedules M5 as forward-only migrations and M8 as autodetection. **Drifts from CLAUDE.md.**

The goal of this design is to specify the doc set we will write *before* implementation begins, so the framework's design is auditable on paper before any code is written. The user's directive: "specs first; we shall implement them as we go on."

---

## 2. Decisions

Four shaping choices, made during brainstorming:

1. **Scope.** Deep specs for the M0–M5 spine (and M6 `inspectdb`, because it is the porting payoff and inseparable from M5's migration engine). M7–M13 get half-page **outlines** that get promoted to deep specs when their milestone comes up.
2. **Granularity.** One spec per subsystem (plugin contract, ORM, migration engine, etc.) — not grouped mega-docs. Lets each spec be committed and reviewed in isolation.
3. **Depth.** **Design + API-shape sketches** — mechanics, invariants, trade-offs, *plus* illustrative Rust signatures for the key public surface. Enough to drive implementation; not a frozen API reference.
4. **Existing docs.** Rename `django-shadow-rust-plan.md` → `arch.md` (matches CLAUDE.md's reference). Update PRD in place to bump autodetection to P0 and name the declare→migrate loop.

---

## 3. Cross-cutting principles

Two load-bearing design questions surfaced during brainstorming. Their answers codify the framework's *feel* and are inherited by every subsystem spec — those specs don't relitigate them. Both rules will be lifted into `arch.md` as the single source of truth.

### 3.1 Visibility of underlying crates

**Does an umbral developer see axum?** Rule of thumb: **if a crate is a way to build the framework, hide it; if it is how the user describes their own data and behavior, surface it.**

| Crate | Visibility | Notes |
|---|---|---|
| **axum** | **Hidden** by default. `umbral::web::{Router, Request, Response, Json, Path, Query, Form}`. Escape hatch: `umbral::axum::*`. | Day-to-day umbral looks Django-shape. |
| **sqlx** | **Hidden** behind `QuerySet` / `Manager`. Escape hatch: `umbral::db::query!` is `sqlx::query!`. | Compile-time-checked SQL remains available. |
| **sea-query** | **Fully hidden.** | Pure implementation detail. |
| **tower / tower-http** | **Mixed.** Middleware is configured through umbral's chain, but the underlying type is a tower service so standard layers compose. | Contract reads as umbral; ecosystem still works. |
| **serde** | **Visible.** Users `#[derive(Serialize, Deserialize)]` on their own types. | Ecosystem fluency, not infrastructure. |
| **clap** | **Visible at the extension seam.** Custom `Command`s use clap derives. | Same reason as serde. |
| **tracing** | **Visible.** Users add their own spans/logs. | Observability is the user's. |
| **figment / config** | **Hidden** behind `Settings`. | Users see typed structs, not a config library. |

### 3.2 Handler-visible context: ambient vs explicit

**Does a handler signature carry `State<DbPool>`?** No — and the same rule extends to every other kind of context. The first table answers *what types* show up; this one answers *what context* shows up.

| Kind of context | Examples | Visibility in a handler |
|---|---|---|
| **App-wide / process-scoped** | DB pool, `Settings`, plugin registry, task-queue handle, cache, template engine | **Ambient.** Set during `App::build()` (stored in `OnceLock`s inside the relevant module). Reached via accessors: `Post::objects()`, `umbral::settings()`, `umbral::tasks::enqueue(...)`. **No `State<…>` in the handler signature.** |
| **Per-request / request-scoped** | The Request, parsed body, path/query params, the session, the authenticated user, an active transaction handle | **Explicit arguments.** Extracted into the handler signature: `Request`, `Path<T>`, `Json<T>`, `Form<T>`, `Query<T>`, `Session`, `Auth<User>`. Uses axum extractors *under the hood*; the user sees umbral types only. |

A Django-shape umbral handler — no `State`, no `axum`, ambient ORM:

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

**Edge cases the rule has to survive** (recorded so the specs that own them don't forget):

- **Tests.** `OnceLock` is write-once per process. The override path lives in `01-app-and-settings.md`: a `Manager::on(&pool)` explicit-pool escape hatch, plus a `test_with_pool(pool, async { ... })` helper that scopes the override for a test future. Open question #1 closes here.
- **Multi-database routing** (PRD F-ORM-8). Default pool is ambient; explicit alias via `Post::objects().using("replica")` keeps the rule.
- **Per-request transactions.** `Db::tx(|tx| async { ... })` passes `tx` into the closure as a request-scoped argument without leaking it through the handler signature.

---

## 4. Existing-doc updates (2 commits)

### Commit A — rename `django-shadow-rust-plan.md` → `arch.md`, sync

- §7 Build Order rewritten to match CLAUDE.md:
  - **M5** = full migration engine (model snapshot + basic autodetection + tracking table + `migrate`). Not forward-only.
  - **M6** = `inspectdb`.
  - **M7** = Plugin trait extraction (architectural keystone).
  - **M8** = hardening autodetection (rename detection, data migrations, cross-plugin FK ordering).
- §0 already names managed migrations as a north star; add the explicit *declare → migrate → change → migrate* phrasing.
- Insert a new section **between §1 (Architectural Pillars) and §2 (The Plugin Contract)** titled "Visibility of underlying crates" — adopt the table from §3 above. It belongs there because dependency direction is already established in §1, and §2 starts naming concrete public surface (the prelude), so the rule needs to be in scope before that point.

### Commit B — update `umbral-PRD.md` in place

- `F-MIG-3` (autodetection) **P1 → P0**, with rationale (matches CLAUDE.md "day one"; the declare→migrate loop is the product, not a later feature).
- §1 Summary and §6 Product Principles call out the declare→migrate→change→migrate loop by name.
- §10 Release Phasing rewritten so phase 0.1 *includes* M5 (the loop is alive at the 0.1/0.2 boundary). 0.2 ("Porting MVP") then becomes `inspectdb` (M6) + hardening — same goal, more accurate cut.
- Companion-doc reference updated: `django-shadow-rust-plan.md` → `arch.md`.

---

## 5. Deep specs (`docs/specs/`, 8 commits)

Each follows a common skeleton:

> **Purpose · Concepts · API-shape sketch (illustrative Rust) · Mechanics & invariants · Trade-offs and alternatives considered · Open questions · Cross-links.**

Target length: 1–3 pages. Illustrative code, not a frozen reference.

| # | File | Covers | Maps to milestone |
|---|---|---|---|
| 00 | `00-overview.md` | Index, reading order, Django↔umbral glossary, naming conventions (`umbral-*`), the canonical example app the specs reference | — |
| 01 | `01-app-and-settings.md` | Typed settings (env layering via figment), `App::builder()`, lifecycle order (build → system check → on_ready → serve), the `OnceLock<DbPool>` decision **including the test-override path (`Manager::on(&pool)` + `test_with_pool` scoped helper) and which modules own which `OnceLock`s** | M0 |
| 02 | `02-plugin-contract.md` | The `Plugin` trait, dependency-inversion model, what a plugin contributes (models, routes, middleware, commands, settings schema, hooks), registration (explicit + optional `inventory`), the prelude surface | M7 build-order, **specced early** as architectural keystone — gates every built-in spec |
| 03 | `03-orm-querysets.md` | `QuerySet<T>` builder, lazy eval, `filter / exclude / order_by / limit / values`, Manager (`T::objects()`), ambient pool access, raw-SQL escape hatch | M1 |
| 04 | `04-orm-model-and-fields.md` | The `Model` trait by hand → `#[derive(Model)]` output shape, field types (text/int/float/bool/datetime/decimal/UUID/JSON), options (optional/default/unique/indexed), `Meta` (table name, ordering, indexes), the nullable→`Option<T>` invariant | M2–M3 |
| 05 | `05-backends-and-system-check.md` | `DatabaseBackend` trait (dialect, quoting, RETURNING, upsert), field→backend declaration (`ArrayField` → `[Postgres]`), the boot-time system check that fails loudly | M4 |
| 06 | `06-migration-engine.md` | **The north star.** Model snapshot format, autodetected ops (create/alter/drop table, add/alter/drop column), tracking table, `makemigrations` + `migrate` CLI, the declare→migrate→change→migrate loop end-to-end, plugin-aware ordering | M5 |
| 07 | `07-inspectdb.md` | Introspection (sea-schema), DB type → Rust field mapping, conflict resolution, output to a migrations directory that feeds straight into M5 | M6 — the porting payoff |

### Deliberately *not* deep at this stage

- **Routing / views / middleware.** M0 has one hand-written axum route on purpose; the `umbral::web` API is best designed once we know what handlers need to receive from the ORM and the Plugin contract. Locking it down before M3/M5 would freeze the wrong shape. Outline only.
- **CLI.** `manage.py`-equivalent gets a section inside `arch.md` for now; promoted when the command list grows past `migrate` / `makemigrations` / `inspectdb`.
- **Error model.** Referenced cross-cuttingly inside `arch.md` and inside specs that touch it; promoted to its own spec once it accretes real surface area. (Security defaults *do* get their own outline below — see §6 — because Django ships a noticeable security middleware surface that we can't fold into one paragraph.)

---

## 6. Outlines (`docs/specs/outlines/`, 15 commits)

Each outline is ~½ page: **Purpose · Key concepts · Open questions · Cross-links to deep specs that constrain it · "Promote to deep spec when …" trigger.**

The first six outlines were named in the initial brainstorming. The remaining nine were added during the **§7 Django coverage audit** below, which walked through Django's full surface and surfaced real gaps.

| File | Covers | Promote-to-deep trigger |
|---|---|---|
| `web-layer.md` | `umbral::web` shape (Router, Request, Response, extractors `Auth<User>` / `Session` / `Path<T>` / `Json<T>` / `Form<T>` / `Query<T>`), middleware chain, generic views, multipart / file uploads, streaming responses, cookies, the "hide axum" rule applied, **the invariant that handler signatures never carry `State<X>` for any app-wide X** | Promote when M0's second route lands, or when the Plugin contract spec needs to name `Router` concretely |
| `auth-and-sessions.md` | `umbral-auth` (User model + custom user override, permissions, groups, authentication backends, login/logout, argon2 hashing, password validators, password reset) + `umbral-sessions` (tower-sessions wrapper, DB session store) | M8 entry — re-expressing built-ins as plugins |
| `tasks.md` | `umbral-tasks`: `#[task]`, `Task` trait, DB-backed broker, worker loop, retries, periodic scheduling ("beat"), the `worker` and `beat` CLI commands | M10 entry |
| `rest.md` | `umbral-rest`: serializers / `ModelSerializer`, viewsets, routers, pagination, filtering, throttling, content negotiation, renderers, parsers | M11 entry |
| `admin.md` | `umbral-admin`: auto CRUD UI, list/filter/search, inlines, bulk actions, fieldsets, permission integration | M12 entry |
| `openapi.md` | `umbral-openapi`: utoipa integration, Swagger UI, schema generation from REST viewsets and serializers | M12 entry (after admin or in parallel) |
| `signals.md` | Decoupled events (pre_save / post_save / pre_delete / post_delete + custom signals), sync vs async dispatch, ordering guarantees, signal connection lifecycle | Open question #4 resolves here; promote at M8 (re-expressing built-ins) or earlier if a built-in plugin needs them |
| `templates.md` | Template engine choice (minijinja vs askama), autoescape semantics, inheritance, custom tags/filters, the rendering substrate the admin and email plugins reuse | Promote at M12 (admin entry) or earlier if any feature needs server-rendered HTML |
| `forms.md` | Declarative server-rendered forms (parallel to but distinct from REST serializers), `ModelForm` equivalent, fields, widgets, validation, formsets / inline formsets | Promote at M9–M12 when the admin or non-API web flows need them |
| `static-and-media.md` | `collectstatic` equivalent, dev-time static serving, `FileField` / `ImageField` semantics, storage backends (filesystem default; S3-compatible later), multipart upload handling | Promote alongside `web-layer.md` when file uploads or admin file fields land |
| `caching.md` | Cache backends (moka in-process, redis), per-view / per-fragment / low-level cache APIs, invalidation patterns | Promote when a built-in plugin (sessions, admin, rest) needs caching beyond defaults |
| `email.md` | `send_mail` equivalent, `EmailMessage`, backends (SMTP, console, file, dummy), templated emails (cross-links `templates.md`) | Promote when password reset, or any built-in, needs to send mail |
| `testing.md` | Test client (`umbral::test::Client` wrapping `axum-test`), fixtures, factories, transactional `TestCase` analog, integration vs unit boundaries | Promote at M9 — the moment the built-ins need integration tests |
| `security-defaults.md` | CSRF, clickjacking (X-Frame-Options), HSTS, Content-Type-Nosniff, Referrer-Policy, COOP / CSP, secret signing (`SECRET_KEY` equivalent), secure-cookie defaults | Promote at M9 (with auth/sessions) — security middleware ships with the first plugin set |
| `dev-experience.md` | `startproject` / `startapp` generators, dev server + autoreload (cargo-watch / listenfd), rich error pages, debug-mode tracebacks | Promote at M13 (polish) |

Outlines live in `docs/specs/outlines/` rather than as half-finished entries inside `docs/specs/`, so the deep-spec directory stays a clean "source of truth" list and deferred work stays obviously deferred.

---

## 7. Django coverage audit

A walk through Django's feature surface, mapped onto the umbral doc set. The goal is to confirm nothing important slipped past, and to surface exactly where each Django capability lands. Three states matter: **covered by a deep spec**, **covered by an outline** (deferred to its milestone), or **explicitly out of scope** (with reason).

This table is the source of truth for "where does feature X go?" When a deep spec gets written, it uses this table to know what to scope in. When a new feature need surfaces during implementation, it gets added to the right cell here first. The §5 deep-spec scope columns are deliberately terse so they don't lock in API; this audit is where the full enumeration lives.

### 7.1 ORM and data

| Django capability | Location in umbral doc set | Notes |
|---|---|---|
| Models, fields, options, `Meta` | deep `04-orm-model-and-fields.md` | |
| Field types (text, int, float, bool, datetime, decimal, UUID, JSON, binary) | deep `04` | base set, day-one |
| `FileField`, `ImageField`, `EmailField`, `URLField`, `SlugField` | deep `04` (field types) + outline `static-and-media.md` (file storage semantics) | |
| Relationships (`ForeignKey`, `OneToOne`, `ManyToMany` with through tables) | deep `04` | scoped in when written; PRD F-ORM-4 (P1) |
| Validators (model-level) | deep `04` | model field validators |
| Validators (form-level) | outline `forms.md` | |
| Manager (`objects`), custom managers | deep `03-orm-querysets.md` | |
| QuerySet (`filter / exclude / get / all / order_by / limit / values`) | deep `03` | |
| F() / Q() expressions | deep `03` | PRD F-ORM-5 (P1) |
| Aggregates (Count, Sum, Avg, Min, Max) | deep `03` | |
| Annotations | deep `03` | |
| `select_related`, `prefetch_related` (N+1 fix) | deep `03` | PRD F-ORM-6 (P1) |
| Subqueries, `OuterRef` | deep `03` | |
| Lifecycle hooks (`save` / `delete` overrides) | deep `04` + outline `signals.md` | PRD F-ORM-7 (P2) |
| Signals (`pre_save`, `post_save`, custom, decoupled events) | outline `signals.md` | open question #4 closes here |
| Transactions (atomic blocks, savepoints) | deep `03` | PRD F-ORM-8 (P1) |
| Multi-database routing (`.using("alias")`) | deep `01-app-and-settings.md` (pool ownership) + deep `03` (call site) | direction set in §3.2 |
| Raw SQL escape hatch | deep `03` | `umbral::db::query!` is `sqlx::query!` |

### 7.2 Migrations and porting

| Django capability | Location | Notes |
|---|---|---|
| Migration engine, `makemigrations`, `migrate` | deep `06-migration-engine.md` | the north star |
| Autodetection (basic ops: create/alter/drop table, add/alter/drop column) | deep `06` | day-one |
| Autodetection (renames, data-preserving alters, complex constraints) | deep `06` carries the open question; hardening at M8 | iterated, not gated |
| Reversibility (forwards / backwards) | deep `06` | |
| Cross-plugin migration dependency graph | deep `06` + deep `02-plugin-contract.md` | |
| Data migrations (`RunPython` analog) | deep `06` (open Q for M8) | |
| `inspectdb` | deep `07-inspectdb.md` | the porting payoff |
| Squashing, fake migrations | deferred (PRD F-MIG-6 P2) | |

### 7.3 Web layer (HTTP)

| Django capability | Location | Notes |
|---|---|---|
| URL routing (path, includes, namespaces, `reverse`) | outline `web-layer.md` | |
| Function views | outline `web-layer.md` | |
| Generic class-based views | outline `web-layer.md` | trait-based, not inheritance |
| `Request` / `Response` types | outline `web-layer.md` | `umbral::web` |
| Middleware (tower-based) | outline `web-layer.md` | |
| Cookies | outline `web-layer.md` + outline `auth-and-sessions.md` | secure-cookie defaults in `security-defaults.md` |
| File uploads (multipart parsing) | outline `web-layer.md` (parsing) + outline `static-and-media.md` (storage) | |
| Streaming responses | outline `web-layer.md` | |
| Rich error pages / debug tracebacks | outline `dev-experience.md` | |
| Templates (autoescape, inheritance, custom tags/filters) | outline `templates.md` | minijinja or askama |
| Forms (declarative forms, `ModelForm`, validation, widgets, formsets) | outline `forms.md` | server-rendered HTML; separate from REST serializers |
| Static files (collectstatic, dev serving, storages) | outline `static-and-media.md` | |
| Media files (FileField, ImageField, storages) | outline `static-and-media.md` | |
| Content negotiation, renderers, parsers | outline `rest.md` | API-side |

### 7.4 Identity, sessions, permissions

| Django capability | Location | Notes |
|---|---|---|
| User model + custom user model | outline `auth-and-sessions.md` | open question #5 |
| Permissions (per-model, custom) | outline `auth-and-sessions.md` | |
| Groups | outline `auth-and-sessions.md` | |
| Authentication backends | outline `auth-and-sessions.md` | |
| `login` / `logout` / `login_required` | outline `auth-and-sessions.md` | |
| Password hashing (argon2) | outline `auth-and-sessions.md` | |
| Password validators | outline `auth-and-sessions.md` + outline `security-defaults.md` | |
| Password reset | outline `auth-and-sessions.md` + outline `email.md` | needs email |
| Sessions (cookie, DB) | outline `auth-and-sessions.md` | tower-sessions wrapper |

### 7.5 Background work and email

| Django capability | Location | Notes |
|---|---|---|
| Task queue (`#[task]`, broker, worker, retries) | outline `tasks.md` | DB-backed default |
| Periodic scheduling (Celery `beat` analog) | outline `tasks.md` | |
| Email (`send_mail`, `EmailMessage`) | outline `email.md` | |
| Email backends (SMTP, console, file, dummy) | outline `email.md` | |
| Email templates | outline `email.md` + outline `templates.md` | |

### 7.6 API surface

| Django capability | Location | Notes |
|---|---|---|
| Serializers, `ModelSerializer` | outline `rest.md` | |
| ViewSets, routers | outline `rest.md` | |
| Authentication / permission / throttle classes | outline `rest.md` | |
| Pagination, filtering, ordering | outline `rest.md` | |
| OpenAPI schema + Swagger UI | outline `openapi.md` | utoipa-based |
| Browsable API explorer | deferred (PRD §14) | revisit |

### 7.7 Admin

| Django capability | Location | Notes |
|---|---|---|
| Auto CRUD UI, list / filter / search | outline `admin.md` | |
| Inlines, bulk actions, fieldsets, `readonly_fields` | outline `admin.md` | |
| Permission integration | outline `admin.md` | |
| Admin UI rendering technology choice | outline `admin.md` (open Q #3) | server templates vs embedded SPA |

### 7.8 Operations and DX

| Django capability | Location | Notes |
|---|---|---|
| Typed, env-layered settings | deep `01-app-and-settings.md` | figment |
| App builder / lifecycle | deep `01` | build → system check → on_ready → serve |
| Boot-time system check (field/backend compatibility) | deep `05-backends-and-system-check.md` | |
| Management commands (manage.py extensions, per-plugin) | deep `02-plugin-contract.md` | |
| Management commands (binary surface: `migrate`, `makemigrations`, `inspectdb`, `worker`, `beat`, `shell`, `createsuperuser`, `runserver`, `collectstatic`, `dumpdata`, `loaddata`, `test`) | `arch.md` CLI section; promoted to its own spec when the list grows | |
| Caching framework (per-view, per-fragment, low-level; backends: moka, redis) | outline `caching.md` | |
| Logging | tracing visibility in §3.1 + cross-cutting in `arch.md` | promote to outline only if a custom design surfaces |
| Test client / fixtures / factories / transactional tests | outline `testing.md` | |
| Project / app scaffolding (`startproject`, `startapp`) | outline `dev-experience.md` | |
| Dev server + autoreload | outline `dev-experience.md` | |
| Rich error pages | outline `dev-experience.md` | |

### 7.9 Security

| Django capability | Location | Notes |
|---|---|---|
| CSRF middleware | outline `security-defaults.md` | |
| Clickjacking (X-Frame-Options) | outline `security-defaults.md` | |
| HSTS, secure cookies | outline `security-defaults.md` | |
| Content-Type-Nosniff, Referrer-Policy, COOP, CSP | outline `security-defaults.md` | |
| Template autoescape | outline `templates.md` | |
| Parameterized SQL | deep `03` / `04` | sqlx forces it; free guarantee |
| Signing (signed cookies, `SECRET_KEY` analog) | outline `security-defaults.md` | |
| Password hashing + validators | outline `auth-and-sessions.md` | argon2 |

### 7.10 Explicitly out of scope

The features below exist in Django but aren't shipping in umbral's first iteration. Each one is captured as a structured backlog entry (Django term, purpose, why deferred, complexity hint, suggested umbral shape, revisit signal) in **`docs/specs/deferred.md`** — the source of truth for "what's next after the M0–M13 spine."

Short summary, grouped:

- **Django `contrib` niceties.** `umbral-contenttypes`, `umbral-messages`, `umbral-sites`, `umbral-humanize`, `umbral-redirects`, `umbral-sitemaps`, `umbral-syndication`, `umbral-flatpages`.
- **Specialty domains.** `umbral-gis` (GeoDjango), `umbral-i18n`, `umbral-channels` (websockets).
- **Backend and infrastructure.** MySQL / Oracle backend support, pluggable non-DB task brokers (Redis, AMQP).
- **Tooling and UX.** DRF browsable API, hosted deploy / runtime tooling.
- **Cross-cutting umbral surfaces that haven't earned their own spec yet.** Error model, logging.

Use `deferred.md` to reorder and pick up items one at a time.

---

## 8. Commit cadence

One commit per file. Message form:

```
docs(arch):     changes to arch.md
docs(prd):      changes to umbral-PRD.md
docs(specs):    new file or change in docs/specs/
docs(outline):  new file or change in docs/specs/outlines/
```

Each commit stands on its own and is reviewable independently.

---

## 9. Total commit budget

```
2  doc updates       (rename plan→arch.md, update PRD)
8  deep specs        (00 overview + 01–07)
15 outlines          (web-layer, auth-and-sessions, tasks, rest, admin, openapi,
                      signals, templates, forms, static-and-media, caching,
                      email, testing, security-defaults, dev-experience)
──
25 commits before any Rust code is written.
```

---

## 10. Open questions captured for later

Carried forward from PRD §13 and from this brainstorming, to be resolved in the specs that touch them:

1. **Ambient ORM access** — `OnceLock<DbPool>` vs always-explicit `State` threading. *Direction decided here in §3.2 (ambient, with `Manager::on(&pool)` + `test_with_pool` escape hatches); concrete `OnceLock` placement, the test-override helper signature, and any `using("alias")` multi-DB hook designed in `01-app-and-settings.md`.*
2. **Plugin registration default** — explicit builder vs `inventory`/`linkme` auto-registration as the recommended path. *Decided in `02-plugin-contract.md`.*
3. **Admin UI rendering** — server-rendered templates vs a small embedded SPA. *Decided in `admin.md` (outline → deep spec at M12), constrained by `templates.md`.*
4. **Async story for signals/hooks** — sync vs async callbacks; ordering guarantees. *Decided in `signals.md`.*
5. **Custom user model mechanism** — how to allow override without Django's runtime swapping tricks. *Decided in `auth-and-sessions.md` (outline → deep at M8).*
6. **`umbral::web` API shape** — concrete types for `Router`, `Request`, `Response`, extractors. *Deliberately deferred to the web-layer deep spec (post-ORM).*

---

## 11. Out of scope for this design

- The actual content of the specs themselves — written one by one in the implementation plan that follows.
- Any Rust code. No `Cargo.toml`, no `src/`, until the spec set is complete and approved.
- Tooling choices (CI, formatter config, MSRV policy) — captured in arch.md or later commits as they become needed.

---

## 12. Next step

Hand off to the **writing-plans** skill: turn this design into an ordered, committable plan of 25 writing tasks with clear inputs, outputs, and review checkpoints.
