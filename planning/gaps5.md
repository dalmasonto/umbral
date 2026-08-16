# gaps5 - Framework Completeness and Organization Adoption Sweep

Status: open review draft  
Date: 2026-08-02  
Scope: broad framework/product review. GitNexus and Taskflow MCP were intentionally skipped per request.

Tracking: every numbered item below is mirrored on the TaskFlow board (project 8). The `[tf#NNN]` tag after each number is the TaskFlow task id — the mapping is uniform, **gaps5 #N = tf #(N+213)** (so #1 → tf#214 … #100 → tf#313). Priority on the board: the "Highest-Leverage Sequence" items are `critical`, remaining `[HIGH]` are `high`, `[MEDIUM]` are `normal`. The unnumbered eBPF/Aya note under Observability is not tracked (explicitly out of scope).

This file focuses on gaps that would matter if umbral is trying to compete as:

- a Rust "Laravel/Django" style batteries-included framework;
- a "Supabase/Firebase with stronger backend control" style platform;
- a credible framework for teams, startups, and larger organizations.

This is not a denial of the existing work. The repository already has a serious spine: ORM, migrations, admin, REST, GraphQL, OpenAPI, auth, sessions, tasks, email, storage, realtime, cache, security, logs, tenants, RLS, permissions, playground, and health. The gaps below are the remaining holes in trust, platform completeness, enterprise adoption, and operational maturity.

External baseline links checked while reviewing:

- Django: https://www.djangoproject.com/ and https://docs.djangoproject.com/
- Laravel auth, queues, Horizon, notifications, broadcasting: https://laravel.com/docs/13.x/authentication, https://laravel.com/docs/13.x/queues, https://laravel.com/docs/13.x/horizon, https://laravel.com/docs/13.x/notifications, https://laravel.com/docs/13.x/broadcasting
- Supabase features and docs: https://supabase.com/features and https://supabase.com/docs
- Firebase App Check, Cloud Functions, Remote Config, Realtime Database, Performance Monitoring: https://firebase.google.com/docs/app-check, https://firebase.google.com/docs/functions, https://firebase.google.com/docs/remote-config, https://firebase.google.com/docs/database, https://firebase.google.com/docs/perf-mon

Local evidence anchors used repeatedly:

- `README.md:9` says the framework is early/alpha and APIs will still move before 1.0.
- `README.md:64-69` states the core product promise.
- `crates/umbral-core/src/plugin.rs:144-220` defines the plugin trait and route-registration caveats.
- `crates/umbral-core/src/settings.rs:337-430` shows the current compact first-class settings surface.
- `plugins/umbral-oauth/src/providers/mod.rs:1-10` exposes only Google/GitHub built-ins.
- `docs/superpowers/specs/2026-06-13-masked-and-oauth-design.md:13` explicitly defers SAML, broader OIDC discovery, and token-refresh jobs.
- `docs/decisions/2026-06-28-auth-full-surface.md:165-170` explicitly defers change-password, magic-link login, TOTP/2FA, and durable mail retry.
- `plugins/umbral-tasks/src/lib.rs:22-32` states the task queue has no separate broker or distributed locks.
- `plugins/umbral-email/src/lib.rs:44-58` states no email retry queue, no CC/BCC, no S/MIME/DKIM.
- `plugins/umbral-openapi/src/lib.rs:16-19` states v1 only describes REST auto-generated endpoints, not handwritten builder routes.
- `plugins/umbral-realtime/src/lib.rs:75-84` states realtime uses bounded buffers and best-effort delivery.
- `plugins/umbral-storage/src/lib.rs:51-63` notes S3/private media can still proxy through the app unless presigned URLs are used.
- `documentation/docs/v0.0.1/observability/index.mdx:79-85` explicitly defers DB/task spans and traceparent propagation.
- `documentation/docs/v0.0.1/rest/throttling.mdx:11-15` states throttling is opt-in and in-memory per process.
- `documentation/docs/v0.0.1/deployment/migrations-in-production.mdx:14-83` documents one-shot/boot migrations but not full rollout orchestration.
- `documentation/docs/v0.0.1/migrations/data-migrations.mdx:11-13` states data migrations are hand-authored `RunSql`, not auto-generated.
- `docs/specs/06-migration-engine.md:21-25` is stale against current implementation and still says several shipped operations are deferred.
- `crates/umbral-cli/src/lib.rs:118-155` shows `migrate` has fake/drift/destructive flags but no target rollback command.
- `planning/gaps3.md:406` still tracks the hardcoded `startcommand` reserved-name issue.
- `crates/README.md:63-77` says many Postgres-backed tests are ignored and require an external `UMBRAL_TEST_POSTGRES_URL`.

## Strategic Product and Adoption

1. [tf#214] [HIGH] The product boundary is still ambiguous: framework, backend platform, or managed BaaS.
   Evidence: the repo ships both framework primitives and BaaS-shaped plugins, but there is no explicit "self-hosted framework" vs "managed platform" contract. Supabase/Firebase compete partly because they define projects, environments, credentials, auth, storage, realtime, functions, dashboards, logs, billing, and management APIs as one product. Recommended fix: write a north-star product spec that names the target shape: "framework only", "self-hosted platform", "managed cloud", or a staged path across all three.

2. [tf#215] [HIGH] Alpha status blocks organization adoption unless paired with a stability path.
   Evidence: `README.md:9` says APIs will still move before 1.0. That is honest, but orgs need semver, MSRV, deprecation windows, migration guides, security advisories, and supported upgrade paths. Recommended fix: add a public stability policy: API tiers, deprecation minimums, plugin compatibility rules, MSRV policy, and a 0.x-to-1.0 roadmap.

3. [tf#216] [HIGH] There is no "enterprise-ready preset" that turns on the expected production posture.
   Evidence: security, logs, health, rate limits, OpenAPI gating, auth, sessions, RLS, permissions, and observability exist as separate pieces. Orgs need a single profile that wires the secure defaults together. Recommended fix: add `EnterprisePreset` or scaffold flag that installs security headers, CSRF, host validation, trusted-proxy checks, health readiness, request logs, metrics, throttles, auth, sessions, audit, and production system checks.

4. [tf#217] [HIGH] The deployment reference architecture is not complete enough for procurement or platform teams.
   Evidence: `documentation/docs/v0.0.1/deployment/migrations-in-production.mdx:14-83` covers migration policy, but not an end-to-end architecture for web, worker, beat, Redis, Postgres, S3, OpenTelemetry collector, metrics, backups, CDN, and secrets. Recommended fix: publish supported deployment topologies with Docker Compose, Kubernetes, Fly.io/Render/Railway, and bare-metal examples.

5. [tf#218] [HIGH] There is no managed-project model like Firebase/Supabase.
   Evidence: Umbra is currently app/framework-first. A BaaS competitor needs project creation, environment separation, API keys, database/storage/realtime resources, quotas, logs, and team access. Recommended fix: design an optional `umbral-control-plane` or `umbral-studio` that can manage multiple projects.

6. [tf#219] [MEDIUM] The plugin ecosystem promise has no marketplace, certification, or compatibility contract.
   Evidence: `crates/umbral-core/src/plugin.rs:144-220` gives a strong trait, but not distribution metadata, compatibility ranges, security status, docs URL, migrations ownership, or marketplace discovery. Recommended fix: define plugin manifest metadata and a registry/catalog format; add compatibility checks at boot.

7. [tf#220] [MEDIUM] The comparison story against existing Rust frameworks is incomplete.
   Evidence: `arch.md:360-364` compares Reinhardt, but the public adoption argument should also cover Loco, Poem/OpenAPI stacks, Axum plus SeaORM, Rocket, Actix Web, and Leptos/server functions. Recommended fix: maintain a current "why Umbra" matrix focused on org-visible outcomes.

8. [tf#221] [MEDIUM] The current examples are demos, not a production reference app.
   Evidence: examples exist, but orgs need a reference SaaS/ecommerce/backoffice app with auth, tenancy, background jobs, realtime, storage, billing, metrics, CI, and deploy scripts. Recommended fix: build one canonical "production app" that dogfoods the whole framework.

## Identity, Auth, and Security

9. [tf#222] [HIGH] Enterprise SSO is not there.
   Evidence: OAuth has Google/GitHub built-ins only (`plugins/umbral-oauth/src/providers/mod.rs:1-10`), and the OAuth design explicitly defers SAML and broader OIDC discovery. Recommended fix: add generic OIDC discovery/JWKS/ID-token verification, SAML 2.0, enterprise domain mapping, and provider metadata.

10. [tf#223] [HIGH] MFA/passkeys are missing.
    Evidence: auth design defers TOTP/2FA (`docs/decisions/2026-06-28-auth-full-surface.md:165-170`). Firebase/Supabase-style adoption expects MFA, WebAuthn/passkeys, recovery codes, backup factors, and step-up auth. Recommended fix: create `umbral-mfa` with TOTP, WebAuthn/passkeys, recovery codes, remembered devices, and admin enforcement policies.

11. [tf#224] [HIGH] There is no organization identity lifecycle: SCIM, JIT provisioning, domain verification, group sync.
    Evidence: permissions and tenants exist, but no SCIM/OIDC group mapping lifecycle was found. Recommended fix: add org provisioning models and adapters for SCIM 2.0, OIDC claims-to-groups, domain verification, and deprovisioning.

12. [tf#225] [HIGH] Client integrity protection is missing.
    Evidence: Firebase App Check exists specifically to reduce abuse from unauthorized clients; Umbra has no equivalent device/app attestation layer. Recommended fix: add optional App Check-style attestation adapters for web/mobile clients, with enforcement hooks for REST, realtime, storage, and auth endpoints.

13. [tf#226] [HIGH] Session/device management is incomplete for account security.
    Evidence: logout revokes only the current bearer token (`plugins/umbral-auth/src/lib.rs:1179-1183`). That is a valid primitive, but users and admins need to view devices, revoke all sessions, revoke one device, enforce max session age, and respond to account compromise. Recommended fix: add session/device inventory, revocation APIs, admin UI, and security event notifications.

14. [tf#227] [MEDIUM] Magic-link/passwordless login is missing.
    Evidence: explicitly out of scope in `docs/decisions/2026-06-28-auth-full-surface.md:165-170`. Recommended fix: add a passwordless flow built on the challenge/reset infrastructure with throttling and replay protection.

15. [tf#228] [MEDIUM] Change-password for authenticated users is missing.
    Evidence: explicitly out of scope in `docs/decisions/2026-06-28-auth-full-surface.md:165-170`. Recommended fix: add built-in HTML and JSON endpoints, with old-password verification, session rotation, and optional revoke-other-devices behavior.

16. [tf#229] [HIGH] Authorization needs an organization-grade policy model.
    Evidence: permissions/RLS exist, but orgs need ABAC, named policies, auditability, policy simulation, tenant role templates, and admin-editable role assignment with guardrails. Recommended fix: layer a policy engine over permissions/RLS with typed predicates, dry-run explanations, and policy tests.

17. [tf#230] [HIGH] RLS policy bodies are raw SQL strings.
    Evidence: `plugins/umbral-rls/src/lib.rs:71-90` warns policy SQL is interpolated verbatim. That is acceptable for developer-authored SQL, but too sharp for admin/UI-managed policy. Recommended fix: provide typed policy builders for common ownership/team/tenant rules, plus lint/simulation for raw policies.

18. [tf#231] [MEDIUM] Secrets management is still environment/settings-first.
    Evidence: `Settings` has `secret_key` and `extra` (`crates/umbral-core/src/settings.rs:337-430`), but no first-class KMS/Vault/Secrets Manager integration, key rotation workflow, or per-secret metadata. Recommended fix: add a secrets provider trait with local env, AWS/GCP/Azure/Vault backends, rotation support, and system checks for stale/default keys.

19. [tf#232] [MEDIUM] Abuse controls stop short of a WAF/bot-management layer.
    Evidence: throttles and CSRF exist, but no bot score/CAPTCHA/challenge framework, IP reputation, automated lockout policy, or request firewall DSL was found. Recommended fix: add an abuse plugin that can compose throttles, captcha providers, IP deny/allow lists, honeypots, and event-based lockouts.

20. [tf#233] [MEDIUM] Security compliance evidence is not packaged.
    Evidence: there are many hardening fixes and audits, but no SOC2/HIPAA/GDPR controls map, SBOM story, SLSA/provenance, release signing, or formal security policy was found. Recommended fix: add `SECURITY.md`, release signing, cargo-deny/cargo-audit CI, SBOM generation, and a controls matrix.

## Data, ORM, and Migrations

21. [tf#234] [HIGH] Database support is currently SQLite/Postgres only.
    Evidence: Postgres rollout is complete in `FEATURES.md`, but MySQL/MariaDB, SQL Server, Oracle, and Cockroach are not first-class. This is an adoption tradeoff, not necessarily a bug. Recommended fix: either declare Postgres-only production as a strategic stance or add a backend roadmap.

22. [tf#235] [MEDIUM] GIS/PostGIS is still missing.
    Evidence: deferred backlog and `planning/gaps2.md` mention PostGIS geometry/geography as genuinely deferred. Recommended fix: add PostGIS field types, GiST indexes, SRID validation, spatial predicates, admin widgets, REST/OpenAPI schema, and inspectdb support.

23. [tf#236] [MEDIUM] Generic relations/content types are missing.
    Evidence: `docs/specs/deferred.md:35-44` defers content types/generic relations. Django-style frameworks often need this for comments, audit logs, tags, notifications, and object permissions. Recommended fix: implement `umbral-contenttypes` with typed escape hatches and admin/REST integration.

24. [tf#237] [MEDIUM] Custom managers/querysets are not a first-class extension story.
    Evidence: the ORM has strong `Manager<T>` and `QuerySet<T>` primitives, but no obvious pattern for reusable domain-specific managers comparable to Django custom managers. Recommended fix: document and/or implement custom manager traits, derive hooks, and repository-style extension patterns.

25. [tf#238] [HIGH] Online migration orchestration is partial.
    Evidence: `checkmigrations` and production migration docs exist, but deployment phases, dual writes, background backfills, contract/expand sequencing, and rollout gates are not orchestrated. Recommended fix: add a zero-downtime migration planner that emits phase plans and CI gates for add/backfill/validate/switch/drop.

26. [tf#239] [HIGH] Migration rollback/targeting is missing.
    Evidence: `crates/umbral-cli/src/lib.rs:118-155` exposes fake/drift/destructive flags but no `migrate <target>`, reverse migration, or rollback command. Recommended fix: add reversible migration metadata enforcement and a rollback/target CLI, with clear irreversible op handling.

27. [tf#240] [MEDIUM] Data migrations are raw SQL only.
    Evidence: `documentation/docs/v0.0.1/migrations/data-migrations.mdx:11-13` says data migrations are hand-authored `RunSql`. Recommended fix: add `RunCode`/Rust data migration hooks with transaction access, typed model APIs, batching helpers, idempotency helpers, and tenant-aware execution.

28. [tf#241] [MEDIUM] Migration specs are stale.
    Evidence: `docs/specs/06-migration-engine.md:21-25` still says index ops and `RunSql` are deferred even though code/docs ship them. Recommended fix: run a docs/spec drift audit and make implementation docs trustworthy before external adoption.

29. [tf#242] [HIGH] Database branching/preview environments are missing.
    Evidence: Supabase has a strong branching/preview workflow; Umbra has migrations and inspectdb but no per-PR database branch workflow. Recommended fix: add CLI recipes for ephemeral DBs, shadow databases, schema diffs, seed data, and teardown.

30. [tf#243] [HIGH] Backup/recovery is not PITR-grade.
    Evidence: backup via dumpdata/loaddata exists (`FEATURES.md:15`), but orgs need scheduled encrypted backups, WAL archiving/PITR, restore drills, retention policies, and restore verification. Recommended fix: define a Postgres backup plugin/runbook for logical and physical recovery.

31. [tf#244] [HIGH] No CDC/outbox/database webhook product exists.
    Evidence: signals exist and realtime can fan out model changes, but there is no durable database-change stream like Supabase database webhooks or a transactional outbox. Recommended fix: add outbox tables, after-commit publishing, retry, delivery logs, and webhook destinations.

32. [tf#245] [MEDIUM] Read-replica/failover is a seam, not a full operations product.
    Evidence: examples/tests exist for read replicas, but no failover policy, replica lag checks, read-your-writes strategy, or dashboard. Recommended fix: add router policies, lag metrics, failover runbooks, and readiness gating.

33. [tf#246] [MEDIUM] Search is useful but not yet a complete search product.
    Evidence: Postgres FTS exists, but no ranking DSL, faceting, highlighting, multilingual stemming config, typo/fuzzy search, sync to external search engines, or admin search tuning was found. Recommended fix: add an `umbral-search` plugin that wraps FTS and optionally syncs to Meilisearch/Typesense/OpenSearch.

34. [tf#247] [MEDIUM] Data governance primitives are thin.
    Evidence: Masked fields and privacy hiding exist, but no full data catalog, retention classes, legal hold, DSAR export/delete workflows, or per-field residency policy. Recommended fix: add model/field metadata for data classification and generate compliance workflows.

## API, SDK, and BaaS Surface

35. [tf#248] [HIGH] OpenAPI coverage misses handwritten app routes.
    Evidence: `plugins/umbral-openapi/src/lib.rs:16-19` states v1 only describes umbral-rest auto-generated endpoints. Recommended fix: make route registration and OpenAPI schema contribution part of the normal handler/action path, not an optional plugin afterthought.

36. [tf#249] [HIGH] There is no API breaking-change checker.
    Evidence: OpenAPI is generated, but no CI command was found to diff prior specs and block breaking changes. Recommended fix: add `umbral openapi diff --breaking` and generated changelog output.

37. [tf#250] [MEDIUM] Generated clients are not a full SDK program.
    Evidence: OpenAPI client generation exists, but org adoption needs versioned packages, auth refresh, retries, pagination helpers, realtime/storage/auth clients, semver, and CI publishing. Recommended fix: create official JS/TS first, then Rust/Python/Kotlin/Swift SDK plans if BaaS is the target.

38. [tf#251] [HIGH] Security rules are fragmented across REST, storage, realtime, permissions, and RLS.
    Evidence: each subsystem has controls, but Firebase/Supabase users expect one mental model for auth rules across database/realtime/storage/functions. Recommended fix: define an authorization rule DSL or policy graph that compiles to REST scopes, RLS policies, storage gates, and realtime channel checks.

39. [tf#252] [MEDIUM] No Management API or Terraform/IaC provider exists.
    Evidence: project resources are configured in code/env. Supabase-style org adoption expects API/IaC for projects, secrets, auth providers, storage buckets, webhooks, functions, and environments. Recommended fix: design a management API and a Terraform provider for self-hosted/control-plane mode.

40. [tf#253] [MEDIUM] GraphQL exists, but gRPC/protobuf does not.
    Evidence: GraphQL plugin exists; `arch.md:360` notes another Rust peer has gRPC. Recommended fix: decide whether gRPC is in strategic scope; if yes, add protobuf/service generation and auth integration.

41. [tf#254] [MEDIUM] API versioning exists but lifecycle governance is missing.
    Evidence: REST versioning exists, but no policy for deprecations, sunset headers, changelogs, multi-version docs, or SDK compatibility gates was found. Recommended fix: add API lifecycle docs and optional response headers for deprecation/sunset.

42. [tf#255] [MEDIUM] Webhook infrastructure is missing as a first-class API surface.
    Evidence: no durable webhook sender/receiver plugin was found. Recommended fix: add signed webhooks, retries, delivery logs, endpoint secrets, replay, per-tenant quotas, and admin UI.

## Realtime, Offline, and Collaboration

43. [tf#256] [HIGH] Realtime delivery is best-effort, not durable.
    Evidence: `plugins/umbral-realtime/src/lib.rs:75-84` uses bounded buffers and replay buffer for recent events. Slow consumers drop events by design. Recommended fix: add optional durable channels backed by Postgres/Redis Streams/NATS with acknowledgements and retention.

44. [tf#257] [HIGH] Offline sync is missing.
    Evidence: Firebase's realtime database/firestore value is offline-first client sync; Umbra has server push but no client-side offline cache, conflict resolution, or sync protocol. Recommended fix: design an offline sync layer for selected models with conflict strategies and client SDK support.

45. [tf#258] [MEDIUM] Realtime authorization is not unified with database/storage rules.
    Evidence: realtime has identity resolver hooks, but no first-class channel rules DSL tied to RLS/permissions. Recommended fix: add channel policies, tenant scoping, group membership helpers, and policy simulation.

46. [tf#259] [MEDIUM] Presence/collaboration state needs productization.
    Evidence: realtime primitives exist, but no complete presence model, rooms API, read receipts, typing indicators, collaborative document primitives, or admin/operator dashboard was found. Recommended fix: add optional collaboration primitives rather than leaving every app to reinvent them.

47. [tf#260] [MEDIUM] Realtime quotas and abuse controls are incomplete.
    Evidence: connection/message caps exist, but no tenant/channel quotas, billing-meter counters, noisy-neighbor isolation, or dashboard. Recommended fix: emit metrics and enforce per-tenant/channel budgets.

48. [tf#261] [MEDIUM] Realtime operational metrics are missing.
    Evidence: no Prometheus-style metrics for open connections, dropped messages, buffer pressure, reconnects, broker lag, or channel fanout was found. Recommended fix: add metrics and admin dashboard panels.

## Tasks, Events, Email, and Notifications

49. [tf#262] [HIGH] The task queue has no pluggable external broker.
    Evidence: `plugins/umbral-tasks/src/lib.rs:22-32` says no separate broker/distributed locks. DB-backed tasks are a strong default, but orgs often need Redis, AMQP/RabbitMQ, SQS, NATS, or Kafka. Recommended fix: define a broker trait and add Redis/SQS first.

50. [tf#263] [HIGH] Task operations need dead-letter queues and routing.
    Evidence: retries/status/admin exist, but no DLQ, queue routing, per-queue concurrency, resource classes, rate-limited queues, or poison-message isolation was found. Recommended fix: add queue names, DLQ, retry policies per queue, and operator controls.

51. [tf#264] [HIGH] No Horizon-like task dashboard exists.
    Evidence: tasks admin is read-only plus retry, but Laravel Horizon sets a higher bar: throughput, wait time, failures, worker status, balancing, and queue health. Recommended fix: add queue metrics, worker heartbeats, live dashboard, and alerts.

52. [tf#265] [HIGH] Transactional outbox/after-commit integration is missing.
    Evidence: signals exist, but durable "write DB row and enqueue/send exactly after commit" was not found as a productized path. Recommended fix: make `after_commit` plus outbox the blessed pattern for email, tasks, webhooks, analytics, and realtime.

53. [tf#266] [HIGH] Email has no durable retry queue.
    Evidence: `plugins/umbral-email/src/lib.rs:44-48` explicitly says no retry queue. Recommended fix: integrate email with tasks/outbox, add retry/backoff, idempotency keys, and delivery status.

54. [tf#267] [MEDIUM] Email provider support is still narrow.
    Evidence: SMTP plus Resend/SendGrid API are noted, but no SES, Postmark, Mailgun, provider failover, bounce webhooks, suppression lists, unsubscribe groups, or deliverability tooling was found. Recommended fix: add provider adapters and inbound webhook models.

55. [tf#268] [HIGH] Unified notifications are missing.
    Evidence: Laravel has notification channels; Firebase has messaging; Umbra has email/tasks/realtime separately. Recommended fix: add `umbral-notifications` for email, SMS, push, Slack/webhook, in-app, templates, preferences, digests, and delivery logs.

56. [tf#269] [MEDIUM] Scheduler operations need more controls.
    Evidence: beat exists and guards double-fire, but no admin pause/resume, per-schedule locks, missed-run policy, calendar/timezone UX, or HA leader election visibility was found. Recommended fix: expand PeriodicTask into an operator-grade scheduler.

57. [tf#270] [MEDIUM] Signals are in-process.
    Evidence: `FEATURES.md:25` says signals are strictly in-process v1 with no Redis/NATS broker or replay. Recommended fix: add optional durable/distributed signal bus and make local-only semantics explicit in docs.

## Storage and Media

58. [tf#271] [HIGH] Direct-to-object-storage uploads are not first-class.
    Evidence: storage supports S3 and signed media URLs, but upload flows still largely pass through the Rust app; `plugins/umbral-storage/src/lib.rs:51-63` notes proxy round-trips for gated S3/custom backends. Recommended fix: add presigned POST/PUT, multipart uploads, resumable uploads, client SDK helpers, and completion callbacks.

59. [tf#272] [HIGH] Malware scanning/quarantine/DLP is missing.
    Evidence: upload caps and active-content guards exist, but no antivirus, quarantine state, moderation workflow, DLP scan, or async approval pipeline was found. Recommended fix: add storage processors for ClamAV/vendor scanners, quarantine status, and admin review.

60. [tf#273] [MEDIUM] CDN integration is partial.
    Evidence: collectstatic hashed assets exist and docs suggest CDN/proxy, but no CDN invalidation, signed cookies, edge cache policies, image CDN transforms, or cache purge hooks. Recommended fix: add CDN provider adapters and explicit cache invalidation APIs.

61. [tf#274] [MEDIUM] Storage retention/lifecycle/legal hold is missing.
    Evidence: file cleanup exists, but no bucket lifecycle, retention labels, tenant quotas, legal holds, soft-delete windows, or purge jobs. Recommended fix: add storage policies tied to data governance metadata.

62. [tf#275] [MEDIUM] Media processing needs a fuller pipeline.
    Evidence: thumbnails exist behind feature flags, but no documented variants/srcset pipeline, EXIF stripping, AVIF/WebP transcoding policy, video/audio processing, or moderation hooks. Recommended fix: add a media pipeline spec and processors for common production needs.

63. [tf#276] [MEDIUM] Storage access rules are not unified with auth policies.
    Evidence: media gates are callbacks, while REST/RLS/permissions use separate concepts. Recommended fix: compile shared policies into storage gates and signed URL claims.

## Observability, Operations, and Performance

- eBPF observability - with Aya rust library for the same (Out of topic for now but good to have some information around this)

64. [tf#277] [HIGH] Metrics are missing.
    Evidence: observability docs cover logs and OTLP traces, while deeper instrumentation is deferred (`documentation/docs/v0.0.1/observability/index.mdx:79-85`). No Prometheus `/metrics` exporter was found. Recommended fix: add counters/histograms for HTTP, DB, cache, tasks, storage, realtime, auth, and queue latency.

65. [tf#278] [HIGH] Trace propagation is missing.
    Evidence: `documentation/docs/v0.0.1/observability/index.mdx:83-85` says W3C `traceparent` extraction is not wired. Recommended fix: implement trace context extraction/injection across HTTP clients, tasks, email/webhooks, and realtime.

66. [tf#279] [HIGH] DB/task spans are missing.
    Evidence: same observability doc explicitly defers per-DB-query and per-task spans. Recommended fix: instrument ORM queries, migrations, task enqueue/claim/run, email sends, cache calls, and storage operations.

67. [tf#280] [HIGH] Rate limiting is in-memory and opt-in.
    Evidence: `documentation/docs/v0.0.1/rest/throttling.mdx:11-15` says state lives in process and multi-instance limits multiply by replica count. Recommended fix: add Redis/distributed limiter backend and a production scaffold default.

68. [tf#281] [MEDIUM] Logs stored in the app DB can become load and retention risk.
    Evidence: `plugins/umbral-logs/src/lib.rs:1-7` inserts request logs asynchronously into the database. Recommended fix: add log drains/sinks (OTLP logs, Kafka, S3, ClickHouse, Datadog), retention jobs, and per-tenant sampling.

69. [tf#282] [MEDIUM] Analytics is PostHog-only and best-effort.
    Evidence: `plugins/umbral-analytics/src/lib.rs:1-8` describes PostHog fire-and-forget sends. Recommended fix: add pluggable analytics destinations, consent hooks, warehouse export, event schema registry, and retry/outbox for critical product events.

70. [tf#283] [HIGH] Incident readiness is not packaged.
    Evidence: health exists, but no SLO templates, alert rules, runbooks, saturation dashboards, dependency degradation modes, or incident drills were found. Recommended fix: ship Grafana/Prometheus dashboards, alert examples, and operational runbooks.

71. [tf#284] [MEDIUM] Slow query and N+1 detection are mostly test-time, not operator-facing.
    Evidence: query-count tests exist, but no runtime slow query log, explain integration, admin query profiler, or dev toolbar was found. Recommended fix: add slow query threshold logging, `EXPLAIN` helper, and development profiler.

72. [tf#285] [MEDIUM] Public performance benchmarks are missing.
    Evidence: no benchmark dashboard comparing Umbra to Axum baseline, Loco, Django, Laravel, etc. was found. Recommended fix: maintain reproducible TechEmpower-style and app-realistic benchmarks.

73. [tf#286] [MEDIUM] Health checks need dependency registry depth.
    Evidence: health plugin exists, but orgs need standardized checks for DB, migrations, Redis, S3, email, OAuth providers, task workers, realtime broker, and disk. Recommended fix: add dependency health registry and readiness/liveness profiles.

## Admin, Developer Experience, and Docs

74. [tf#287] [HIGH] i18n/l10n is still missing.
    Evidence: deferred backlog and docs config show i18n is not implemented. Django/Laravel-class frameworks need translations, locale routing, pluralization, date/number formatting, timezone behavior, and admin localization. Recommended fix: add `umbral-i18n`.

75. [tf#288] [MEDIUM] Admin is strong CRUD, but not yet an organization console.
    Evidence: admin has deep CRUD/dashboard pieces, but no full org/team management console, identity provider configuration UI, audit event explorer across plugins, tenant switcher, support impersonation with approvals, or compliance exports was found. Recommended fix: evolve admin into an ops/control console.

76. [tf#289] [MEDIUM] `startcommand` can still rot and shadow third-party commands.
    Evidence: `planning/gaps3.md:406` and `crates/umbral-cli/src/scaffold.rs:93-129` show hardcoded plugin command reservations. Recommended fix: forward `startcommand` into the project and derive names from the live command registry.

77. [tf#290] [MEDIUM] Plugin route discovery still has escape-hatch drift.
    Evidence: `crates/umbral-core/src/plugin.rs:174-214` documents that `routes()`/`route_paths()` can drift and nested/merged routers cannot be introspected. Recommended fix: make `Routes` builder the preferred/scaffolded path and warn in system checks when legacy routes lack specs.

78. [tf#291] [MEDIUM] Plugin settings are ad-hoc.
    Evidence: `Settings.extra` and per-plugin `from_settings` patterns exist, but no typed plugin config schema registry was found. Recommended fix: let plugins declare settings schemas, env names, validation, secrets, defaults, and docs generation.

79. [tf#292] [MEDIUM] Error taxonomy is not unified across plugins.
    Evidence: plugins expose their own errors; no single public error code taxonomy, user-safe/internal-safe distinction, or RFC7807-style response contract was found across all surfaces. Recommended fix: define standard error codes and response shapes for REST/GraphQL/auth/admin/tasks.

80. [tf#293] [MEDIUM] Documentation trust needs a drift sweep.
    Evidence: migration spec drift (`docs/specs/06-migration-engine.md:21-25`) proves docs can lag shipped behavior. Recommended fix: add docs tests/link checks/spec owner reviews and mark stale specs clearly.

81. [tf#294] [MEDIUM] There is no official plugin author test/certification harness.
    Evidence: plugin trait exists but no plugin compliance suite was found. Recommended fix: provide tests that third-party plugins can run for migrations, route specs, OpenAPI, system checks, settings, admin, auth, and semver compatibility.

82. [tf#295] [MEDIUM] Project scaffolds need production variants.
    Evidence: quickstart scaffold is great for starting; orgs need `--profile api`, `--profile saas`, `--profile backoffice`, `--profile baas`, `--prod-hardening`. Recommended fix: add opinionated templates with CI/deploy/observability wired.

## Enterprise, Governance, and Platform Features

83. [tf#296] [HIGH] Feature flags/remote config are missing.
    Evidence: no Remote Config/feature flag plugin was found, while Firebase has Remote Config and orgs need progressive rollout. Recommended fix: add feature flags, percentage rollout, user/tenant targeting, kill switches, audit, and SDK access.

84. [tf#297] [HIGH] Billing, quotas, and usage metering are missing.
    Evidence: if Umbra aims at Supabase/Firebase-like use, it needs project usage, tenant quotas, metered resources, plan limits, and billing hooks. Recommended fix: add a metering subsystem covering API calls, storage bytes, realtime connections/messages, tasks, DB rows, and seats.

85. [tf#298] [HIGH] Multi-region and data residency are missing.
    Evidence: no region model, residency policy, or multi-region deployment story was found. Recommended fix: define regional projects, tenant region pinning, storage/database residency, replication/failover, and routing.

86. [tf#299] [HIGH] Compliance workflows are missing.
    Evidence: privacy fields exist, but no DSAR workflow, retention automation, consent ledger, processing-purpose metadata, export/delete approvals, or compliance report generator was found. Recommended fix: add compliance plugins tied to model metadata.

87. [tf#300] [HIGH] Audit trail needs tamper-evident mode.
    Evidence: audit/logging exists in DB rows, but no append-only/tamper-evident audit log or external immutable sink was found. Recommended fix: add hash-chained audit events and optional WORM/S3/Kafka sink.

88. [tf#301] [HIGH] Team/org access control for the framework/control plane is missing.
    Evidence: app auth exists, but not project-level roles like owner/admin/developer/viewer/billing/security. Recommended fix: add project/team RBAC if a BaaS/control-plane path is pursued.

89. [tf#302] [MEDIUM] No formal support lifecycle exists.
    Evidence: no LTS policy, supported versions matrix, maintenance branches, or enterprise support model was found. Recommended fix: define support windows and upgrade tooling before courting orgs.

90. [tf#303] [MEDIUM] No marketplace governance exists.
    Evidence: no package review, verified publisher, security scan, or plugin trust signal. Recommended fix: add plugin signing, verified publishers, security badges, and compatibility metadata.

91. [tf#304] [MEDIUM] No managed functions/edge-functions story exists.
    Evidence: Firebase/Supabase both include functions; Umbra has app handlers and tasks but no deployable function unit with secrets, logs, schedules, and resource limits. Recommended fix: decide whether functions are just handlers/tasks or introduce a functions plugin/runtime.

92. [tf#305] [MEDIUM] Environment promotion is not a product feature.
    Evidence: no dev/staging/prod project graph, config diff, secret promotion, DB migration preview, or release approval workflow was found. Recommended fix: add environment model and promotion CLI/control-plane flows.

## Testing, Release, and Quality

93. [tf#306] [HIGH] Live-service CI coverage is incomplete.
    Evidence: `crates/README.md:63-77` says Postgres tests require `UMBRAL_TEST_POSTGRES_URL` and ignored tests. That is reasonable locally, but release CI needs live Postgres/Redis/S3-compatible services. Recommended fix: add Testcontainers/docker-compose CI matrix for Postgres, Redis, MinIO, and relevant feature flags.

94. [tf#307] [HIGH] Reliability/chaos testing is missing.
    Evidence: no chaos tests for DB failover, Redis disconnects, S3 latency, worker crashes, broker partitions, or realtime reconnect storms were found. Recommended fix: add fault-injection tests and long-running reliability jobs.

95. [tf#308] [MEDIUM] Fuzz/property tests are not visible for critical parsers/planners.
    Evidence: migration diff/rendering, filters, multipart, settings/env parsing, SQL builders, and route patterns would benefit from fuzzing. Recommended fix: add `cargo-fuzz` or property tests for those surfaces.

96. [tf#309] [HIGH] Upgrade compatibility tests are missing.
    Evidence: no fixtures of old generated projects/migrations being upgraded through current crates were found. Recommended fix: maintain golden projects for prior releases and run upgrade tests in CI.

97. [tf#310] [MEDIUM] Load/soak tests are missing.
    Evidence: no soak tests for 10k websocket clients, queue backlogs, large migrations, storage uploads, or admin list scale were found. Recommended fix: create reproducible k6/vegeta/Locust scenarios and publish results.

98. [tf#311] [MEDIUM] Security regression tests need packaging as a permanent suite.
    Evidence: audits are present and many fixes landed, but org confidence requires an always-on security test suite grouped by OWASP/API/storage/auth/realtime/migrations. Recommended fix: consolidate security regressions and run them under CI profiles.

99. [tf#312] [MEDIUM] Release automation and provenance are not obvious.
    Evidence: crates are published, but no full release checklist with signing, changelog validation, SBOM, advisory scan, and docs publish verification was found. Recommended fix: add a release pipeline spec and automation.

100. [tf#313] [MEDIUM] The framework needs a public maturity matrix.
    Evidence: many subsystems are complete enough to use, others are explicitly partial. Recommended fix: publish a matrix with status levels such as experimental, beta, production, enterprise, and managed-platform-ready for each plugin.

## Highest-Leverage Sequence

If the goal is "organizations can realistically adopt this", the next sequence should not be random feature accretion. The highest leverage path is:

1. Define product boundary and stability policy.
2. Ship production preset, deployment reference architecture, metrics, distributed throttling, and live-service CI.
3. Close enterprise identity: generic OIDC, SAML, MFA/passkeys, session/device management, SCIM.
4. Add zero-downtime migration planning, rollback/targeting, backup/PITR runbooks, and upgrade tests.
5. Add durable outbox/webhooks/notifications and queue operations dashboard.
6. Add feature flags, billing/quotas/metering, and project/team control-plane concepts if the Supabase/Firebase path is real.
7. Build one production reference SaaS app that dogfoods everything above.

## Security (addendum)

101. [tf#322] [HIGH] IDOR protection has the pieces but no unified contract or documentation.
    Evidence: object-level authorization already ships across four subsystems — REST `ResourceConfig::owned_by`/`scope`/`owner_field` (`plugins/umbral-rest/src/resource.rs:388-521`, audit_2 H1/P2), GraphQL `GraphqlPlugin::owned_by(table, owner_column)` (`plugins/umbral-graphql/src/lib.rs:255-395`, gaps4 #9), RLS row filtering (`plugins/umbral-rls/src/lib.rs`), and storage file→owner gates + signed URLs (`plugins/umbral-storage/src/lib.rs`, gaps4 #56-58) — but they are documented in isolation, share no mental model, and nothing flags a write surface that was left unscoped. A developer who wires REST `owned_by` yet forgets the matching RLS policy or storage owner gate has an Insecure Direct Object Reference hole and no signal telling them. Recommended fix: (1) ship a cross-cutting "Object-level authorization / IDOR" guide presenting `owned_by` (REST + GraphQL) + RLS policies + storage owner gates as one defense-in-depth story with a decision guide for which layer to reach for; (2) add a boot-time system check that warns when a write-enabled REST resource / GraphQL mutation model has neither an object `scope`/`owned_by` nor a covering RLS policy; (3) tie it into the coming policy engine (#16/#17/#38) so a single named policy compiles to REST scope + RLS policy + storage gate + realtime channel check, declared once rather than four times. Relates to #16, #17, #38.
    (Note: the uniform `#N → tf#(N+213)` mapping above held only through #100; this entry's board task is the real id tf#322, created after the board drifted past #313.)

## Web layer / middleware (addendum)

102. [tf#323] [MEDIUM] The middleware layer has no first-class, ordered, introspectable contract.
    Evidence: middleware is contributed implicitly today — plugins wrap the router via `Plugin::wrap_router` (SecurityPlugin's CSRF + hardening headers, sessions, the user-context layer, host validation), composed in plugin dependency order over axum/tower. That works, but there is no explicit middleware ordering/priority contract (composition order is a side effect of plugin dependency order, not a declared thing), no per-route or per-subtree scoping (middleware is effectively global), no typed `Middleware` abstraction over raw `tower::Layer` (`wrap_router` is the only seam), and no introspection ("what middleware is active, in what order?") to mirror route introspection. The pipeline is also under-documented — there is no single "request pipeline" page (security → host-check → sessions → user-context → handler) or guidance on inserting a layer at a chosen position. Recommended fix, leaving an explicit opening for the pipeline to grow: (1) a first-class middleware registry with explicit, stable ordering (named phases or numeric priorities) so cross-plugin composition is predictable; (2) per-route / per-scope middleware application; (3) an optional typed `Middleware` contract with `wrap_router` as the documented escape hatch; (4) middleware introspection + system-check-style footgun warnings (auth-without-CSRF is partly covered by `plugin.security_missing`); (5) a "Middleware / request pipeline" doc page. Future ordered layers this opens the door to: rate-limiting, tracing/traceparent spans, tenant resolution, compression — declared, positioned layers rather than ad-hoc wraps. Relates to the observability (#57/#58 area) and throttling (#67) roadmap items.

## Workflow / state machines (addendum)

103. [tf#327] [HIGH] No state-machine / workflow engine for model lifecycles.
    Evidence: apps constantly model lifecycles — an order (placed → paid → shipped → delivered), a submission under review, a multi-party approval — but umbral offers only a bare `status` enum (`#[umbral(choices)]`) and leaves the developer to hand-roll the transition rules, the "who may do this" checks, the audit trail, and any multi-actor gate. Nothing enforces that `shipped` can only follow `paid`, records who moved it and when, or expresses "two managers must both approve before it advances" or "a reviewer requests changes → the user edits and resubmits." No `state machine` / `workflow` / `transition` machinery was found in the codebase. Recommended fix: add a `umbral-workflow` plugin that attaches a declarative FSM to a model — states + transitions declared once (event, from-states → to-state) and enforced through `entity.transition(Event, actor)` (an illegal from-state or a failed guard is rejected, never silently applied); guards that decide who may fire a transition (predicate + permission/ownership, tying into permissions/RLS); multi-actor transitions gated on N approvals from distinct actors (quorum / M-of-N), tracked in an approval table so the transition fires only when quorum is met; first-class resubmit / request-changes loops (backward transitions); a recorded transition history (entity, from, to, event, actor, at, note), optionally hash-chained (ties to #87 tamper-evident audit); on-transition effects and an emitted event (blessed via after-commit/outbox, #52/#31, for webhooks/tasks/email/realtime); and surfaces — an admin state widget with per-object transition buttons that respect the guards, REST/GraphQL "allowed transitions" + a transition endpoint, and a generated FSM diagram. Support the full complexity range: a simple 2-state toggle, a linear order lifecycle, and complex flows (multi-actor quorum, resubmit loops, SLA/timeout transitions driven by tasks). Design: `docs/decisions/2026-08-10-state-machine-workflow-plugin.md`. Relates to #31, #52, #87, #20, and the storage moderation-workflow note (#63 area).

## Data / graph queries (addendum)

104. [tf#328] [LOW] No property-graph query surface over the model FK graph (coming-later).
    Evidence: deep or variable-length traversals across foreign keys — org hierarchies (`reports_to` chains), friends-of-friends, role/permission inheritance, dependency / bill-of-materials — are painful today: N explicit joins or a hand-written recursive CTE. PostgreSQL is gaining SQL/PGQ (the SQL:2023 property-graph query feature: `CREATE PROPERTY GRAPH` + `GRAPH_TABLE(g MATCH (a)-[:rel]->(b) …)`), targeted for the PG 19 line, and umbral already owns the graph — every model declares its `ForeignKey`/M2M relationships, so the vertices (models) and edges (FKs / junction tables) are known without the developer defining anything. Recommended fix (parked until the Postgres feature ships/stabilizes): auto-derive a property graph from the registered models (vertex per model, edge per FK + M2M), kept in sync by the migration engine as a PG-only op, and expose an ORM graph-query surface (`Model::graph().match_("(a)-[:rel]->(b)").select::<T>("b")`) that compiles to `GRAPH_TABLE(umbral_graph MATCH …)` on Postgres and hydrates typed results across hops — "zero joins, just a graph query." One API, two backends: native SQL/PGQ on Postgres; on SQLite compile fixed-length patterns to joins, PG-only-gate the recursive/variable-length ones with a clear warning (never silently diverge). Queries run over the base tables, so RLS still applies. This is an ORM capability, not the existing `umbral-graphql` plugin (which is the API layer). Design note: `docs/decisions/2026-08-11-property-graph-queries-pgq.md`. Relates to the ORM (`select_related`), RLS, and the migration engine.

## Model bases (addendum)

105. [x] Typed column constants for `#[umbral(flatten)]`-inherited base columns — SHIPPED via `mixin_cols!`.
    `#[derive(ModelBase)]` now also emits a `#[macro_export] macro_rules! __umbral_base_cols_<Base>` that expands to the base's column consts parameterized by a target model type. The user opts in per model with `umbral::mixin_cols!(Article: TimeStamped)` (or several bases, comma/`+`-separated), which emits `impl Article { pub const ID/CREATED_AT/…: Col<Article> }` — so base columns are usable in `filter`/`order_by` (`Article::CREATED_AT.desc()`) exactly like own fields. Because the caller supplies the base path, cross-crate resolution works when the base is spelled with its crate (`mixin_cols!(Model: shared::Base)`); a same-crate base is named bare. The `#[derive(Model)]` proc-macro is NOT changed to do this automatically — that would break cross-crate flatten with an "unresolved macro" error — so `mixin_cols!` stays opt-in. Impl: `crates/umbral-macros/src/lib.rs` (`mixin_cols` proc macro, `col_type_ident`, the base-cols macro in the Base-mode emission), facade `crates/umbral/src/lib.rs` (`pub use umbral_macros::mixin_cols`). Tested: `crates/umbral-core/tests/model_base.rs::mixin_cols_generates_typed_base_column_consts`. Docs: `documentation/docs/v0.0.1/orm/model-base.mdx`.
