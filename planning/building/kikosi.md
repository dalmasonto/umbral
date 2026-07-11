# Heavy items harvested from Kikosi (web3clubs_fc) — a live umbral consumer

**Provenance.** Kikosi (`/home/dalmas/E/projects/web3clubs/web3clubs_fc`) is a real
production umbral app: a football-club members portal (Rust `umbral` backend on a
plugin-per-domain layout + a React/Vite/Capacitor SPA). It runs on umbral 0.0.6 in
prod (Postgres, docker-compose, Caddy). These items came out of actually building
and shipping features on it (match-total split, multi-match-per-fixture,
admin-created members, presence, realtime, deploys).

**Honest framing.** umbral is already mature: it ships crates for most "heavy"
capabilities — background jobs (`umbral-tasks`), file storage (`umbral-storage`),
email (`umbral-email`), caching (`umbral-cache`), row-level security
(`umbral-rls`), object/model permissions (`umbral-permissions`), and realtime even
has a `RedisBroker` multi-instance seam (`umbral-realtime`). So these are **not
"missing features."** They are the **cross-cutting architecture a developer still
has to assemble themselves** — the stuff that's heavy to build per-app, easy to
get subtly wrong, and hostile to retrofit. Ranked by leverage (highest first).

Small, already-filed papercuts from the same consumer live in `gaps3.md` as **#36**
(no `Cache-Control` on REST/JSON responses) and **#37** (a fresh consumer
hand-rolls `RequireStaff` / `db::transaction` / `trim,lowercase` — a discoverability
signal). This file is the *heavy* list; it pairs with the open `gaps3.md` **#29**
(boilerplate reduction / what else can move to the framework).

---

## 1. End-to-end type safety — a generated typed client from the backend schema

**The single biggest ongoing tax on this app.** `umbral-openapi` already emits an
OpenAPI spec, but there is no generated client, so Kikosi hand-maintains the
*entire* client data layer: every DTO interface, every URL in `endpoints.ts`, and
every typed function in `lib/services/*` — kept in sync with the Rust models by
hand and memory. Every schema change paid the tax twice: adding `total_amount`,
then `match_no`, then the optional `username` each meant editing the Rust model
**and** hand-mirroring a DTO + service + mapping in TypeScript, with nothing but
discipline stopping drift. This is a whole class of avoidable bugs (a field renamed
on one side, a nullable the client doesn't expect — exactly what bit the
`created_by → number | null` change).

**Proposed.** A first-party `umbral-openapi → TypeScript client + types` codegen
(the tRPC / `openapi-typescript` / `orval` experience): typed request/response
models generated from the same source of truth that renders the OpenAPI doc, a
thin typed `fetch` client, and enums/choices carried through. Ship it as a CLI
step (`umbral gen-client --lang ts --out …`) so the SPA imports generated types
instead of re-declaring them.

**Why heavy / why framework.** No single crate covers it; it spans the
schema → codegen → client-runtime boundary; and it's the difference between a
backend framework and a full-stack one. Highest leverage because *every* umbral app
with a real frontend pays this tax forever.

**Progress (2026-07-11) — the query client shipped.** `umbral typegen` (types)
landed earlier; now `umbral gen-client --out <dir>` (`plugins/umbral-openapi/src/client_gen.rs`)
emits the typed query client Kikosi hand-maintained: `new Umbral(url).from("post").filter({...})`
where the filter map autocompletes to the model's filterable fields with correct
value types (FK → target PK type, choices → union, `__gte`/`__in`/`__contains`/`__isnull`
per the REST contract). Offline CLI step, `--check` CI gate, `tsc --strict`-verified
(rejects typo'd choices, unknown fields, wrong value types, FK-as-number). This is
exactly the `total_amount`→`match_no`→`username` drift class eliminated. Still open
here: the auth/session client + optimistic-update patterns (small); the bulk of #2
is the realtime cache-invalidation client — but see below, the realtime *backend* is
already done.

## 2. An official client SDK / SPA-integration story

Same theme, one level up. umbral gives you an excellent backend, but the *entire*
client integration in Kikosi is bespoke and non-trivial: the `lib/api.ts` swap
boundary, a hand-written fetch-based SSE reader (`lib/realtime.ts`), bearer-token
storage + 401 handling, presence bookkeeping, and — where the real bugs lived —
optimistic-update + refetch + cache-invalidation logic (the stale-data `no-store`
workaround and the `setMemberRsvp` self-overlay revert both came from this layer).
The app even hand-rolls a `notify_change(resource, id)` broadcast on the backend and
a `switch` on the client to refetch the affected slice.

**Proposed.** A framework client SDK (building on #1): a realtime subscription
client that maps model-change events → client cache invalidation (so apps stop
hand-writing `notify_change` + the client switch), an auth/session client, and a
documented optimistic-update-then-reconcile pattern. This is what makes
Phoenix LiveView / Rails-Hotwire / tRPC feel "batteries included."

**Why heavy.** It's an entire product surface (a client library + a realtime cache
protocol), and it's where correctness bugs concentrate — every app reinvents it,
slightly wrong.

**Correction (2026-07-11).** The realtime *backend* half is already shipped and
this note is stale: the ORM auto-emits `post_save`/`post_delete` signals, and
`RealtimePlugin::expose::<T>(...)` bridges them to the realtime stream with default-
deny, field-projected safety, consumed by the served `client.js`'s
`umbral.realtime.model('post', {created,updated,deleted}, {group})`. So `notify_change`
is redundant — a consumer replaces it with one `.expose::<T>(...)` call.

**Update (2026-07-11) — the typed realtime client shipped too.** `gen-client` now
emits `client.on("post", {created,updated,deleted}, {group})` over the existing SSE
stream: `"post"` autocompletes to exposed tables, each handler gets a typed
`Partial<Row>` (the `expose` projection), returns a `Subscription` with `.close()`.
tsc-verified. So the realtime-cache-invalidation half of #2 is done — the developer
writes `expose::<Post>(...)` once server-side and gets a typed client subscription,
replacing Kikosi's hand-rolled `notify_change` + client `switch`. Still open in #2:
the auth/session client helper + a documented optimistic-update-then-reconcile pattern.

## 3. Multi-tenancy as a posture, not a pile of parts

Kikosi's own roadmap is single-club → multi-club SaaS (a `Club` model,
multi-club membership; see the app's `multi-tenancy-direction` note). umbral ships
the *parts* — `umbral-rls` (Postgres RLS) and `umbral-permissions` — but safe
tenancy is an **architecture**, not a crate: tenant resolution middleware
(host/subdomain/header), **automatic** query scoping so a developer *cannot forget*
a `WHERE tenant_id = …` (one missed filter is a cross-tenant data-leak incident),
per-tenant config, and a per-tenant migration/seed story.

**Proposed.** A first-class tenancy layer: a `Tenant` context extracted once per
request, ORM-level default scoping (opt-out explicitly, not opt-in), and RLS
wired from the same tenant context as defense-in-depth. Make leaking data the
thing you have to work *at*, not the default failure mode.

**Why heavy.** Security-critical, spans routing + ORM + migrations + admin, and is
extremely painful to retrofit onto an app that assumed single-tenant (which is why
it belongs in the framework before the app grows into it).

## 4. Background jobs + scheduling as the obvious default

`umbral-tasks` exists (DB-backed queue + worker), but Kikosi still dispatches FCM
push **fire-and-forget, inline** in the request handler (`fc_push::dispatch_to_*`)
— no retry, no backoff, no visibility. Anything asynchronous (push, the
password-reset/verification emails, match reminders, digests, cleanup) wants a
durable job.

**Proposed.** Make jobs the blessed default, not an add-on: an ergonomic enqueue
(`#[job]` / `enqueue(MyJob{…})`), a worker service that ships in the compose
template, retries + backoff + dead-letter, **cron/scheduled** jobs (match-reminder
one hour before kickoff is the canonical example), and job observability in admin.

**Why heavy.** The queue crate is the easy 20%; the ops story (worker lifecycle,
retries, scheduling, dead-letter, monitoring) is the 80% every app re-derives.

## 5. The production / ops story

Deploying Kikosi is hand-assembled: a GH Actions workflow that builds the SPA +
scp's it, then a **manual** `docker compose build && up -d` on the VPS for the Rust
backend, Caddy serving the static SPA + reverse-proxying the API, a one-shot
`migrate` service, and secrets via sops+age. It works — but every umbral app
re-invents it, and there are concrete papercuts: the backend container's
healthcheck is a **known false alarm** (`(unhealthy)` while it serves 200s), and
there's no blessed zero-downtime story.

**Proposed.** A framework "production" opinion: correct `/healthz` + `/readyz`
endpoints (readiness gates on DB + migrations), graceful shutdown, a documented
migrate-on-boot vs. one-shot-migrate policy, and a reference deploy recipe
(compose + reverse proxy + zero-downtime rollout) so apps inherit it instead of
hand-rolling.

**DONE (2026-07-10) — framework side fully closed.** All four sub-parts landed:

1. **Correct `/healthz` + `/readyz`, readiness gates on DB + migrations.**
   `umbral-health` gained `HealthPlugin::require_migrations()` (opt-in): `/ready`
   (and the new `/readyz` alias) returns 503 while any on-disk migration is
   unapplied — closing the rolling-deploy race where a `web` container boots
   before the one-shot `migrate` finishes and 500s against the old schema. A
   rollback (DB ahead) stays ready. Backed by a read-only
   `umbral::migrate::drift_report()` (no-print sibling of `show()`) +
   `DriftReport::pending()`.
2. **Graceful shutdown + zero-downtime drain.** Graceful in-flight drain already
   existed (`App::serve` → `with_graceful_shutdown`, audit_2 core-app-config
   #13). Added `AppBuilder::shutdown_drain(Duration)` + `umbral::shutdown`
   (`is_draining`/`begin_drain`): on SIGTERM the process flips `/readyz` to 503
   immediately, keeps serving for the delay so the LB drains it, THEN stops
   accepting. This is the actual zero-downtime mechanism. Default `ZERO` (no
   drain) preserves instant Ctrl-C for dev/single-instance.
3. **Migrate-on-boot vs one-shot policy** — documented in
   `deployment/migrations-in-production.mdx`, grounded in the existing Postgres
   advisory lock (`pg_try_advisory_lock`, per-alias key, 300s timeout,
   auto-release on crash) that makes concurrent migrate-on-boot across replicas
   safe.
4. **Reference deploy recipe** — `deployment/going-to-production.mdx`: compose
   (db → migrate → web) + reverse proxy + the full zero-downtime rollout
   sequence tying readyz + drain + graceful shutdown together.

The false-alarm healthcheck this item named (a container curling the *home page*
instead of a probe endpoint) is now called out explicitly in the deploy docs;
the framework fix is available. **Remaining is consumer adoption, not a framework
gap:** umbral_website's `docker-compose.yml` still curls `/`, and Kikosi
inlines its FCM push — both fixed by pointing HEALTHCHECK at `/readyz` +
`.shutdown_drain(...)` once they take the release that carries these APIs.

**Why heavy.** It's cross-cutting (health, lifecycle, migrations, deploy) and it's
where "works on my machine" meets real uptime.

## 6. Media / file-upload pipeline (crate exists, pipeline doesn't)

`umbral-storage` stores files (local FS / S3), but the heavy layer is the
*pipeline*: multipart upload handling, validation (type/size), image
resize/thumbnail/format-convert, signed URLs, and direct-to-S3 uploads. Kikosi
sidestepped it (avatars are deterministic generated colors, the logo is a static
asset), but any app with user-generated images needs it.

**Proposed.** An uploads pipeline on top of `umbral-storage`: a form/field type for
uploads, pluggable processors (image resize/thumbnail), and signed-URL delivery.

## 7. Audit trail + soft delete + data lifecycle

Kikosi hand-rolls audit-ish fields (`created_by` / `recorded_by`, which we just made
`SET NULL` on user delete) and **hard-deletes** rows (see the manual, careful
member-deletion cleanup we had to do by hand). Common, retrofit-hostile needs:
automatic per-model change history (who changed what, when), a soft-delete mixin
(`deleted_at` + default-exclude), and data export/retention (GDPR "download/delete
my data").

**Proposed.** Opt-in model mixins: `#[umbral(audited)]` (writes a history row per
change) and `#[umbral(soft_delete)]` (sets `deleted_at`, filters by default,
cascade-aware), plus a per-user data-export helper.

**Why heavy.** Touches the ORM write path, querying defaults, cascade semantics, and
admin — and is very hard to add after an app has hard-deleted for a year.

---

## Suggested sequencing

**#1 → #2 first.** The typed-client / SPA-integration story is the biggest,
most-universal tax and unlocks everything client-side. **#3 (tenancy)** should land
before any consumer commits to a single-tenant schema it can't walk back. **#4/#5**
(jobs + ops) are the "make the happy path production-grade" pair. **#6/#7** are
valuable but app-triggered — build when a consumer actually needs them.
