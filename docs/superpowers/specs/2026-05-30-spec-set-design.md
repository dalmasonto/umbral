# Umbra Spec Set — Design

| | |
|---|---|
| **Date** | 2026-05-30 |
| **Status** | Approved (brainstorming) — implementation plan pending |
| **Authors** | Dalmas Ogembo + Claude |
| **Scope** | A meta-design: which specs to write before umbra implementation starts, at what depth, in what order. |

---

## 1. Context

Umbra is a greenfield, Django-equivalent web framework for Rust. The repository
contains three pre-existing documents:

- **`CLAUDE.md`** — the most recent thinking; treats the *declare → migrate →
  change → migrate* loop as a day-one (M5) target and treats *thin core + plugin-
  heavy* as the one idea that matters most.
- **`umbra-PRD.md`** — product requirements; downgrades autodetection to P1 and
  does not name the declare→migrate loop. **Drifts from CLAUDE.md.**
- **`django-shadow-rust-plan.md`** — architecture and build order; schedules M5
  as forward-only migrations and M8 as autodetection. **Drifts from CLAUDE.md.**

The goal of this design is to specify the doc set we will write *before*
implementation begins, so the framework's design is auditable on paper before
any code is written. The user's directive: "specs first; we shall implement them
as we go on."

---

## 2. Decisions

Four shaping choices, made during brainstorming:

1. **Scope.** Deep specs for the M0–M5 spine (and M6 `inspectdb`, because it is
   the porting payoff and inseparable from M5's migration engine). M7–M13 get
   half-page **outlines** that get promoted to deep specs when their milestone
   comes up.
2. **Granularity.** One spec per subsystem (plugin contract, ORM, migration
   engine, etc.) — not grouped mega-docs. Lets each spec be committed and
   reviewed in isolation.
3. **Depth.** **Design + API-shape sketches** — mechanics, invariants, trade-
   offs, *plus* illustrative Rust signatures for the key public surface. Enough
   to drive implementation; not a frozen API reference.
4. **Existing docs.** Rename `django-shadow-rust-plan.md` → `arch.md` (matches
   CLAUDE.md's reference). Update PRD in place to bump autodetection to P0 and
   name the declare→migrate loop.

---

## 3. Cross-cutting principle: visibility of underlying crates

A load-bearing design question surfaced during brainstorming: **does an umbra
developer see axum?** The answer codifies the framework's *feel*.

Rule of thumb: **if a crate is a way to build the framework, hide it; if it is
how the user describes their own data and behavior, surface it.**

| Crate | Visibility | Notes |
|---|---|---|
| **axum** | **Hidden** by default. `umbra::web::{Router, Request, Response, Json, Path, Query, Form}`. Escape hatch: `umbra::axum::*`. | Day-to-day umbra looks Django-shape. |
| **sqlx** | **Hidden** behind `QuerySet` / `Manager`. Escape hatch: `umbra::db::query!` is `sqlx::query!`. | Compile-time-checked SQL remains available. |
| **sea-query** | **Fully hidden.** | Pure implementation detail. |
| **tower / tower-http** | **Mixed.** Middleware is configured through umbra's chain, but the underlying type is a tower service so standard layers compose. | Contract reads as umbra; ecosystem still works. |
| **serde** | **Visible.** Users `#[derive(Serialize, Deserialize)]` on their own types. | Ecosystem fluency, not infrastructure. |
| **clap** | **Visible at the extension seam.** Custom `Command`s use clap derives. | Same reason as serde. |
| **tracing** | **Visible.** Users add their own spans/logs. | Observability is the user's. |
| **figment / config** | **Hidden** behind `Settings`. | Users see typed structs, not a config library. |

This principle is **inherited by every subsystem spec** — they don't relitigate
it. It will be lifted into `arch.md` as the single source of truth.

---

## 4. Existing-doc updates (2 commits)

### Commit A — rename `django-shadow-rust-plan.md` → `arch.md`, sync

- §7 Build Order rewritten to match CLAUDE.md:
  - **M5** = full migration engine (model snapshot + basic autodetection +
    tracking table + `migrate`). Not forward-only.
  - **M6** = `inspectdb`.
  - **M7** = Plugin trait extraction (architectural keystone).
  - **M8** = hardening autodetection (rename detection, data migrations,
    cross-plugin FK ordering).
- §0 already names managed migrations as a north star; add the explicit
  *declare → migrate → change → migrate* phrasing.
- Insert a new section **between §1 (Architectural Pillars) and §2 (The
  Plugin Contract)** titled "Visibility of underlying crates" — adopt the
  table from §3 above. It belongs there because dependency direction is
  already established in §1, and §2 starts naming concrete public surface
  (the prelude), so the rule needs to be in scope before that point.

### Commit B — update `umbra-PRD.md` in place

- `F-MIG-3` (autodetection) **P1 → P0**, with rationale (matches CLAUDE.md
  "day one"; the declare→migrate loop is the product, not a later feature).
- §1 Summary and §6 Product Principles call out the declare→migrate→change→
  migrate loop by name.
- §10 Release Phasing rewritten so phase 0.1 *includes* M5 (the loop is alive
  at the 0.1/0.2 boundary). 0.2 ("Porting MVP") then becomes `inspectdb` (M6)
  + hardening — same goal, more accurate cut.
- Companion-doc reference updated: `django-shadow-rust-plan.md` → `arch.md`.

---

## 5. Deep specs (`docs/specs/`, 8 commits)

Each follows a common skeleton:

> **Purpose · Concepts · API-shape sketch (illustrative Rust) · Mechanics &
> invariants · Trade-offs and alternatives considered · Open questions ·
> Cross-links.**

Target length: 1–3 pages. Illustrative code, not a frozen reference.

| # | File | Covers | Maps to milestone |
|---|---|---|---|
| 00 | `00-overview.md` | Index, reading order, Django↔umbra glossary, naming conventions (`umbra-*`), the canonical example app the specs reference | — |
| 01 | `01-app-and-settings.md` | Typed settings (env layering via figment), `App::builder()`, lifecycle order (build → system check → on_ready → serve), the `OnceLock<DbPool>` decision | M0 |
| 02 | `02-plugin-contract.md` | The `Plugin` trait, dependency-inversion model, what a plugin contributes (models, routes, middleware, commands, settings schema, hooks), registration (explicit + optional `inventory`), the prelude surface | M7 build-order, **specced early** as architectural keystone — gates every built-in spec |
| 03 | `03-orm-querysets.md` | `QuerySet<T>` builder, lazy eval, `filter / exclude / order_by / limit / values`, Manager (`T::objects()`), ambient pool access, raw-SQL escape hatch | M1 |
| 04 | `04-orm-model-and-fields.md` | The `Model` trait by hand → `#[derive(Model)]` output shape, field types (text/int/float/bool/datetime/decimal/UUID/JSON), options (optional/default/unique/indexed), `Meta` (table name, ordering, indexes), the nullable→`Option<T>` invariant | M2–M3 |
| 05 | `05-backends-and-system-check.md` | `DatabaseBackend` trait (dialect, quoting, RETURNING, upsert), field→backend declaration (`ArrayField` → `[Postgres]`), the boot-time system check that fails loudly | M4 |
| 06 | `06-migration-engine.md` | **The north star.** Model snapshot format, autodetected ops (create/alter/drop table, add/alter/drop column), tracking table, `makemigrations` + `migrate` CLI, the declare→migrate→change→migrate loop end-to-end, plugin-aware ordering | M5 |
| 07 | `07-inspectdb.md` | Introspection (sea-schema), DB type → Rust field mapping, conflict resolution, output to a migrations directory that feeds straight into M5 | M6 — the porting payoff |

### Deliberately *not* deep at this stage

- **Routing / views / middleware.** M0 has one hand-written axum route on
  purpose; the `umbra::web` API is best designed once we know what handlers
  need to receive from the ORM and the Plugin contract. Locking it down before
  M3/M5 would freeze the wrong shape. Outline only.
- **CLI.** `manage.py`-equivalent gets a section inside `arch.md` for now;
  promoted when the command list grows past `migrate` / `makemigrations` /
  `inspectdb`.
- **Error model & security defaults.** Referenced cross-cuttingly inside
  `arch.md` and inside specs that touch them; promoted to their own specs once
  they accrete real surface area.

---

## 6. Outlines (`docs/specs/outlines/`, 6 commits)

Each outline is ~½ page: **Purpose · Key concepts · Open questions · Cross-
links to deep specs that constrain it · "Promote to deep spec when …"
trigger.**

| File | Covers | Promote-to-deep trigger |
|---|---|---|
| `web-layer.md` | `umbra::web` shape (Router, Request, Response, extractors), middleware chain, the "hide axum" rule applied | Promote when M0's second route lands, or when the Plugin contract spec needs to name `Router` concretely |
| `auth-and-sessions.md` | `umbra-auth` (User model, perms, argon2, login guards) + `umbra-sessions` (tower-sessions wrapper, DB session store) | M8 entry — re-expressing built-ins as plugins |
| `tasks.md` | `umbra-tasks`: `#[task]`, `Task` trait, DB-backed broker, worker loop, retries, scheduling | M10 entry |
| `rest.md` | `umbra-rest`: serializers / `ModelSerializer`, viewsets, routers, pagination, filtering, throttling | M11 entry |
| `admin.md` | `umbra-admin`: auto CRUD UI, list/filter/search, inlines, bulk actions, permission integration | M12 entry |
| `openapi.md` | `umbra-openapi`: utoipa integration, Swagger UI, schema gen from REST viewsets | M12 entry (after admin or in parallel) |

Outlines live in `docs/specs/outlines/` rather than as half-finished entries
inside `docs/specs/`, so the deep-spec directory stays a clean "source of
truth" list and deferred work stays obviously deferred.

---

## 7. Commit cadence

One commit per file. Message form:

```
docs(arch):     changes to arch.md
docs(prd):      changes to umbra-PRD.md
docs(specs):    new file or change in docs/specs/
docs(outline):  new file or change in docs/specs/outlines/
```

Each commit stands on its own and is reviewable independently.

---

## 8. Total commit budget

```
2  doc updates       (rename plan→arch.md, update PRD)
8  deep specs        (00 overview + 01–07)
6  outlines          (web-layer, auth-and-sessions, tasks, rest, admin, openapi)
──
16 commits before any Rust code is written.
```

---

## 9. Open questions captured for later

Carried forward from PRD §13 and from this brainstorming, to be resolved in the
specs that touch them:

1. **Ambient ORM access** — `OnceLock<DbPool>` vs always-explicit `State`
   threading. *Decided in `01-app-and-settings.md`.*
2. **Plugin registration default** — explicit builder vs `inventory`/`linkme`
   auto-registration as the recommended path. *Decided in `02-plugin-
   contract.md`.*
3. **Admin UI rendering** — server-rendered templates vs a small embedded SPA.
   *Decided in `admin.md` (outline → deep spec at M12).*
4. **Async story for signals/hooks** — sync vs async callbacks; ordering
   guarantees. *Decided in `02-plugin-contract.md` or a follow-up signals
   spec.*
5. **Custom user model mechanism** — how to allow override without Django's
   runtime swapping tricks. *Decided in `auth-and-sessions.md` (outline → deep
   at M8).*
6. **`umbra::web` API shape** — concrete types for `Router`, `Request`,
   `Response`, extractors. *Deliberately deferred to the web-layer deep spec
   (post-ORM).*

---

## 10. Out of scope for this design

- The actual content of the specs themselves — written one by one in the
  implementation plan that follows.
- Any Rust code. No `Cargo.toml`, no `src/`, until the spec set is complete
  and approved.
- Tooling choices (CI, formatter config, MSRV policy) — captured in arch.md or
  later commits as they become needed.

---

## 11. Next step

Hand off to the **writing-plans** skill: turn this design into an ordered,
committable plan of 16 writing tasks with clear inputs, outputs, and review
checkpoints.
