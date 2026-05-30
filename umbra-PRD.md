# Umbra — Product Requirements Document

| | |
|---|---|
| **Product** | Umbra — a batteries-included, Django-inspired web framework for Rust |
| **Status** | Draft v0.1 |
| **Date** | May 30, 2026 |
| **Owner** | *[you]* |
| **Companion doc** | `django-shadow-rust-plan.md` (architecture & build strategy) |

---

## 1. Summary

Umbra is a batteries-included web framework for Rust that recreates Django's developer
experience — declarative models, managed migrations, an auto-generated admin, an optional REST
layer, an out-of-the-box task queue — while delivering Rust's compile-time safety, predictable
latency, and fearless concurrency. Its defining bet is the **porting experience**: the fastest
path in the Rust ecosystem to take an existing database-backed API and stand it up in Rust,
rather than assembling Axum + sqlx + serde + a dozen other crates by hand.

The architecture is **thin-core + plugin-heavy**, and the framework **dogfoods its own plugin
system** — auth, admin, sessions, and the task queue are all plugins, structurally identical to
third-party ones.

---

## 2. Problem & Motivation

Rust offers safety and performance that make it attractive for production APIs, but web
development in Rust is *unassembled*. Unlike Django (Python) or Rails (Ruby), there is no
mainstream "just works" path: developers must individually choose and integrate a web framework,
a database layer, a migration tool, serialization, auth, and background jobs, then wire them
together correctly. This raises the cost of two things in particular:

1. **Greenfield productivity** — spinning up a conventional CRUD/REST API takes days of plumbing
   instead of minutes.
2. **Porting** — moving an existing API (from Django, Rails, Node, etc.) to Rust for performance
   or cost reasons means re-deriving schema, models, and conventions by hand. There is no
   equivalent of Django's `inspectdb` + managed migrations that makes "lift an existing database
   into a working backend" a near-mechanical task.

The result: teams that would benefit from Rust's resilience often stay on higher-level
frameworks because the on-ramp is too steep. Umbra targets that on-ramp.

---

## 3. Goals & Non-Goals

### Goals
- **G1 — Fast porting.** Point Umbra at an existing database and generate working models +
  migrations; bring up a CRUD API over them with minimal code.
- **G2 — Batteries included.** ORM, migrations, routing, auth, admin, sessions, caching, and a
  DB-backed task queue available without external assembly.
- **G3 — Resilience by default.** The easy path is the safe path: nullable columns are
  `Option<T>`, errors are `Result`, query mistakes and backend mismatches fail at build/boot,
  not in production.
- **G4 — Extensible by design.** A first-class plugin system where a plugin can own models,
  migrations, routes, commands, and settings — and "just works" after `cargo add` + register.
- **G5 — Familiar.** A Django/Rails developer should find concepts (models, migrations, apps,
  `manage.py`, admin) recognizable.

### Non-Goals
- **N1** — Not a frontend/WASM UI framework (no Leptos-style client). Server-side first.
- **N2** — Not attempting to beat Axum/Actix on raw benchmark throughput; Umbra *sits on* Axum
  and accepts a thin overhead in exchange for productivity.
- **N3** — Not a 1:1 Django API clone; idioms are translated to Rust, not transliterated.
- **N4** — Not multi-database-perfect at launch. **Postgres-first**; SQLite for tests; other
  backends gated behind a compatibility check.
- **N5** — Not a hosting/deploy product. Deployment is the user's choice.

---

## 4. Target Users & Personas

- **Persona A — The Migrator.** A team running a Django/Rails/Node API hitting performance or
  infrastructure-cost ceilings, wanting Rust without a ground-up rewrite. *Primary persona.*
- **Persona B — The Assembly-Fatigued Rustacean.** A Rust developer who likes the language but is
  tired of hand-wiring Axum + sqlx + serde + auth for every new service.
- **Persona C — The Django Refugee.** A developer who loves Django's ergonomics, is learning
  Rust, and wants the same shape of productivity.
- **Persona D — The Plugin Author.** A developer building a reusable extension (e.g. a payments
  integration, a CMS module) who needs the framework's ORM and lifecycle without fighting it.

---

## 5. User Stories (Jobs To Be Done)

- As a **Migrator**, I run one command against my existing Postgres database and get generated
  models, so I can start serving traffic from Rust without re-typing my schema.
- As a **Migrator**, I evolve a model and run `migrate`, and the framework generates and applies
  the schema change safely and reversibly.
- As an **Assembly-Fatigued Rustacean**, I define a struct, derive `Model`, and immediately get a
  typed query API and a REST endpoint, without choosing or wiring five crates.
- As a **Django Refugee**, I register a model with the admin and get a working CRUD UI to manage
  data during development.
- As any user, I enqueue a background job with a typed argument and a worker runs it reliably,
  with retries, without standing up Redis or RabbitMQ first.
- As a **Plugin Author**, I `cargo add` the framework facade, implement one trait, ship my crate,
  and a consumer registers it — their `migrate` provisions my tables automatically.

---

## 6. Product Principles

1. **Convention over configuration** — sensible defaults; configuration is the exception.
2. **The easy path is the safe path** — ergonomics and resilience are not traded off.
3. **The framework dogfoods its own extension points** — built-ins are plugins.
4. **Stand on shoulders** — reuse primitives (HTTP, async, SQL, JSON); build conventions & glue.
5. **Fail left** — turn runtime failures into compile-time or boot-time failures wherever possible.
6. **Postgres-first, honest about it** — expose backend-specific power with guardrails, not lowest-common-denominator abstraction that silently breaks.

---

## 7. Functional Requirements

Prioritized **P0** (MVP / must-have), **P1** (v1), **P2** (later). Phase mapping refers to
milestones M0–M13 in the companion plan.

### 7.1 Data Layer (ORM)
| ID | Requirement | Priority |
|----|-------------|----------|
| F-ORM-1 | Declarative models via `#[derive(Model)]` | P0 |
| F-ORM-2 | Core field types (text, int, float, bool, datetime, decimal, UUID, JSON) | P0 |
| F-ORM-3 | Typed QuerySet API (filter/exclude/order/limit/values, lazy) | P0 |
| F-ORM-4 | Relationships (FK, O2O, M2M with through tables) | P1 |
| F-ORM-5 | Expressions (F/Q), aggregates, annotations | P1 |
| F-ORM-6 | Relation loading (select_related, prefetch_related / N+1 fix) | P1 |
| F-ORM-7 | Lifecycle hooks & signals | P2 |
| F-ORM-8 | Transactions, multi-DB routing, raw-SQL escape hatch | P1 |

### 7.2 Migrations (Porting Superpower)
| ID | Requirement | Priority |
|----|-------------|----------|
| F-MIG-1 | `inspectdb` — introspect existing DB → generate models | **P0** |
| F-MIG-2 | Forward migrations: generate + apply + track | P0 |
| F-MIG-3 | Autodetection: diff model snapshot → ordered, reversible ops | P1 |
| F-MIG-4 | Cross-plugin dependency graph | P1 |
| F-MIG-5 | Data migrations | P1 |
| F-MIG-6 | Squashing & fake migrations | P2 |

### 7.3 Web Layer
| ID | Requirement | Priority |
|----|-------------|----------|
| F-WEB-1 | URL routing (patterns, includes, namespaces, reverse) | P0 |
| F-WEB-2 | Function views + generic views via traits/composition | P0 |
| F-WEB-3 | Middleware stack (tower-based) | P0 |
| F-WEB-4 | Sessions, cookies, file uploads | P1 |

### 7.4 Plugin System
| ID | Requirement | Priority |
|----|-------------|----------|
| F-PLG-1 | `Plugin` trait: models, migrations, routes, commands, settings, hooks | **P0** |
| F-PLG-2 | Explicit builder registration (INSTALLED_APPS equivalent) | P0 |
| F-PLG-3 | Plugin-owned migrations auto-run on `migrate` | **P0** |
| F-PLG-4 | Facade/prelude crate for stable author surface (`use umbra::prelude::*`) | P0 |
| F-PLG-5 | Optional auto-registration (inventory/linkme) | P2 |

### 7.5 Built-in Plugins
| ID | Requirement | Priority |
|----|-------------|----------|
| F-BLT-1 | Auth: user model (incl. custom), permissions, password hashing | P1 |
| F-BLT-2 | Sessions plugin | P1 |
| F-BLT-3 | Task queue: DB-backed, `#[task]`, worker, retries, scheduling | P1 |
| F-BLT-4 | REST (optional): serializers, viewsets, routers, pagination, filtering | P1 |
| F-BLT-5 | OpenAPI/Swagger UI generation (depends on REST) | P2 |
| F-BLT-6 | Admin: auto CRUD UI, list/filter/search, inlines, actions | P2 |

### 7.6 Tooling & DX
| ID | Requirement | Priority |
|----|-------------|----------|
| F-DX-1 | `manage.py`-equivalent CLI with extensible subcommands | P0 |
| F-DX-2 | Project/app scaffolding & generators | P1 |
| F-DX-3 | Typed, environment-aware settings | P0 |
| F-DX-4 | Boot-time system check (config + backend-field compatibility) | P0 |
| F-DX-5 | Dev server with autoreload | P1 |
| F-DX-6 | Fixtures, test client, rich error pages | P2 |

### 7.7 Database Backends
| ID | Requirement | Priority |
|----|-------------|----------|
| F-DB-1 | Postgres backend (primary) | P0 |
| F-DB-2 | SQLite backend (tests/dev) | P1 |
| F-DB-3 | Backend-specific fields (e.g. Postgres ArrayField) declaring supported backends | P1 |
| F-DB-4 | System check fails at boot on field/backend mismatch | P0 |

---

## 8. Non-Functional Requirements

- **Resilience:** Nullable columns map to `Option<T>`; all fallible operations return `Result`;
  compile-time-checked queries where feasible; backend mismatches caught at boot.
- **Performance:** Acceptable overhead over bare Axum (target: negligible for I/O-bound
  workloads); no GC pauses; worker pool exploits true parallelism.
- **Security (secure by default):** CSRF protection, template autoescaping, parameterized
  queries, clickjacking/HSTS headers, password hashing, secret signing — on by default.
- **Developer Experience:** Time-to-first-CRUD-endpoint in minutes; one stable import surface
  (the facade); clear, actionable error messages.
- **Compatibility:** Stable MSRV policy; semver discipline on the facade crate so plugins don't
  break on internal refactors.
- **Documentation:** First-class — a guide, API reference, and a "porting in 10 minutes" tutorial.

---

## 9. Success Metrics

- **Porting time:** existing Postgres DB → generated models + running CRUD endpoint in **< 15
  minutes** (vs. hours assembling by hand).
- **Boilerplate reduction:** lines of code for a basic CRUD resource vs. equivalent raw
  Axum + sqlx (target: a large, demonstrable reduction).
- **Time-to-first-endpoint:** new project to first working endpoint in **< 10 minutes**.
- **Ecosystem health (longer term):** number of third-party `umbra-*` plugins; the built-ins
  themselves passing the "could a stranger have written this as a plugin?" test.
- **Safety:** classes of bug (null handling, backend mismatch, SQL typos) demonstrably moved to
  build/boot time.

---

## 10. Release Phasing

| Phase | Theme | Milestones | Exit criteria |
|-------|-------|-----------|---------------|
| **0.1 — Core spine** | Prove the ORM concept | M0–M3 | Define a model, query it via derived API |
| **0.2 — Porting MVP** | The differentiator | M4–M6 | `inspectdb` + forward migrations against a real Postgres DB |
| **0.3 — Plugin contract** | Extensibility keystone | M7–M9 | Auth + sessions re-expressed as plugins; `migrate` walks all plugins |
| **0.4 — Productivity** | Batteries | M10–M12 | Task queue, optional REST, admin, OpenAPI all usable |
| **0.5 — Polish** | DX & ecosystem | M13 | Generators, autoreload, docs, first external plugin |

> Phasing is sequenced for *learning-first* development (the project's stated primary goal):
> each phase is independently demoable, and the plugin contract is extracted only after the
> primitives have been built once by hand.

---

## 11. Competitive Landscape

- **Django + DRF (Python):** the experience being shadowed. Wins on maturity & ecosystem; loses
  on runtime performance and compile-time safety. Umbra's reference point.
- **Loco (Rust):** Rails-style, batteries-included on SeaORM. Closest productivity peer; Umbra
  differentiates on Django-style migrations/`inspectdb` and porting focus.
- **Cot (Rust):** explicitly Django-like, builds its own ORM on sea-query atop Axum. Closest
  prior art and direct comparison; not production-ready as of this writing.
- **Axum / Actix (Rust):** the assembly-required baseline Umbra is built on and abstracts over.
- **Differentiator:** Umbra leads with **porting** (`inspectdb` + managed migrations) and a
  **dogfooded plugin system**, framed around moving existing APIs onto Rust's resilience.

---

## 12. Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|-----------|
| Migration autodetector complexity (Django spent years on edge cases) | High | Ship `inspectdb` + forward-only first; treat autodetection as iterative; constrain scope to Postgres |
| ORM "magic" fights Rust ownership/idioms | High | Lean into owned builder patterns; decide ambient-pool strategy deliberately (`OnceLock`) |
| Proc-macro complexity slows progress | Medium | Build the `Model` impl by hand first, then automate; isolate macro crate |
| Scope creep (Django is enormous) | High | Strict P0/P1/P2; defer all contrib-style extras |
| Solo/learning project bandwidth | Medium | Phasing optimized for demoable slices; output is secondary to the learning goal |
| Multi-DB expectations | Medium | Be explicit: Postgres-first, guardrails enforce it |

---

## 13. Open Questions

- Ambient ORM access: confirm `OnceLock<DbPool>` vs. always-explicit `State` threading.
- Registration default: explicit builder vs. inventory auto-registration as the recommended path.
- Admin UI rendering: server-rendered templates vs. a small embedded SPA.
- Async story for signals/hooks: sync callbacks vs. async, and ordering guarantees.
- Custom user model mechanism: how to allow override without Django's runtime swapping tricks.

---

## 14. Out of Scope (for now) / Future

GeoDjango-style geospatial; i18n/l10n; syndication feeds, sitemaps, flatpages; pluggable
non-DB task brokers (Redis/AMQP); MySQL/Oracle backends; a browsable API explorer; hosted
deployment tooling. Revisit after 0.5 based on real usage.

---

*Companion: see `django-shadow-rust-plan.md` for architecture, the plugin contract, the
dependency-inversion model that avoids circular crate deps, and the full M0–M13 build order.*