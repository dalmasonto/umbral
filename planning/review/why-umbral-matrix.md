# Why umbral: an honest comparison matrix

Status: maintained draft (gaps5 #7, tf#220). Internal, org-facing. Adaptable later to a docs-site "Why umbral" page once facts are re-verified for public use.

Last updated: 2026-08-08. umbral facts are grounded in `FEATURES.md`, the `plugins/*` tree, and `planning/review/competitive-positioning.md` (whose 2026-08-08 correction is carried here: secure-by-default and umbral-tasks correctness are FIXED, not open gaps). Competitor facts were last swept 2026-06-10 in that same file; anything marked "verify" below needs a fresh check before public use, because Cot and Loco move fast.

## What this document is and is not

This is an outcomes matrix, not a benchmark. The rows are the things a team actually asks about when it picks a backend framework ("do I get an admin for free?", "is auth a plugin or bolted into the core?"), not requests-per-second. At the HTTP layer umbral, Loco, and Cot all sit on axum/tokio, so raw throughput is a wash between them and is dominated by the database and the ORM, not the framework. Where performance matters to an org is the interpreted-vs-compiled gap, and that is captured in the "compile-time guarantees" and "maturity" rows, not a latency table.

We are scrupulous about umbral's alpha status. It is v0.0.11 on crates.io, effectively solo-maintained, carries a placeholder name, and is not something to run a production-critical service on today. Where a competitor is more mature we say so plainly: Cot ships and has real mindshare; Loco is the most mature of the batteries-included Rust set and has the biggest community.

## The honest wedge, stated first

umbral's differentiator is not "the batteries-included Rust framework in the abstract." That slot is contested (Cot is the same thesis and is publicly further along). umbral's real, code-enforced wedge is two things:

1. **The most radically decomposed plugin architecture in Rust web.** Every capability, auth included, is a `Plugin` behind the exact same trait a third party implements. `umbral-core` names no concrete plugin and touches them only as `Box<dyn Plugin>`; Cargo's ban on circular crate dependencies makes "serializers are just a plugin" a structural fact, not a slogan. A built-in and a stranger's crate have identical standing. No privileged core.
2. **Breadth that some peers lack.** A real background task queue *with a cron/beat scheduler*, a full serializer/viewset/router REST framework *plus* OpenAPI *plus* an interactive playground, a derived GraphQL API, an auto-CRUD admin, realtime (SSE + WebSocket), storage (static + uploads + S3), schema-per-tenant multitenancy, RBAC permissions, and Postgres RLS. Cot has no background queue and OpenAPI-only REST (no serializer/viewset stack); Loco has workers + scheduler but no admin and controllers rather than a full REST framework.

Everything below substantiates or qualifies those two claims. It also names, without flinching, the places umbral is behind.

## The layer map: most "competitors" are not on the same layer

Before the matrix, the honest framing: several names people reach for are not actually alternatives to umbral. They are substrate umbral is built on, or a different layer entirely.

| Layer | What it solves | Members | Relationship to umbral |
|---|---|---|---|
| Async / HTTP plumbing | event loop, sockets, routing | tokio, hyper, **axum**, **Actix Web** | umbral is built on axum + tokio. Substrate, not a rival. Actix Web competes at *this* layer, not at the batteries layer. |
| Web framework (routing + typing, bring your own everything else) | request routing, extractors, guards | **Rocket**, axum, Actix Web | A framework, but you assemble ORM, migrations, admin, auth yourself. |
| Assemble-it-yourself batteries | you wire the pieces | **Axum + SeaORM (+ tower, + your glue)** | The DIY baseline. Maximum control, maximum wiring. |
| Batteries-included backend | declare data, get migrations + CRUD + admin + API | **umbral, Cot, Loco** | The actual competitive set. |
| Interpreted-language incumbents | the original "declare data, get everything" | **Django, Rails, Laravel** | The experience umbral is trying to recreate with compile-time guarantees. |

So Rocket and Actix Web are honest comparisons only in the sense that a team *could* pick "a lean web framework plus hand-rolled batteries" instead of a batteries-included one. That is a real choice, and the matrix includes them, but the comparison is "batteries out of the box vs. build them yourself," which the rows make explicit.

## The matrix

Legend: **Yes** = shipped and usable. **Partial** = present but narrow, alpha, or with documented deferrals. **No** = not provided by the framework. **DIY** = you assemble it from third-party crates yourself. **Ecosystem** = not in core but a well-worn library exists.

| Outcome a team cares about | umbral (alpha) | Loco | Cot | Axum + SeaORM | Rocket | Actix Web | Django / Rails / Laravel |
|---|---|---|---|---|---|---|---|
| Declare a model, get migrations + CRUD + API + admin | Yes | Partial (no admin) | Yes | DIY | DIY | DIY | Yes |
| Managed migrations + autodetection | Yes (+ `inspectdb`) | Yes (SeaORM migrator, hand-written) | Yes | DIY (SeaORM migrator) | No | No | Yes |
| Admin UI (auto CRUD) | Yes (HTMX, dashboards, bulk actions, custom views) | No | Yes | DIY | No | No | Yes (Django strongest; Rails/Laravel via gems/packages) |
| REST framework: serializers + viewsets + router | Yes (full stack) | Partial (controllers, not a serializer/viewset framework) | No (OpenAPI gen only) | DIY | DIY | DIY | Yes (DRF for Django; Ecosystem for Rails/Laravel) |
| OpenAPI / Swagger | Yes (spec + Swagger UI + playground) | Ecosystem | Yes | DIY | Ecosystem | Ecosystem | Ecosystem |
| GraphQL | Yes (`umbral-graphql`, derived from models) | No | No | DIY (async-graphql) | DIY | DIY | Ecosystem |
| Auth / sessions / permissions | Yes (auth + sessions + RBAC permissions plugins) | Yes | Yes | DIY | DIY | DIY | Yes |
| Row-Level Security (Postgres RLS) | Yes (`umbral-rls`, policy declaration) | No | No | DIY | No | No | Ecosystem / manual |
| Multitenancy | Yes (`umbral-tenants`, schema-per-tenant, Postgres) | Partial (patterns, not a plugin) | No | DIY | No | No | Ecosystem |
| Background tasks + scheduler | Yes (DB-backed queue + cron/beat, `run_beat`) | Yes (workers + cron scheduler) | No | DIY | No | No | Yes (Celery / Sidekiq / queues) |
| Storage (static + uploads + S3) | Yes (`umbral-storage`, unified, S3 backend) | Partial | Partial (static files) | DIY | Partial (static) | Ecosystem | Yes |
| Realtime (SSE / WebSocket) | Yes (`umbral-realtime`, per-user + rooms) | Ecosystem | Partial (WebSocket, verify) | DIY | DIY | Yes (actix has WS) | Ecosystem (Channels / Action Cable) |
| Email | Yes (`umbral-email`, SMTP + console dev backend) | Yes (mailers) | Yes | DIY | DIY | DIY | Yes |
| Cache | Yes (`umbral-cache`, trait + in-memory + SQLite) | Ecosystem | Verify | DIY | DIY | DIY | Yes |
| Secure by default | Yes (SecurityPlugin auto-mounted: CSRF, headers, autoescape, parameterized SQL) | Partial | Yes (their headline brand) | DIY | Partial | DIY | Yes |
| Plugin architecture: swappable built-ins, no privileged core | Yes (single `Plugin` trait, Cargo-enforced) | No (monolith) | No (focused core, feature flags) | n/a | No | No | Partial (Django apps; Rails engines) |
| Compile-time guarantees (types, nullable = `Option<T>`, `?` errors) | Yes | Yes | Yes | Yes | Yes | Yes (framework itself is typed Rust) | No (runtime-checked) |
| Backend support | Postgres + SQLite only | Postgres, SQLite, MySQL (via SeaORM) | Postgres, SQLite, MySQL (verify) | Postgres, SQLite, MySQL (SeaORM) | DB-agnostic (DIY) | DB-agnostic (DIY) | Postgres, MySQL, SQLite, more |
| Maturity / production readiness | Alpha (v0.0.11, solo, placeholder name, not prod-ready) | Mature, biggest Rust-batteries community | Shipping (v0.6, ~940 stars, multi-contributor, self-described not-yet-prod) | ORM mature; the "framework" is your own code | Mature, stable | Very mature, high performance | Battle-tested, decades of production |

### Reading the matrix honestly

- **Where umbral genuinely leads the Rust batteries set:** breadth in one coherent contract. It is the only one of umbral / Cot / Loco that ships *all* of {full REST framework, OpenAPI + playground, GraphQL, admin, background queue + scheduler, realtime, RLS, tenants} behind one plugin trait. That is the "does more, in one shape" story, and it is real in the tree (`plugins/*` has 22 crates, each a `Plugin`).
- **Where umbral ties:** compile-time safety (all Rust frameworks win this over the interpreted incumbents), managed migrations (umbral and Cot both autodetect; Loco hand-writes SeaORM migrations), secure-by-default (umbral now matches Cot's posture after the 2026-08-08 fixes; this was an open gap and is not one anymore).
- **Where umbral is behind, plainly:** maturity and community. Loco is the mature choice with real apps in production; Cot is shipping releases with mindshare and coverage. umbral is alpha, solo, and unproven in production. Also: fewer backends (Postgres + SQLite only; no MySQL) than SeaORM-based peers, and several umbral plugins carry documented deferrals (for example the permissions admin UI is deferred; some Postgres operators are Postgres-only). "Partial" and "alpha" in the matrix are load-bearing, not modesty.
- **Where the DIY column wins:** total control and no framework opinions. Axum + SeaORM, Rocket, and Actix Web give a team that wants to own every layer exactly that. The cost is that every row marked "DIY" is work that team now signs up to build and maintain. umbral's pitch to them is "you do not have to build and own all of that," not "you cannot do it yourself."
- **Against the interpreted incumbents:** Django, Rails, and Laravel are the experience umbral is chasing, and they win decisively on maturity, ecosystem, and hiring pool. umbral's only honest edge over them is Rust's compile-time guarantees (a nullable column is `Option<T>`, errors are `Result` values, SQL is always parameterized) and the resource profile that comes with a compiled binary. That edge is real but it does not, today, outweigh their maturity for most teams.

## When NOT to pick umbral yet

Say this plainly, because it protects the project's credibility:

- **You are shipping something production-critical now.** umbral is v0.0.11 alpha, solo-maintained, and its name is a placeholder. Pick Loco (most mature Rust batteries) or Cot (shipping, more mindshare), or an interpreted incumbent, if downtime or data loss is unacceptable and you need it this quarter.
- **You need MySQL or any database other than Postgres or SQLite.** umbral is Postgres-first with SQLite for tests, and that is the whole supported set today. SeaORM-based stacks (Loco, Axum + SeaORM) cover MySQL; the interpreted incumbents cover far more.
- **You need a mature ecosystem, plugin marketplace, Stack Overflow answers, or a hiring pool.** umbral has none of that yet. Every non-trivial problem you hit is one you or the maintainer solves first. Django / Rails / Laravel are the opposite end of this axis, and even Loco has a real community.
- **You want a stable public API you can build on for years.** umbral is pre-1.0 and the surface will move. The facade (`umbral::prelude`) is designed to keep churn contained, but "designed to" is not "guaranteed to" at this stage.
- **You want to own every layer and dislike framework opinions.** Then the assemble-it-yourself stack (Axum + SeaORM, or Rocket / Actix Web plus your own glue) is the honest fit, and umbral's batteries are a cost, not a benefit, to you.

If none of those apply, and what you want is the "declare your data and get migrations, an ORM, an admin, REST, OpenAPI, GraphQL, tasks, and realtime, all as swappable plugins, with Rust's guarantees" experience, and you can tolerate alpha software, that is exactly the seat umbral is built for.

## One-line pitches (pick by audience)

- Technical: "The most modular Rust web framework: every capability, even auth, is a plugin the framework cannot tell apart from yours."
- Refugees from interpreted-language batteries frameworks: "The same declare-and-get-everything feel, plus a real REST framework, task queue, and admin, with Rust's compile-time guarantees. Alpha today."
- Honest internal: "Cot's niche, broader batteries, a cleaner plugin architecture. Behind on maturity and community, and Postgres/SQLite only. The wedge is the plugin contract plus breadth, not being first to the slot."

## Facts to re-verify before public use

The competitor cells were last swept 2026-06-10 (see `competitive-positioning.md` sources). Flag these specifically before publishing anything outward:

- **Cot:** current version and star count (was v0.6, ~940 stars, 2026-06-10); whether it now ships any background job queue; whether it ships WebSocket support and a cache layer; MySQL support (assumed via its sea-query lineage but not confirmed here). Cells marked "verify" above are the ones I did not independently confirm for Cot.
- **Loco:** whether an admin panel has been added since the sweep (was "none advertised"); exact scheduler shape; storage/realtime story (marked Partial / Ecosystem from general knowledge, not a fresh check).
- **Rocket / Actix Web:** their batteries cells are marked DIY / Ecosystem from general knowledge of those projects, not a 2026 sweep. The claims are conservative (they do not ship ORM/migrations/admin), so the risk is low, but confirm before a public page.
- **umbral self-claims:** all traced to `FEATURES.md` and the `plugins/*` tree as of 2026-08-08. The "secure-by-default shipped" and "tasks correctness fixed" claims come from the 2026-08-08 update in `competitive-positioning.md` and should be re-confirmed against current code at publish time, since they are the two claims most likely to be challenged.
