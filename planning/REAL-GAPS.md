# Real Gaps — Developer vs. Organization Feature Expectations

This file captures the two different audiences that evaluate a web framework.

- **Part A** is what individual developers, indie hackers, and bootstrapped teams reach for first. These are the "ship fast" features.
- **Part B** is what engineering managers, security teams, and platform leads require before adopting a framework at scale. These are the "run safe" features.

Anything in **Part B** that Umbral does not yet cover is a strategic gap — it blocks enterprise pilots and contracting conversations.

---

## Part A — Features Developers Usually Ask For

> "Can I build a working MVP in a weekend?"

| # | Feature | Why it matters | Umbral status |
|---|---------|----------------|--------------|
| 1 | **Auto-generated Admin UI** | Visual CRUD without writing HTML/JS. A batteries-included killer feature. | ✅ `umbral-admin` - list, create, edit, delete, search, filters |
| 2 | **Built-in Auth** | Users, login, logout, password reset, session management out of the box. | ✅ `umbral-auth` + `umbral-sessions` |
| 3 | **REST API Auto-generation** | Expose models as JSON endpoints with pagination, filtering, sorting with zero config. | ✅ `umbral-rest` — list, retrieve, create, update, delete |
| 4 | **Database Migrations** | `makemigrations` + `migrate` that diffs models and generates safe SQL. | ✅ Phase 1 complete — autodetect, apply, track |
| 5 | **CLI Scaffolding** | `startproject`, `startapp`, `createsuperuser` — reduces boilerplate to near zero. | ✅ `umbral-cli` ships all three |
| 6 | **Hot Reload / Dev Server** | Edit a template or handler, see the change immediately without recompile. | ✅ `umbral dev` (cargo-watch) + in-process template hot-reload + `umbral-livereload` — browser auto-reloads over SSE on template/CSS edits (CSS hot-swaps in place) and reconnect-reloads after a `.rs` rebuild. No manual refresh. |
| 7 | **Simple Deployment** | Single binary + static files. No Docker required for side projects. | ✅ Cargo binary + `templates/` dir |
| 8 | **Background Tasks / Job Queue** | Schedule emails, image processing, reports without blocking the web server. | ✅ `umbral-tasks` — DB-backed queue: enqueue, worker loop, `#[task]` macro, retries (`attempts`/`max_attempts`), scheduled execution (`scheduled_for`). apalis migration is a future improvement |
| 9 | **Email Sending** | Password resets, notifications, transactional emails with SMTP or API backends. | ✅ `umbral-email` — SMTP via `lettre` (STARTTLS 587) + console backend, HTML/multipart messages. API backends (SendGrid/SES) deferred |
| 10 | **File Uploads & Media Storage** | Handle multipart uploads, store locally or on S3/R2, serve with signed URLs. | ✅ `FileField`/`ImageField` + multipart parsing + pluggable `Storage` (`FsStorage` in umbral-media); admin upload widgets. S3 backend + signed URLs deferred |
| 11 | **OpenAPI / Swagger Auto-docs** | Every REST endpoint documented with schemas, ready for frontend code generation. | ✅ `umbral-openapi` generates spec |
| 12 | **Social Auth / OAuth** | "Sign in with GitHub/Google" — table stakes for modern SaaS. | ✅ `umbral-oauth` — Google/GitHub login + account connection, SPA token return, encrypted tokens (`Masked<T>`) |
| 13 | **Full-text Search** | Typeahead, fuzzy matching, ranking — not just exact `LIKE` queries. | ⚠️ Postgres `tsvector` FTS shipped (`TsVector` field, `.matches()`/`.matches_websearch()`, boot-time SQLite gating); auto-GIN-index + SQLite FTS5 fallback deferred (features.md #33) |
| 14 | **WebSockets / Real-time** | Chat, notifications, live dashboards without polling. | ✅ `umbral-realtime` — SSE + WebSocket, user/group-targeted, GroupPolicy gate, signals bridge (`on_model`). Multi-instance Redis broker deferred |
| 15 | **Caching Layer** | Redis-backed cache for expensive queries, view fragments, or session stores. | ✅ `umbral-cache` — in-memory / SQLite / Redis backends + `cache_page` view middleware + ambient `Cache` handle |
| 16 | **Form Validation & Error Messages** | Declarative validation (min/max, regex, custom rules) with user-friendly errors. | ✅ `#[derive(Form)]` validators (required/email/phone/url/regex/min/max/length/message) + `FormErrors` with per-field & non-field messages and template context |
| 17 | **i18n / Internationalization** | Translate templates, model labels, and error messages for multiple locales. | ❌ Not yet implemented |
| 18 | **Testing Utilities** | Test client that boots the app in-memory, DB rollback per test, factory fixtures. | ✅ `umbral-testing` — `TestClient`/`TempPool`/`TestResponse` + `Factory` trait (build/create/create_with/create_batch, `seq()`, `fake`) + `fixtures` (load/dump) |
| 19 | **Dashboard / Analytics Widgets** | Built-in charts, counts, recent-activity panels in the admin. | ✅ `umbral-admin` dashboard — per-model row-count cards + user-customizable widget grid |
| 20 | **Plugin Ecosystem** | Third-party plugins on crates.io that extend the framework (payment, CRM, blog). | ⚠️ Plugin trait exists; ecosystem is empty |

---

## Part B — Features Organizations Look For

> "Can we pass a security audit and scale to 10k users?"

| # | Feature | Why it matters | Umbral status |
|---|---------|----------------|--------------|
| 1 | **RBAC / Fine-grained Permissions** | Not just "is_staff" — role-based access with per-object permissions (`can_edit_own_post`). | ✅ `umbral-permissions` — groups, permissions, M2M checks |
| 2 | **Audit Logging** | Every create, update, delete recorded with who, when, and what changed. | ⚠️ `AdminAuditLog` + admin timeline view record admin-panel actions; no general model-level queryable audit trail API (`umbral-signals` is the seam to build one) |
| 3 | **SSO / OIDC / SAML** | Enterprise customers demand "Sign in with Okta/Azure AD/Google Workspace." | ⚠️ Extensible `OAuthProvider` trait + social login (Google/GitHub) via `umbral-oauth`; no generic OIDC discovery, Okta/Azure AD, or SAML |
| 4 | **Multi-tenancy** | One app serving isolated customers (schemas or row-level) with zero data leakage. | ✅ `umbral-tenants` — schema-per-tenant (Postgres): `Host` subdomain/header resolution, `TenantRouter` schema-qualifies tenant tables with zero extra round-trips, `create_tenant` provisioning + `migrate_schemas` CLI, SHARED_APPS for cross-tenant tables. `TenantStrategy::Database` also does database-per-tenant. Built on the `DatabaseRouter` foundation |
| 5 | **Read Replicas & Connection Pooling** | Route reads to replicas, writes to primary. Transparent failover. | ⚠️ Pooling ✅ (`sqlx` pools + multi-pool registration via `App::builder().database(alias, pool)`); the read/write routing **seam** ships — swappable `DatabaseRouter` with `db_for_read`/`db_for_write` wired through every ORM terminal. Remaining: a turnkey "reads→replica" router policy (write a small router today; `TenantStrategy::Database` is the worked example) and transparent failover |
| 6 | **Horizontal Scaling (Stateless)** | Run 10 app instances behind a load balancer with shared session store. | ✅ `RedisStore` (Redis-backed sessions, feature-gated) + `umbral-cache` Redis backend → stateless app behind a LB. Caveat: cross-instance realtime fan-out still needs the deferred Redis broker (Part A #14) |
| 7 | **Health Checks & Readiness Probes** | `/healthz`, `/ready` endpoints for Kubernetes and load balancers. | ✅ `umbral-health` — `/healthz` (liveness) + `/ready` (DB probe + registered `HealthCheck`s, 503 + per-dep JSON on failure) |
| 8 | **Structured Logging / Observability** | JSON logs, distributed tracing (OpenTelemetry), request correlation IDs. | ✅ `umbral-logs` — DB request log (admin-browsable, fire-and-forget) + structured logging init and, under the `otel` feature, OpenTelemetry OTLP trace export (span-per-request, `trace_id`/`span_id`, Jaeger/Tempo) |
| 9 | **Metrics & Monitoring** | Prometheus-compatible counters for requests, DB latency, queue depth. | ⚠️ OpenTelemetry **traces** via `umbral-logs` (`otel` feature); no Prometheus counters / `/metrics` exporter yet |
| 10 | **Rate Limiting & Throttling** | Per-IP, per-user, per-endpoint limits. Essential for public APIs. | ✅ Core sliding-window limiter (`umbral-core::ratelimit`) backing `umbral-rest` per-resource/plugin-wide API throttles AND `umbral-auth` login/register brute-force throttle |
| 11 | **Input Sanitization & WAF** | XSS prevention, SQL injection guards, CSRF tokens, clickjacking headers. | ✅ CSRF + auto-escaping + parameterized SQL; no WAF layer |
| 12 | **GDPR / Data Retention / Privacy** | Right to erasure, data export, retention policies, PII masking. | ⚠️ `Masked<T>` field encryption gives crypto-shredding (drop the key → erasure) + `dumpdata`/`loaddata` export; no automated retention policies or consent tracking |
| 13 | **API Versioning** | `/v1/`, `/v2/` route prefixes with backward-compatible schemas. | ✅ `umbral-rest` opt-in versioning — two schemes (URL path segment + header/accept), `RequestContext::version`, allow-list of accepted versions |
| 14 | **Backup & Disaster Recovery** | Point-in-time restore, automated dumps, cross-region replication. | ⚠️ `dumpdata`/`loaddata` exist; not enterprise-grade |
| 15 | **Blue-Green Deployments** | Zero-downtime schema migrations with rollback capability. | ❌ Not yet implemented |
| 16 | **Feature Flags** | Toggle features per tenant, per user segment, or percentage rollout. | ❌ Not yet implemented |
| 17 | **gRPC / GraphQL Support** | Internal microservices often prefer gRPC; frontend teams want GraphQL. | ⚠️ `umbral-graphql` — a real GraphQL API derived from models (queries, mutations with `owned_by` row-scope, subscriptions over SSE/WebSocket, batched loaders); gRPC not implemented |
| 18 | **Event Sourcing / CQRS** | Audit-grade change logs, read-model separation, replayable events. | ❌ Not yet implemented |
| 19 | **Row-Level Security (RLS)** | Postgres RLS policies enforced at the DB level for multi-tenant isolation. | ✅ `umbral-rls` — `ENABLE ROW LEVEL SECURITY` + per-table/action `CREATE POLICY`, and the security-context piece now ships: `AuthPlugin::with_db_session_var("app.user_id")` runs `set_config` on every pool acquire so `current_setting('app.user_id')` policies actually isolate (gaps3 #45) |
| 20 | **Data Masking / PII Redaction** | Hide sensitive fields in logs, admin, and API responses based on role. | ⚠️ `Masked<T>` field encryption at rest (X25519 sealed boxes, GDPR crypto-shredding) shipped; role-based redaction in admin/API responses not yet |
| 21 | **Service Mesh Integration** | mTLS, sidecar proxies, circuit breakers for inter-service calls. | ❌ Not yet implemented |
| 22 | **Change Data Capture (CDC)** | Stream DB changes to Kafka/message bus for downstream analytics. | ❌ Not yet implemented |
| 23 | **Performance Profiling Tools** | Built-in flamegraph endpoint, slow-query logging, memory usage dashboards. | ❌ Not yet implemented |
| 24 | **Compliance Certifications** | SOC 2, ISO 27001, HIPAA — framework defaults that make audits easier. | ❌ Not yet implemented |
| 25 | **Vendor Lock-in Avoidance** | Clean abstraction over DB, cache, and queue so migration is possible. | ✅ Backend abstraction + plugin contract |

---

## Strategic Takeaway

Umbral now covers roughly **85% of Part A** — 17/20 ✅ (admin, auth, REST, migrations, CLI, hot-reload, deploy, **tasks**, **email**, media, OpenAPI, OAuth, realtime, **caching**, **form validation**, testing, **dashboard**); only FTS and the plugin ecosystem are partial and i18n is unstarted — and **~40% of Part B fully, with another third partial**: **10/25 ✅** (RBAC, **multi-tenancy**, **horizontal scaling**, health checks, **observability**, **rate limiting**, CSRF/XSS hardening, **API versioning**, **RLS**, vendor-neutral abstractions) plus **8 ⚠️ partials** (audit log, social-only SSO, **read-replica routing seam + pooling**, OTel-traces-but-no-metrics, GDPR crypto-shredding, backup, GraphQL-not-gRPC, role-based masking). Updated 2026-07-15 (was ~30% on 2026-06-14).

The June→July delta is large: multi-tenancy (`umbral-tenants` on the `DatabaseRouter` foundation), structured logging + OTel tracing (`umbral-logs`), rate limiting (`umbral-core::ratelimit`), API versioning (`umbral-rest`), Redis sessions (`RedisStore`), and the RLS security-context middleware all landed as first-class plugins — turning three of the four features the old takeaway named as "next" into shipped ones.

The gap pattern has shifted:
- **Developer features** are largely done and being rebuilt well (admin, auth, REST, migrations, CLI).
- **Organization features** are now half-covered; what remains is the deep-enterprise tier — **generic SSO (OIDC discovery / SAML)**, **Prometheus metrics** (traces exist, counters don't), a **queryable model-level audit trail**, **feature flags**, and a **turnkey read-replica router + failover** (the routing seam is already there). Compliance (SOC 2/HIPAA), CDC, event sourcing, and service-mesh integration stay out of scope for now.

The next high-value milestone is picking 3–4 of those remaining organization-grade features and shipping them as first-class plugins — that keeps the conversation at "we can pilot this internally," now that the table-stakes tier (multi-tenancy, observability, rate limiting, RLS) is in place.
