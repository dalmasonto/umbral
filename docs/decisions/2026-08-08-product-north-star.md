# Product north star: what umbral is, and what it is becoming

Status: draft for ratification (proposes the answer to gaps5 #1; the final call is the maintainer's)
Date: 2026-08-08
Decision coverage: planning/gaps5.md #1 (tf #214). This closes the product-boundary decision, not the downstream implementation work.

## The question

umbral ships both framework primitives (ORM, migrations, routing, the plugin system) and BaaS-shaped plugins (auth, storage, realtime, tasks, admin, RLS, tenants, email, cache). That breadth is a strength, but it leaves the product boundary ambiguous: is umbral a framework, a self-hosted backend platform, or a managed BaaS? Supabase and Firebase compete partly by answering this crisply. They define projects, environments, credentials, dashboards, logs, quotas, billing, and management APIs as one product. umbral has not named its shape, so an adopter cannot tell what they are buying into or what will be supported.

## The three shapes

1. **Framework only.** A library plus a CLI you build apps with. You own deployment, operations, and multi-tenancy. Peers: the Django/Rails/Laravel role in Rust, plus Loco and Cot.
2. **Self-hosted platform.** The framework plus an opinionated, batteries-on runtime that a team self-hosts to get auth, storage, realtime, admin, tasks, health, metrics, and secure defaults wired together, with a real operations story (deployment topologies, backups, observability). No managed control plane; the team runs it.
3. **Managed cloud (BaaS).** A hosted product: projects, environments, API keys, dashboards, quotas, billing, per-project resource provisioning, a management API. This is a business and an operations organization, not just code.

## Where umbral actually is today (evidence)

- It is **framework-first**: `App::builder().plugin(...).build()`, `umbral startproject`, path-dep example apps. There is no project/environment model, no control plane, and no billing.
- It already has most of a self-hosted platform's **ingredients**: auth, sessions, permissions, RLS, tenants, storage (S3 plus signed media), realtime, tasks plus beat, email, cache, admin, health, logs, analytics, and security headers plus CSRF on by default. What is missing is the **preset** that turns them on together (gaps5 #3) and the **operations reference** (gaps5 #4), plus metrics (gaps5 #64) and distributed throttling (gaps5 #67) to make it operable at more than one replica.
- It is **nowhere near a managed cloud**: no projects, no per-tenant provisioning, no billing, no management API.

## Recommendation: a staged path (framework first, platform next, managed optional)

Commit publicly to this ordering and contract:

- **Stage 1 (now, and the 1.0 promise): umbral is a declarative, plugin-first web framework for Rust.** This is the supported product. Every capability is a plugin. The stability policy (gaps5 #2), the docs, and semver apply to this surface. This is what the README, crates.io, and the docs already say after the 2026-08-08 rebrand.
- **Stage 2 (the near-term differentiator): a first-class self-hosted platform posture, delivered as an opt-in preset plus an operations reference, not a separate product.** Concretely: `EnterprisePreset` (gaps5 #3) wires the secure production posture; a deployment reference architecture (gaps5 #4) documents web/worker/beat/Postgres/Redis/S3/OpenTelemetry topologies; metrics (gaps5 #64) and distributed throttling (gaps5 #67) make it operable. This makes "self-host a backend platform" a supported, documented outcome without umbral becoming a hosting company.
- **Stage 3 (optional, gated on demand and resourcing): a managed control plane** (gaps5 #5) plus project/team RBAC (gaps5 #88) plus billing/metering (gaps5 #84). Out of scope until Stages 1 and 2 are solid and there is a business case. Design the seams now (a project/tenant abstraction a control plane could later manage) so Stage 3 is additive, not a rewrite.

## What this commits us to

- We describe umbral as "a declarative web framework" (Stage 1) with a "self-hostable backend platform" posture (Stage 2), and we do not market managed-cloud features we do not run.
- The **plugin contract stays the boundary**: platform features are plugins, so Stage 2 is composition, not a fork. This is the same bet as the motto ("every capability is a plugin, including auth").
- Every BaaS-shaped plugin owns its self-hosted operational needs (backups, metrics, scaling) as part of Stage 2, so "self-hosted platform" is honest rather than aspirational.

## v0.0.12 to v0.1 adoption gate

Use v0.0.12 as the implementation release that turns the highest-leverage design decisions into shipped, test-backed production posture. v0.1 should not be described as broadly adoptable until the following are true:

1. **The framework contract is public and enforceable.** `STABILITY.md`, the maturity matrix, plugin manifests/compatibility checks, docs-drift checks, and the OpenAPI breaking-change gate are published and wired into CI.
2. **The production posture is one switch.** `EnterprisePreset` and `startproject --prod-hardening` exist, with boot checks for default/empty `secret_key`, debug/dev mode, secure cookies/HSTS, allowed hosts, trusted proxies, Postgres production stance, bearer-token max age, provider callback safety, and staff-MFA waiver/requirement.
3. **Operations are observable and tested.** `/metrics`, traceparent extraction/injection, DB/task spans, distributed throttling, live-service CI for Postgres/Redis/MinIO, and the permanent security/regression suite run before release.
4. **Identity is credible for orgs.** At minimum: generic OIDC discovery/JWKS/ID-token verification; first-class OAuth provider catalog beyond Google/GitHub (Apple, Microsoft, Facebook, X/Twitter, LinkedIn, GitLab, Bitbucket, Discord, Slack, custom OAuth2/OIDC); TOTP + recovery codes; session/device inventory with revoke-one/revoke-all; authenticated change-password finishing work; and SCIM/JIT/domain verification design implemented far enough that org onboarding is not hand-rolled. SAML and WebAuthn/passkeys can follow behind feature flags if they are not yet stable.
5. **Data movement is durable.** Zero-downtime migration planning, rollback/targeting, database branching/shadow verification, PITR backup runbooks, transactional outbox/after-commit, webhook delivery logs, durable email retry, and task DLQ/operator dashboard exist or are explicitly marked beta in the maturity matrix.
6. **Storage/realtime have production boundaries.** Direct uploads, CDN invalidation hooks, storage scanning/quarantine, retention/legal hold, durable realtime/offline-sync posture, realtime quotas, and realtime metrics are either implemented or marked as not-v0.1 in the maturity matrix with safe defaults.

Everything managed-cloud-shaped remains out of the v0.1 adoption bar unless the product decision changes: managed projects/control plane (gaps5 #5), management API/Terraform (#39), billing/quotas as a hosted business feature (#84), project/team control-plane RBAC (#88), environment promotion (#92), and multi-region/residency (#85). Those are Stage 3 seams; v0.1 can be adoptable as a framework/self-hosted platform without pretending to be a hosted BaaS.

## What is explicitly out (for now)

Managed hosting, per-project provisioning, a billing system, a public management API and Terraform provider (gaps5 #39), and multi-region/residency (gaps5 #85). These are Stage 3, deferred.

## Open decision for the maintainer

Ratify the ordering (framework, then self-hosted platform, then optional managed) or pick a different shape. If Stage 3 (managed cloud) is a near-term goal rather than an optional later stage, say so, because that reprioritizes roughly fifteen backlog items (control plane, billing, project/team RBAC, residency, environment promotion, management API). Until then, the backlog is sequenced Stage 1 hardening first, Stage 2 platform posture second.
