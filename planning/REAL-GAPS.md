# Real Gaps — Developer vs. Organization Feature Expectations

This file captures the two different audiences that evaluate a web framework.

- **Part A** is what individual developers, indie hackers, and bootstrapped teams reach for first. These are the "ship fast" features.
- **Part B** is what engineering managers, security teams, and platform leads require before adopting a framework at scale. These are the "run safe" features.

Anything in **Part B** that Umbra does not yet cover is a strategic gap — it blocks enterprise pilots and contracting conversations.

---

## Part A — Features Developers Usually Ask For

> "Can I build a working MVP in a weekend?"

| # | Feature | Why it matters | Umbra status |
|---|---------|----------------|--------------|
| 1 | **Auto-generated Admin UI** | Visual CRUD without writing HTML/JS. Django's killer feature. | ✅ `umbra-admin` — list, create, edit, delete, search, filters |
| 2 | **Built-in Auth** | Users, login, logout, password reset, session management out of the box. | ✅ `umbra-auth` + `umbra-sessions` |
| 3 | **REST API Auto-generation** | Expose models as JSON endpoints with pagination, filtering, sorting with zero config. | ✅ `umbra-rest` — list, retrieve, create, update, delete |
| 4 | **Database Migrations** | `makemigrations` + `migrate` that diffs models and generates safe SQL. | ✅ Phase 1 complete — autodetect, apply, track |
| 5 | **CLI Scaffolding** | `startproject`, `startapp`, `createsuperuser` — reduces boilerplate to near zero. | ✅ `umbra-cli` ships all three |
| 6 | **Hot Reload / Dev Server** | Edit a template or handler, see the change immediately without recompile. | ⚠️ Templates hot-reload; Rust source needs `cargo-watch` (Already done with `cargo run -- dev`) |
| 7 | **Simple Deployment** | Single binary + static files. No Docker required for side projects. | ✅ Cargo binary + `templates/` dir |
| 8 | **Background Tasks / Job Queue** | Schedule emails, image processing, reports without blocking the web server. | ⚠️ `umbra-tasks` planned (M9) (There is some implementation of it, an improvement to use apalis) |
| 9 | **Email Sending** | Password resets, notifications, transactional emails with SMTP or API backends. | ❌ Not yet implemented (There is AuthPlugin, needs proper extension to make it reusable and easily update some things like the email templates, the sending task, etc) |
| 10 | **File Uploads & Media Storage** | Handle multipart uploads, store locally or on S3/R2, serve with signed URLs. | ✅ `FileField`/`ImageField` + multipart parsing + pluggable `Storage` (`FsStorage` in umbra-media); admin upload widgets. S3 backend + signed URLs deferred |
| 11 | **OpenAPI / Swagger Auto-docs** | Every REST endpoint documented with schemas, ready for frontend code generation. | ✅ `umbra-openapi` generates spec |
| 12 | **Social Auth / OAuth** | "Sign in with GitHub/Google" — table stakes for modern SaaS. | ✅ `umbra-oauth` — Google/GitHub login + account connection, SPA token return, encrypted tokens (`Masked<T>`) |
| 13 | **Full-text Search** | Typeahead, fuzzy matching, ranking — not just exact `LIKE` queries. | ⚠️ Postgres `tsvector` FTS shipped (`TsVector` field, `.matches()`/`.matches_websearch()`, boot-time SQLite gating); auto-GIN-index + SQLite FTS5 fallback deferred (features.md #33) |
| 14 | **WebSockets / Real-time** | Chat, notifications, live dashboards without polling. | ✅ `umbra-realtime` — SSE + WebSocket, user/group-targeted, GroupPolicy gate, signals bridge (`on_model`). Multi-instance Redis broker deferred |
| 15 | **Caching Layer** | Redis-backed cache for expensive queries, view fragments, or session stores. | ❌ Not yet implemented |
| 16 | **Form Validation & Error Messages** | Declarative validation (min/max, regex, custom rules) with user-friendly errors. | ⚠️ Basic validation exists; rich error formatting missing |
| 17 | **i18n / Internationalization** | Translate templates, model labels, and error messages for multiple locales. | ❌ Not yet implemented |
| 18 | **Testing Utilities** | Test client that boots the app in-memory, DB rollback per test, factory fixtures. | ✅ `umbra-testing` — `TestClient`/`TempPool`/`TestResponse` + `Factory` trait (build/create/create_with/create_batch, `seq()`, `fake`) + `fixtures` (load/dump) |
| 19 | **Dashboard / Analytics Widgets** | Built-in charts, counts, recent-activity panels in the admin. | ❌ Not yet implemented |
| 20 | **Plugin Ecosystem** | Third-party plugins on crates.io that extend the framework (payment, CRM, blog). | ⚠️ Plugin trait exists; ecosystem is empty |

---

## Part B — Features Organizations Look For

> "Can we pass a security audit and scale to 10k users?"

| # | Feature | Why it matters | Umbra status |
|---|---------|----------------|--------------|
| 1 | **RBAC / Fine-grained Permissions** | Not just "is_staff" — role-based access with per-object permissions (`can_edit_own_post`). | ✅ `umbra-permissions` — groups, permissions, M2M checks |
| 2 | **Audit Logging** | Every create, update, delete recorded with who, when, and what changed. | ⚠️ `AdminAuditLog` model exists; no queryable audit trail API |
| 3 | **SSO / OIDC / SAML** | Enterprise customers demand "Sign in with Okta/Azure AD/Google Workspace." | ❌ Not yet implemented |
| 4 | **Multi-tenancy** | One app serving isolated customers (schemas or row-level) with zero data leakage. | ❌ Not yet implemented (We need a good database router) |
| 5 | **Read Replicas & Connection Pooling** | Route reads to replicas, writes to primary. Transparent failover. | ⚠️ `sqlx` pool handles basics; no replica routing (We need a good database router) |
| 6 | **Horizontal Scaling (Stateless)** | Run 10 app instances behind a load balancer with shared session store. | ⚠️ Sessions use DB; Redis-backed sessions needed for scale |
| 7 | **Health Checks & Readiness Probes** | `/healthz`, `/ready` endpoints for Kubernetes and load balancers. | ✅ `umbra-health` — `/healthz` (liveness) + `/ready` (DB probe + registered `HealthCheck`s, 503 + per-dep JSON on failure) |
| 8 | **Structured Logging / Observability** | JSON logs, distributed tracing (OpenTelemetry), request correlation IDs. | ❌ Not yet implemented |
| 9 | **Metrics & Monitoring** | Prometheus-compatible counters for requests, DB latency, queue depth. | ❌ Not yet implemented |
| 10 | **Rate Limiting & Throttling** | Per-IP, per-user, per-endpoint limits. Essential for public APIs. | ❌ Not yet implemented |
| 11 | **Input Sanitization & WAF** | XSS prevention, SQL injection guards, CSRF tokens, clickjacking headers. | ✅ CSRF + auto-escaping + parameterized SQL; no WAF layer |
| 12 | **GDPR / Data Retention / Privacy** | Right to erasure, data export, retention policies, PII masking. | ❌ Not yet implemented |
| 13 | **API Versioning** | `/v1/`, `/v2/` route prefixes with backward-compatible schemas. | ❌ Not yet implemented |
| 14 | **Backup & Disaster Recovery** | Point-in-time restore, automated dumps, cross-region replication. | ⚠️ `dumpdata`/`loaddata` exist; not enterprise-grade |
| 15 | **Blue-Green Deployments** | Zero-downtime schema migrations with rollback capability. | ❌ Not yet implemented |
| 16 | **Feature Flags** | Toggle features per tenant, per user segment, or percentage rollout. | ❌ Not yet implemented |
| 17 | **gRPC / GraphQL Support** | Internal microservices often prefer gRPC; frontend teams want GraphQL. | ❌ Not yet implemented |
| 18 | **Event Sourcing / CQRS** | Audit-grade change logs, read-model separation, replayable events. | ❌ Not yet implemented |
| 19 | **Row-Level Security (RLS)** | Postgres RLS policies enforced at the DB level for multi-tenant isolation. | ❌ Not yet implemented (There is an implementation of RLS but it needs testing) |
| 20 | **Data Masking / PII Redaction** | Hide sensitive fields in logs, admin, and API responses based on role. | ⚠️ `Masked<T>` field encryption at rest (X25519 sealed boxes, GDPR crypto-shredding) shipped; role-based redaction in admin/API responses not yet |
| 21 | **Service Mesh Integration** | mTLS, sidecar proxies, circuit breakers for inter-service calls. | ❌ Not yet implemented |
| 22 | **Change Data Capture (CDC)** | Stream DB changes to Kafka/message bus for downstream analytics. | ❌ Not yet implemented |
| 23 | **Performance Profiling Tools** | Built-in flamegraph endpoint, slow-query logging, memory usage dashboards. | ❌ Not yet implemented |
| 24 | **Compliance Certifications** | SOC 2, ISO 27001, HIPAA — framework defaults that make audits easier. | ❌ Not yet implemented |
| 25 | **Vendor Lock-in Avoidance** | Clean abstraction over DB, cache, and queue so migration is possible. | ✅ Backend abstraction + plugin contract |

---

## Strategic Takeaway

Umbra now covers roughly **80% of Part A** (added OAuth, file uploads/media, testing utilities + factories; FTS partial) and **~25% of Part B** (added health checks; data-masking via `Masked<T>` and RLS partial). Updated 2026-06-13.

The gap pattern is clear:
- **Developer features** that Django already solved are being rebuilt well (admin, auth, REST, migrations, CLI).
- **Organization features** are almost entirely untouched (SSO, multi-tenancy, observability, rate limiting, compliance).

The next high-value milestone is not adding more ORM sugar. It is picking 3–4 organization-grade features (SSO, health checks, rate limiting, structured logging) and shipping them as first-class plugins. That changes the conversation from "this is a cool side-project framework" to "we can pilot this internally."
