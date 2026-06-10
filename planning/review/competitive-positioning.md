# Competitive positioning — umbra in the Rust web landscape

Date: 2026-06-10. umbra facts are grounded in the security/feature/performance audit in this folder. Competitor facts were verified by web sweep on 2026-06-10 (sources at the bottom); they reflect that date and should be re-checked before any public use, as cot/loco move fast.

## The layer map — most "competitors" aren't on the same layer

| Layer | Solves | Members | Relationship to umbra |
|---|---|---|---|
| Async/HTTP plumbing | event loop, sockets, routing | tokio, hyper, **axum**, actix-web | umbra is *built on* axum+tokio. Substrate, not a rival (Django→WSGI). |
| Reactive UI / cross-platform | client reactivity, WASM, desktop/mobile | **Leptos, Dioxus** | Orthogonal; opposite frontend philosophy. umbra = server-rendered HTML-over-the-wire (MiniJinja + HTMX); they = reactive WASM SPAs. Could consume an umbra API. |
| **Batteries-included backend** | ORM, migrations, admin, auth, declare→app | **cot.rs, loco.rs, umbra** | The actual competitive set. |

The real comparison is **two frameworks: cot.rs (Django-shaped) and loco.rs (Rails-shaped).**

## Verified competitor state (2026-06-10)

**cot.rs** — the direct 1:1 rival.
- v0.6.0 (released 2026-03-19); announced Feb 2025 → steady release cadence. ~940 GitHub stars, 49 forks, multi-contributor, docs site, InfoWorld coverage. Real mindshare in umbra's exact slot.
- Surface: own ORM on axum (sea-query lineage), auto-migrations, admin panel, forms, templates, **OpenAPI**, email, static files, testing, auth, sessions.
- **Markets "security should be opt-out, not opt-in" as its headline.**
- Not found: a background task/job queue; a full DRF-style serializer/viewset REST framework (has OpenAPI gen, not the serializers+viewsets+playground stack).
- Still "not yet production-ready" by its own docs.

**loco.rs** — the Rails axis; most mature of the three.
- Built on **SeaORM** (2.0, "genuinely production-ready" Jan 2026) + axum. "The one-person framework."
- Has auth, Redis/thread-backed workers, mailers, a cron scheduler, scaffolding generators.
- **No admin panel advertised.** Biggest community, most real apps shipping.

## Feature comparison

| Capability | umbra | cot.rs | loco.rs |
|---|---|---|---|
| Philosophy | Django | Django (direct rival) | Rails |
| ORM | own / sea-query | own / sea-query | SeaORM (mature, 3rd-party) |
| Auto-migrations + inspectdb | ✓ (+ inspectdb) | ✓ | ✓ (SeaORM migrator) |
| Admin panel | ✓ rich (HTMX, ApexCharts dashboards, sheets, bulk actions) | ✓ | ✗ |
| Auth / sessions / permissions | ✓ (3 plugins + RLS) | ✓ | ✓ |
| Background tasks | ✓ umbra-tasks (has correctness bugs — see broken-features.md) | ✗ (not found) | ✓ workers + scheduler |
| DRF-style REST + OpenAPI + playground | ✓ full stack | OpenAPI only | controllers only |
| Plugin breadth | rls, cache, email, media, health, signals, openapi, playground all first-class | focused core | focused core |
| Secure-by-default | **opt-in (round-one gap)** | **opt-out (their brand)** | partial |
| Maturity / traction | greenfield, solo, unpublished, placeholder name | v0.6, ~940★, shipping releases | most mature, biggest community |

## ORM call-site & data-layer comparison

Three differences a developer feels on line one — the kind of thing that decides a framework evaluation before any feature list is read.

### 1. Database handle — ambient (umbra) vs explicit (cot)

```rust
// cot.rs — the db handle is threaded into every terminal
let link = query!(Link, $slug == LimitedString::new("cot").unwrap())
    .get(request.db())
    .await?;

// umbra — the pool is ambient; nothing is threaded
let link = Link::objects()
    .filter(link::SLUG.eq("cot"))
    .first()
    .await?;
```

| | umbra | cot.rs |
|---|---|---|
| DB source | Ambient `OnceLock` pool set at `App::build()` | Explicit `request.db()` passed to each terminal |
| Call site | Reads like Django's `Model.objects.get()` — nothing plumbed | DB dependency visible at every call |
| Cost | One intentional global (the *only* one; `.on(&pool)` is the test escape hatch) | Verbosity; needs a `request`/`&Database` in scope everywhere |
| Idiom | Convenience-first (Django feel) | Explicitness-first (functional-Rust honest) |

Both are defensible — they optimize *different virtues*. umbra's pitch ("feels like Django") and cot's ("explicit and honest") are each true; a developer trying both feels this on the first query. This is the clearest ergonomic expression of umbra's "Django feel" claim.

### 2. Query syntax — fluent typed builder (umbra) vs macro DSL (cot)

- umbra: `.filter(link::SLUG.eq("cot"))` — generated typed column constants, IDE-completable, no macro.
- cot: `query!(Link, $slug == ...)` — a macro DSL that reads like Django's `filter()` kwargs.

### 3. Multi-database routing — a genuine umbra edge

umbra already ships the **static foundation** of Django's database-router system (verified in `crates/umbra-core/src/db.rs`, `migrate.rs`, `orm/queryset/mod.rs`):

- **Named pools by alias** — `databases` settings map (`UMBRA_DATABASES__REPLICA=…` / `umbra.toml`) + builder `.database(alias, pool)`; stored in `POOLS: HashMap<String, DbPool>`.
- **Per-model routing** — a model declares its DB via `Plugin::database()` / `Model::DATABASE`; `resolve_pool::<T>()` routes automatically with precedence **explicit `.on(&pool)` → per-model alias → default**.
- **Per-DB migrations** — the migrate engine walks every registered alias, routes each op by `table_alias()`, and keeps a per-DB tracking table.

**Not yet built** (the replica layer): read/write splitting (`db_for_read` vs `db_for_write`), and a dynamic router object (Django's `allow_relation`/`allow_migrate`). Today routing is static one-model-one-DB. Adding read-replicas means extending `resolve_pool` to split read vs write terminals + a router abstraction above the flat alias map.

Why this matters competitively: cot's explicit-handle model has **no model-routing convention** — multi-DB there is "pass a different handle by hand" at each call. umbra bakes the Django routing *convention* into the ORM, so the framework decides the pool from the model. Once read-replica support lands, this is headline-worthy ("declare which database a model lives on; reads scale to replicas for free") — and it's a capability neither cot nor a raw-axum stack offers out of the box. Flag it as a roadmap differentiator, not a shipped one, until the read/write split exists.

## Scorecard on the named dimensions

- **Performance:** Wash at the HTTP layer — all three sit on axum/tokio; throughput is dominated by the DB and ORM, not the framework. All crush Django/Rails on latency/memory. umbra's engine batching is genuinely good (no N+1 in hydration; COUNT pushdown; single-statement bulk ops). umbra loses points only on **defaults**: no FK auto-index, unbounded REST pagination default, bulk_create validation N+1 (see performance.md). Fixable, not architectural.
- **Ease of use:** All Rust batteries-frameworks are "harder than Django, easier than raw axum" — compile times, async, borrow checker are a tax no abstraction fully removes. umbra's `prelude::*` + derive macros + declare→migrate loop are the right ergonomic bets; on par with cot, tighter than loco's SeaORM verbosity.
- **Building structure:** **umbra's strongest card.** Workspace-as-architecture, facade pattern, the Plugin trait as the single dynamic seam, dependencies-point-inward enforced by Cargo's circular-dep ban. More principled than loco's monolith; at least as clean as cot. Lead here for a technical audience.
- **Completeness:** Feature-*broad* but not prod-*ready*. More plugins than a pre-1.0 framework usually has, but round one found the gaps that separate "demos well" from "runs in prod": opt-in security defaults, ORM correctness holes (tasks double-claim, missing select_for_update), i64-PK lock-in (refactor already planned). cot and especially loco are closer to "ship a real app this weekend."

## Strategic read

umbra is **strong framework engineering in an already-occupied niche.** cot.rs is the same idea — "Django for Rust on a bespoke sea-query ORM" — and is publicly further along (released, ~940★, regular cadence, multi-contributor). So the wedge cannot be "Django for Rust" in the abstract; that slot is taken.

**Defensible wedges, ranked by how real they are in the code:**
1. **A real background task queue + a full DRF-equivalent (serializers/viewsets/routers) + OpenAPI + interactive playground.** cot has neither in full; loco has workers but no DRF/admin. This is umbra's clearest "does more" story — *once umbra-tasks' correctness bugs are fixed.*
2. **The most radically decomposed plugin architecture in Rust web** — auth/sessions/admin/tasks/REST all plugins via the same trait a third party uses, enforced by crate boundaries. A true, demonstrable, architecturally-enforced claim.
3. **Admin depth + Django-shape vs loco** (loco has no admin); **breadth of built-in plugins vs cot.**

**The hard truth the sweep surfaced:** cot has already claimed "secure by default" as its brand. umbra cannot out-market cot on Django-ergonomics-plus-safety while shipping opt-in security defaults. **Fixing the round-one security theme (auto-mount SecurityPlugin, default REST to authenticated/read-only, wire the RLS context, gate is_superuser edits) is now a competitive necessity, not just hygiene** — it's the price of entry to compete with cot on cot's own headline.

**Risks to stay clear-eyed about:** solo maintenance vs a multi-contributor rival; a placeholder name with zero SEO/mindshare; pre-prod security posture undercutting the core pitch; and the genuine question of whether to differentiate hard (lean into the task-queue + REST + plugin-architecture wedge) or accept overlap with cot and compete on execution/polish.

## Suggested one-line pitches (pick by audience)
- Technical: *"The most modular Rust web framework — every batterie, even auth, is a plugin the framework can't distinguish from yours."*
- Django refugees: *"Django's feel — models, migrations, admin, a real REST framework and task queue — with Rust's guarantees."*
- Honest internal: *"cot's niche, broader batteries (tasks + DRF + OpenAPI), a cleaner plugin architecture — contingent on closing the security-defaults and tasks-correctness gaps to reach parity on cot's secure-by-default brand."*

## Sources
- cot GitHub — https://github.com/cot-rs/cot
- cot guide — https://cot.rs/guide/latest/
- cot announcement — https://mackow.ski/blog/cot-the-rust-web-framework-for-lazy-developers/
- InfoWorld on cot — https://www.infoworld.com/article/3832992/cot-framework-aims-to-ease-rust-web-development.html
- loco.rs — https://loco.rs/
- loco GitHub — https://github.com/loco-rs/loco
