# umbral maturity matrix

Status: published reference (planning/gaps5.md #100, tf #313).
Version: 0.0.11 (pre-1.0; all `umbral-*` crates share one lockstep version).
Last reviewed: 2026-08-08.

This matrix states, honestly, how far along each umbral subsystem is. umbral is pre-1.0 and its APIs still move (see the README and `STABILITY.md`), so no subsystem here is being sold as "enterprise-hardened, done". The levels below are *relative to the framework's own maturity*: they tell an adopter which pieces are safe to build on today, which work but have known operational gaps you may hit, and which are early and likely to change. Each note is grounded in `FEATURES.md`, the security audits under `planning/review/`, and the per-plugin design docs, not in aspiration.

## Legend

| Level | Meaning |
|---|---|
| **Production** | API is settled and the surface is well tested; secure-by-default where security applies; the known deferrals are enhancements, not correctness or safety gaps. Safe to build on now, expecting only additive change before 1.0. |
| **Beta** | Works and is exercised by tests and example apps; the core API is mostly settled but may still shift in a minor release. Has known operational or feature gaps (documented below) you can hit in production. Build on it, but read the note. |
| **Experimental** | Usable and real, but early: behavior or API is likely to change, delivery is best-effort, or there are notable operational caveats. Prototype with it; do not lean on it for anything load-bearing yet. |

Levels track *this framework's* trajectory to 1.0. A "Production" row is not a claim of parity with a decade-old framework's equivalent; it means the piece is stable and trustworthy within umbral today.

## Matrix

| Subsystem | Crate | Level | Note |
|---|---|---|---|
| ORM | umbral-core | **Production** | Mature and the most-tested surface: typed QuerySet, relations (FK/O2O/M2M/reverse), select_related/prefetch batching with no N+1, aggregation, F/Q expressions, upsert, bulk ops, transactions, dual SQLite+Postgres backends, non-i64 PKs. Audited clean for SQL injection. Deferrals (reverse-relation filtering, EXISTS/correlated subqueries, some Case/When) are additive. |
| Migrations | umbral-core | **Production** | The declare, migrate, change, migrate loop is the product and is heavily tested on both backends: autodetection, snapshotting, tracking table, index/unique_together detection, inspectdb (SQLite + Postgres). Known gaps: no `migrate <target>` / rollback command and rename detection is drop+add (both tracked, gaps5 #26/#8). Forward migration is solid; reverse is not there yet. |
| Auth | umbral-auth | **Beta** | Core is strong and audited: argon2 with per-password salt and constant-time verify, no user enumeration, session-fixation defended, hashed tokens at rest, password reset + email verification + change-password, throttling, custom/UUID user models. Deferred: MFA/passkeys, magic-link, SSO (SAML/generic OIDC), device inventory. Solid for password auth; not yet an enterprise identity stack. |
| Sessions | umbral-sessions | **Production** | DB-backed and Redis-backed stores, secure-by-default cookie flags (HttpOnly, Secure under prod, SameSite), session id rotation on login, sliding expiry + max-age, per-user revocation, boot-time store-routing checks. Well covered by tests; API is settled. |
| Permissions | umbral-permissions | **Beta** | Default-closed (REST and admin fail closed on error), middleware + route guards, works with non-i64 user PKs. Gap: no higher-level policy/ABAC engine, named policies, or dry-run simulation (gaps5 #16). Correct for role/permission checks; not yet a full authorization DSL. |
| RLS | umbral-rls | **Beta** | Postgres row-level-security policies declared in the builder and applied idempotently at boot; SQLite path warns and skips by design. The `app.user_id`-style context variable is now wired (via `AuthPlugin::with_db_session_var`), closing the earlier "policies never enforced" audit finding. Caveat: policy bodies are raw SQL strings, so typed policy builders and lint/simulation are still to come (gaps5 #17). Postgres-only. |
| Tenants | umbral-tenants | **Beta** | Multi-tenancy scoping is in place and used by real consumers. Not yet a full platform tenancy product (per-tenant quotas, region pinning, and residency are tracked separately, gaps5 #47/#85). Fine for app-level tenant isolation. |
| Admin | umbral-admin | **Beta** | Deep auto-CRUD: list/detail/create/edit/delete, per-type widgets, dashboard, custom widget/view pages with full permission enforcement. Earlier reflected-XSS-in-filter and superuser-field-guard findings are fixed. Not yet an org/ops console (identity-provider config UI, cross-plugin audit explorer, impersonation-with-approval are deferred, gaps5 #75). |
| REST | umbral-rest | **Beta** | Auto-generated JSON CRUD with per-type dispatch. Now safe-by-default: writes 403 without an explicit permission opt-in, `hide()` blocks writes, list is capped (the WEB-1/WEB-2/PERF-1 audit findings are closed). Gaps: OpenAPI does not yet cover handwritten routes and there is no breaking-change checker (gaps5 #35/#36). Solid for model-backed APIs. |
| OpenAPI | umbral-openapi | **Beta** | Auto-generated OpenAPI 3.0 spec + embedded Swagger UI, plus client generation, walking the same model registry as REST. Documented limit: v1 describes only the REST auto-generated endpoints, not handwritten builder routes (gaps5 #35). Accurate for the auto-CRUD surface. |
| GraphQL | umbral-graphql | **Beta** | Model-backed GraphQL surface honoring column privacy (`private`/secret) and the shared permission model. Works and is tested; the API is younger than REST's and may still move. No subscriptions/federation story yet. |
| OAuth | umbral-oauth | **Beta** | Social login with PKCE and state-CSRF protection, generic OIDC discovery, token-mode, and Google/GitHub built-ins. Audited for the state/PKCE flows. Enterprise SSO (SAML, broader provider metadata, SCIM) is explicitly deferred (gaps5 #9/#11). Good for consumer social login. |
| Storage | umbral-storage | **Beta** | Local + S3 backends, signed media URLs, per-owner media gating, path-traversal / symlink-escape defended (audited), upload caps, active-content guard. Gaps: direct-to-object-storage uploads (presigned POST/multipart) still largely proxy through the app, and there is no malware scanning/quarantine (gaps5 #58/#59). Fine for app-managed uploads. |
| Realtime | umbral-realtime | **Experimental** | WebSocket pub/sub with channel publish authorization and an identity resolver. By design it uses bounded buffers and best-effort delivery: slow consumers drop events, there is no durable/acknowledged channel, and no operational metrics yet (gaps5 #43/#48). Good for live UI updates; not a delivery-guaranteed event bus. |
| Cache | umbral-cache | **Beta** | `Cache` over a backend trait with in-memory, SQLite, and Redis backends, typed get/set with serde, TTL with lazy expiry, `cache_page` middleware (now fails loud instead of fabricating a 200). Known gap: the memory backend only evicts on read, so it can grow unbounded under long-tail keys (BROKEN-10, deferred). Solid with the Redis or SQLite backend. |
| Email | umbral-email | **Beta** | SMTP over STARTTLS with real cert verification (no TLS bypass, audited), a loud console backend for dev, and Resend/SendGrid API senders; header-injection defended. No durable retry queue, no CC/BCC, no S/MIME/DKIM, narrow provider set (gaps5 #53/#54). Reliable for direct sends; pair with tasks for retry. |
| Tasks | umbral-tasks | **Beta** | DB-backed queue (Celery-shaped) fully through the ORM: enqueue, handler registry, worker + periodic beat, retries, panic recovery, cooperative shutdown. The two worst audit findings are fixed: Postgres double-claim (now `FOR UPDATE SKIP LOCKED`) and crash-loss (now reclaimed via visibility timeout). Gaps: no external broker, no dead-letter queue, no Horizon-style dashboard, retry has no backoff yet (gaps5 #49/#50/#51). A strong single-binary default. |
| Health | umbral-health | **Beta** | Liveness/readiness endpoints with a dialect-neutral DB connectivity probe. Gap: no deep dependency registry yet (Redis, S3, email, workers, broker) or readiness/liveness profiles (gaps5 #73). Good enough for a load-balancer health check today. |
| Logs | umbral-logs | **Beta** | Request logging plus the framework's single observability entry point (tracing subscriber, JSON mode, optional OTLP trace export). Caveat: request logs are written to the app DB, which is a load/retention risk at scale, and there are no external log drains yet (gaps5 #68). The observability init path is solid; the DB-sink default needs care in production. |
| Analytics | umbral-analytics | **Experimental** | PostHog event capture via a shared client, fire-and-forget. Single destination, best-effort delivery (no retry/outbox), no consent hooks or warehouse export yet (gaps5 #69). Fine for product telemetry you can afford to lose; not for billing-grade events. |
| Security | umbral-security | **Production** | The secure-by-default posture verified 2026-08-08: CSRF (signed / session-bound double-submit), security headers (X-Content-Type-Options, X-Frame-Options, Referrer-Policy), private-cache for authenticated responses, empty-secret-key boot refusal, and a system check that warns when auth/sessions mount without it. HSTS and CSP are intentionally opt-in (turned on by the enterprise preset). This is a core safety layer and is default-on. |
| Signals | umbral-signals | **Beta** | In-process pub/sub (sync + async subscribers, JSON payloads) with lifecycle signals from ORM writes. The mutex-poisoning-on-panic and silent-deserialize-drop audit findings are fixed (poison recovery + catch_unwind + logged failures). Explicitly in-process only: no distributed bus, no replay (gaps5 #57). Correct and safe locally; use tasks when work must survive the process. |
| Playground | umbral-playground | **Experimental** | An interactive API playground for exploring the surface in development. Flagged in the audit for unauthenticated mounting that runs requests with the visitor's ambient cookies (WEB-6); treat it as a dev-only tool and gate it in production. Useful for local exploration, not a production surface. |
| Livereload | umbral-livereload | **Beta** | Development-time live reload (browser refresh on rebuild) wired through the `dev` command. Does what it says and is stable for its purpose, but it is a developer-experience tool, never mounted in production. Scoped and reliable within that scope. |

## How to read a level for your case

- Building an internal app on Postgres today: the **Production** and **Beta** rows are a complete stack. Read the Tasks, Email, and RLS notes if you rely on those.
- Standing up a public write API: REST is safe-by-default now, but wire explicit permissions and read the OpenAPI-coverage note.
- Anything delivery-critical (payments, notifications you cannot lose): do not lean on **Experimental** realtime or analytics for guaranteed delivery; route through Tasks (with its documented at-least-once semantics after the reclaim fix) or a durable outbox (tracked, gaps5 #52).
- Enterprise identity (SSO, MFA, SCIM): not here yet. Auth is **Beta** for password + social login; the enterprise identity items are tracked in gaps5 #9 through #14.

## Maintenance

This matrix is reviewed each release. A subsystem moves up a level when its named gaps close and its tests + secure-by-default posture hold across a release; it moves down if an audit reopens a correctness or safety finding. Level changes are noted in the changelog. The always-on security regression suite (gaps5 #98) and the release provenance pipeline (gaps5 #99) are what let a level claim stay honest between reviews.

## See also

- `FEATURES.md` (shipped features and their documented deferrals).
- `STABILITY.md` (API tiers, versioning, deprecation windows, the 1.0 gates).
- `SECURITY.md` (secure-by-default posture and supply-chain roadmap).
- `planning/review/` (the security and feature audits grounding these notes).
- `planning/gaps5.md` (the numbered gap backlog referenced throughout).
- `docs/decisions/2026-08-08-security-suite-and-release-automation.md` (the suite and pipeline that keep these claims honest).
