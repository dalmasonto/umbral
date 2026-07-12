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

**Update (2026-07-11) — write DTOs, per-model ids, configurable pagination + adaptive auth.** Follow-on passes hardened the client so it matches *any* umbral app, not just the all-i64/all-bearer happy path: (a) typed `create`/`update` DTOs respecting `noform`/`noedit`/`auto_now*` + hidden-as-write-only; (b) `get`/`update`/`delete` id typed per model (`number` for i64, `string` for Uuid/String-slug PKs — the old global `UmbralId` union was a lie), tsc-proven across all three PK shapes; (c) the list envelope + query-builder now adapt to the paginator — page-number/limit-offset known, and a custom paginator that declares `Pagination::schema()` is emitted fully typed (its own `next_cursor`/`has_more` + `.cursor()`), else an honest open envelope + generic `.param()`; (d) auth is read from `registered_security_schemes()` — the `Authorization` prefix from the scheme (`Bearer`/`Token`), the api-key header from the scheme's `name` (`x-umbral-api-key`), cookie→credentials, plus `getAuthHeaders()` for JWT refresh. So the two things that were hardcoded (Bearer, `{results,count}`) now follow what the app declares. Remaining under #1: the auth/session *login* client (token acquisition/refresh flow) + optimistic-update helpers. Two logged follow-ups: OpenAPI *spec* should emit custom-pagination params from the same `schema().params`; a standalone pagination doc page.

**DONE (2026-07-12) — #1 closed.** The last gap named above (the auth/session *login* client) shipped: `client.auth.login/logout/me/register`, discovered from the paths plugins publish via `Plugin::openapi_paths()` keyed on `operationId` (so a remounted prefix still works, and a REST-only app generates no `auth` namespace at all — no dead code). Types come from the *published* request/response schemas, so they can't drift from the contract; this surfaced a real spec bug — umbral-auth's user response declared no `required`, which would have typed every field as `string | undefined` — now fixed in `auth_routes.rs`. Signing in is enough: the token is held by the client and every later request sends it (verified in node — a plain `.from(...).list()` after `login()` carries `Bearer …` with nothing threaded by hand); `me()` resolves `null` rather than throwing on 401; `logout()` drops the token even if the server call fails. The client deliberately never writes the token to web storage (XSS-readable) — the app opts in via `onToken`. Also shipped alongside: `gen-client` now emits `client.js` (one self-contained ES module — usable with no toolchain) + `client.d.ts`, so the runtime exists exactly once.

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
replacing Kikosi's hand-rolled `notify_change` + client `switch`.

**DONE (2026-07-12) — #2 closed.** The two remaining pieces landed. (a) The **auth/session client** — see #1's closure note. (b) The **optimistic-update-then-reconcile pattern** is documented in `rest/typescript-client.mdx`, naming the two failure modes Kikosi actually hit: keeping the optimistic guess instead of reconciling with the server's returned row, and letting a realtime `updated` echo of *your own* write clobber a newer local edit (the `setMemberRsvp` self-overlay bug). Also fixed in this arc: `.on(...)` used to open one `EventSource` **per subscription** — five models meant five SSE connections per tab against a ~6-per-origin browser cap. It now delegates to the realtime plugin's served runtime (SharedWorker, ONE connection shared across all tabs, presence, degradation), and a test asserts the generated client never constructs a transport of its own.

## 3. Multi-tenancy as a posture, not a pile of parts

**RESOLVED (2026-07-12) — this was the wrong tool; the real gap was smaller and is now closed.** Row-level tenant scoping was designed, then deliberately **not built**. The consumer's actual flow: *"I'm in web3clubs FC, later I join another club — same account, and I can see my clubs."* One user, many clubs, joined like groups. **That is not multi-tenancy.** Tenancy means isolated customers who must never see each other, and both tenancy tools actively break the flow: schema-per-tenant (already shipped in `umbral-tenants`) would scatter one account across schemas and make "my clubs" a cross-schema union; row-level auto-scoping pins every query to *one* tenant, which is exactly what makes belonging to two clubs impossible. The correct shape is ordinary modelling — a `Club` model, a `Membership` join model, plain FKs. No framework feature needed.

The real risk in that design is **authorization**, not a forgotten `WHERE club_id`: an endpoint that fails to check "is the caller a member of *this* club". `ResourceConfig::scope` was the right hook but could not express it — `ScopeDecision::Restrict` is equality-only and ANDed (`club_id = 1 AND club_id = 2` matches nothing), and the hook was **sync**, so it could not run the membership query at all. So: `ScopeDecision::RestrictIn(col, values)` (`col IN (…)`, with **empty ⇒ DenyAll**, never "unconstrained" — "you joined nothing" must not become "you see everything") plus `ResourceConfig::scope_async` for the DB-backed lookup. Proven by `plugins/umbral-rest/tests/membership_scope.rs`: a user in two clubs sees both; a non-member gets **404, not 403** (a 403 would confirm the row exists); a user who joined nothing sees nothing. Full write-up: `docs/specs/row-level-tenancy.md`.

Build row-level scoping only if a real driver appears — thousands of tenants (Postgres schema catalogs suffer past ~1k), or cross-tenant analytics in one query. Neither is true today.

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

**VERIFIED (2026-07-12) — largely already shipped; closing.** Audited against the code rather than assumed:

- **Retries + backoff — DONE.** `RetryPolicy` (`umbral-tasks/src/lib.rs:602`), exponential `backoff_delay()` (`:634`, `base * 2^(attempts-1)`, clamped), default 3 attempts, 2s base / 5min cap. Plus a per-task timeout and **panic capture** (`:1005`), both counted as retriable failures.
- **Cron / scheduled — DONE.** `Schedule::cron()` (`:1291`, real 5-field cron via the `cron` crate) and `Schedule::every()` (`:1296`), a `PeriodicTask` model (`:1362`), and a beat loop (`run_beat` `:1522`) that claims due rows with a conditional UPDATE so two beats can't double-fire. The canonical "one hour before kickoff" case is `EnqueueOptions::eta`/`delay`, not beat.
- **Dead-letter — DONE in substance.** A terminal `failed` status (`:106`), queryable and filterable, with a `retry_task` (`:1682`) path. No separate DLQ table; `status='failed'` + the admin retry action is a de-facto dead-letter queue.
- **Admin observability — DONE.** `umbral_tasks::admin_model()` (`:1725`) — read-only queue browser, `status`/`priority` filters, **"Retry selected"** bulk action.
- **Worker/beat commands — DONE** (`tasks-worker` `:273`, `tasks-beat` `:309`, both with `--once`); **the deploy story was NOT** — "worker" appeared **zero** times in the deployment docs and compose. That was the actual gap kikosi named ("a worker service that ships in the compose template") and it is now fixed: `plugins/tasks.mdx` gained a *Running it in production* section with the compose services (same image, wait for one-shot `migrate`, scale workers freely, run exactly one beat, roll web+worker together or you get `HandlerNotFound`).

**Still open (logged as gaps3 #48, #49):** enqueue is **name-keyed, not type-keyed** — `enqueue("send_welcome", payload, ..)`, so a typo or rename is a silent runtime `HandlerNotFound` rather than a compile error (the literal ask, `enqueue(MyJob{..})`, does not exist); and `PeriodicTask` has no admin model, so schedules and `next_run` aren't visible.

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

**VERIFIED (2026-07-12) — the pipeline exists; uploads work. Closing.** Confirmed against the code (and by the consumer: avatar/media upload is live):

- **Multipart — DONE.** `parse_multipart()` / `parse_multipart_capped()` / `parse_and_store_multipart()` (`crates/umbral-core/src/web/multipart.rs:221,244,354`), 32 MiB default cap. A parser + helper, not an extractor — you write the handler.
- **Model field types — DONE.** `FileField` / `ImageField` (`crates/umbral-core/src/orm/file_field.rs:57`), `url()` resolving through the ambient `Storage`, and a **boot system-check** that fails if a model declares a file field with no storage registered (`plugin.rs:238`).
- **Size validation — DONE.** 25 MiB default, `.max_size()` (`umbral-storage/src/lib.rs:344`), enforced at the storage decorator (`SizeLimitedStorage`, `media.rs:606`) with a mid-stream cap — so *every* save path inherits it.
- **Private delivery — DONE, two ways.** S3 presigned GET (`s3.rs:260`, `UMBRAL_S3_PRESIGN_TTL`) and `StoragePlugin::media_access()` (`lib.rs:170`) — an async gate that runs before any byte is served, 403 on false.
- **Processing lifecycle — DONE (the hook, not the images).** `Processor` (`media.rs:43`), `StoragePlugin::on_upload()` (`lib.rs:204`), `processing`/`ready`/`failed` states, `save_deferred`, and a concurrency cap (`DEFAULT_MEDIA_PROCESSING_CONCURRENCY = 8`, `media.rs:66` — *that* is the "media-processing cap" from audit_2; it bounds concurrent processors, it is not an image pipeline).
- **Docs — DONE.** `plugins/storage.mdx`, `orm/file-image-fields.mdx`, honest about the gap below.

**Still open (logged as gaps3 #50, #51, #52):** no built-in **image processing** (no imaging crate is even a dependency — the `on_upload` hook is there, you supply the pipeline); no **content-type/extension policy** (the size cap is enforced everywhere and active content is defanged, but nothing lets you say "this `ImageField` accepts only `image/*`", so a 20 MB `.exe` into an avatar field is stopped only by size); no **direct-to-S3 presigned upload** (`presign_get` exists, `presign_put` does not — every byte transits the Rust process).

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

**VERIFIED (2026-07-12) — soft delete is DONE end to end; audit is NOT.** The two halves of this item turned out to be in completely different states:

- **Soft delete — DONE, across all three surfaces.** `#[umbral(soft_delete)]` → `Model::SOFT_DELETE` (`orm/model.rs:298`), auto `WHERE deleted_at IS NULL` (`queryset/mod.rs:546`), `with_deleted()` / `only_deleted()` / `hard_delete()`, `delete()` rewritten to an UPDATE (`:2936`), `restore()` (`dynamic.rs:806`). **Admin** gets a full trash UI for free — trash view + auto-injected *Restore selected* and *Delete permanently* actions, injected only for soft-delete models (`umbral-admin/src/config.rs:269,314,382`), zero per-model config. **REST** inherits it through `DynQuerySet` (list excludes trashed, DELETE soft-deletes). Docs: `orm/soft-delete.mdx`.
- **Audit trail — genuinely MISSING.** There is no `#[umbral(audited)]`; grep for it in the macros returns nothing. **Do not mistake `AdminAuditLog` for it** (`umbral-admin/src/models.rs:357`): that is Django's `LogEntry` — it records only writes made *through the admin UI*, its `diff_summary` is free-text prose rather than a field-level before/after, and a write from REST, a task, or `Model::objects().save()` produces **no row at all**.

**Still open (logged as gaps3 #53, #54, #55), ranked:**

1. **Soft-delete cascade** — the one real correctness footgun. `on_delete = "cascade"` is a **DDL FK clause**: it fires on a real SQL `DELETE`, and a soft delete is an `UPDATE`, so children are **never touched**. Reads that traverse *from* a soft-deleted parent do hide them (`hydration.rs:741,869`), but the children remain live rows in their own right. Kikosi explicitly asked for "cascade-aware".
2. **Model-level audit (`#[umbral(audited)]`)** — the retrofit-hostile one; it only gets more expensive to add.
3. **`created_by` / `updated_by` auto-stamping** — zero code today; the ORM write path knows nothing of a request user. Kikosi hand-rolls it, and so will every app. Small and self-contained.
4. **Per-user data export / retention** — only whole-DB `dumpdata` exists. Lowest priority.

---

## Suggested sequencing

**#1 → #2 first.** The typed-client / SPA-integration story is the biggest,
most-universal tax and unlocks everything client-side. **#3 (tenancy)** should land
before any consumer commits to a single-tenant schema it can't walk back. **#4/#5**
(jobs + ops) are the "make the happy path production-grade" pair. **#6/#7** are
valuable but app-triggered — build when a consumer actually needs them.
