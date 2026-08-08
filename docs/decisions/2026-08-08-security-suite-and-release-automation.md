# Permanent security regression suite and release automation with provenance

Status: draft for ratification (proposes the design for gaps5 #98 and #99; the final call is the maintainer's)
Date: 2026-08-08
Closes: planning/gaps5.md #98 (tf #311), #99 (tf #312)

## Context: what already exists

Neither of these items starts from zero. The framework has already absorbed two full security audit rounds (`planning/review/`, the audit_2 batch) and shipped the fixes; it publishes 28 crates in lockstep through release-plz; and it runs a supply-chain gate in CI. The gap both items name is the same one: the work exists but it is not *packaged* as an always-on, named guarantee that an adopting organization can point at. A team doing due diligence cannot today run one command and see "the security regressions all pass" or "this release was signed and its SBOM is here". These two designs turn the scattered evidence into two standing contracts.

### For #98, the security tests already exist but are scattered

A sweep of the tree shows the regressions are real and passing, just spread across per-crate `tests/` directories with no shared identity or grouping. The load-bearing ones already in the tree:

- **Web surface / OWASP.** `crates/umbral-core/tests/default_security_headers.rs` (X-Content-Type-Options, X-Frame-Options), `csrf_context.rs`, `render_str_autoescape.rs` and `static_url_escaping.rs` (template autoescape / XSS), `system_check_security_missing.rs` (boot warns when auth/sessions mounted without SecurityPlugin). `plugins/umbral-security/tests/` adds `csrf_flow.rs`, `private_cache.rs`, `production_hardened.rs`, `empty_secret_key.rs`.
- **API auth / authz.** `plugins/umbral-rest/tests/default_safe_permission.rs` (the WEB-1 regression: writes now 403 without opt-in), `auth_permission.rs`, `bulk_security.rs` (mass-assignment / WEB-2), `nested_child_permission.rs`, `throttle.rs`. `plugins/umbral-admin/tests/authz_web7_read_endpoints.rs`, `phase3_action_permissions.rs`, `sidebar_perm_gate.rs` (the WEB-7 bulk-action / autocomplete permission gaps). `plugins/umbral-permissions/tests/` and `plugins/umbral-openapi/tests/client_auth.rs`.
- **Storage.** `plugins/umbral-storage/tests/media_symlink_escape.rs` and `static_symlink_escape.rs` (path-traversal / symlink escape), plus the media MIME/extension gate (WEB-4) covered in `plugins/umbral-rest/tests/file_image_urls.rs`.
- **Auth / session.** `plugins/umbral-auth/tests/` carries `password_validation.rs`, `hash_concurrency_gate.rs`, `throttle.rs`, `email_action_throttle.rs`, `identity_superuser.rs`, `privileged_fields.rs` (the AUTH-4 superuser-field guard), `change_password.rs`, `password_reset.rs`, `challenge_lifecycle.rs`. `plugins/umbral-sessions/tests/` carries `same_site_cookie.rs`, `max_session_age.rs`, `revoke_user_sessions.rs`, `cookie_store_boot_check.rs`, `redis_store.rs`. `plugins/umbral-email/tests/header_injection.rs`. `plugins/umbral-oauth/tests/state_csrf.rs` and `pkce_flow.rs`.
- **Realtime.** `plugins/umbral-realtime/tests/publish_authz.rs` (channel publish authorization).
- **Migrations / ORM SQL.** The ORM/SQL audit (`planning/review/security-orm-sql.md`) found the injection surface clean; the regressions that guard it (identifier quoting via `Alias::new`, bound parameters, LIKE-wildcard escaping in `contains`/`startswith`, hostile-DB-safe inspectdb, escaped DDL) live inline in `crates/umbral-core` ORM and migration tests.

What is missing is not coverage. It is a name, a grouping by threat category, and a CI profile that makes "did any security regression break?" a single, un-skippable signal instead of a needle spread across 80-plus test files.

### For #99, release plumbing exists but provenance does not

- `release-plz.toml` drives the lockstep publish of every `umbral-*` crate (one `version_group = "umbral"`, so "umbral 0.0.N" is unambiguous). `semver_check` is on again as of 0.0.10 and actually earns its keep (it caught the `dispatch_with_app_commands` signature change).
- `.github/workflows/release-plz.yml` is dispatched manually (a "slow, silent" release, not per-push).
- `.github/workflows/audit.yml` already runs `cargo audit` against `Cargo.lock` on dependency changes and weekly, reading tracked exceptions from `.cargo/audit.toml`.
- `.github/workflows/deploy-docs.yml` publishes the documentation site.
- `SECURITY.md` already lists the supply-chain roadmap (cargo-audit + cargo-deny in CI, SBOM per release, release signing / SLSA provenance, a controls matrix) as "planned and tracked, not yet all in place".

The gap #99 names is that a release is currently a `release-plz` dispatch with no checklist wrapped around it: no license/ban gate (`cargo deny`), no SBOM artifact, no signature or attestation on what shipped, no automated changelog validation, and no post-publish verification that the docs for the new version actually went live. This design fills the roadmap SECURITY.md already promised.

---

## #98 (tf #311): `umbral-security-tests`, a standing security regression suite

### Problem

The security regressions pass, but nothing makes them a category. A broken CSRF check and a broken pagination cap fail the same undifferentiated `cargo test` as a typo in a docstring test, so "security posture regressed" is not a distinct, alertable event. There is also no single place a reviewer or an adopter can read to see what threats the suite claims to defend against, and no CI profile that runs the security-relevant tests as their own gate.

### Design

**A dedicated crate: `crates/umbral-security-tests`.** A new workspace member (dev-only, `publish = false`, NOT added to `release-plz.toml`) whose sole job is to host and organize security regressions. It depends on the facade plus `umbral-testing` and every built-in plugin under test, so it can exercise the real wired-together stack the way a consumer would (per the "behavioral tests, real rows, actual public path" convention), rather than reaching into crate internals.

It does not *replace* the inline per-crate tests. A regression that only makes sense next to the code it guards (LIKE-escaping in the QuerySet, identifier quoting in the migration renderer) stays where it is. `umbral-security-tests` is the integration layer: cross-plugin attack paths, the secure-by-default posture of a freshly built `App`, and the OWASP-shaped scenarios that span more than one crate. The design goal is one crate a reviewer can open to answer "what security properties does umbral assert?".

**Organized by threat category, one module per category.** The module tree mirrors the audit taxonomy so an adopter maps it straight onto their own threat model:

```
crates/umbral-security-tests/tests/
  web_owasp.rs        # A01 broken access control, A03 injection, A05 misconfig,
                      #   A07 auth failures: CSRF, clickjacking/security headers,
                      #   template autoescape/XSS, open-redirect, host-header,
                      #   secure-by-default boot posture (system checks fire).
  api_authz.rs        # REST/GraphQL/admin: default-deny writes, mass-assignment
                      #   (noedit/hide enforced), object-scoping, bulk-op perms,
                      #   pagination cap (PERF-1 DoS), error-body schema redaction.
  storage_media.rs    # path traversal, symlink escape, MIME/extension allow-list,
                      #   active-content guard, signed-URL gating, upload caps.
  auth_session.rs     # argon2 params, no user enumeration, session-fixation rotate,
                      #   token/session revocation, SameSite/Secure/HttpOnly flags,
                      #   throttle/lockout, OAuth state (CSRF) + PKCE, email header
                      #   injection, superuser-field guard.
  realtime.rs         # channel publish/subscribe authorization, identity resolver,
                      #   per-connection/tenant caps.
  migrations_sql.rs   # SQL-injection sweep (identifiers quoted, values bound),
                      #   LIKE-wildcard escaping, hostile-DB-safe inspectdb,
                      #   escaped DDL, no schema disclosure in migration errors.
```

Each test carries a one-line doc comment naming the finding or contract it guards (for example `// AUTH-4: is_staff/is_superuser not settable through the generic admin form`), so a failure names the threat, not just an assertion. Where a test re-pins a finding from `planning/review/`, the comment cites the finding id; where it pins a secure-by-default posture from `SECURITY.md`, it cites that. This makes the suite the living, executable version of `SECURITY.md`'s "secure-by-default posture" section.

**A shared harness.** A small `harness` module builds the canonical "secure app" (the `EnterprisePreset` wiring from gaps5 #3, or the explicit secure builder) and the canonical "naive app", so a scenario can assert both "the safe wiring blocks the attack" and "the boot-time system check warns when a consumer forgets the safe wiring". Reusing `umbral-testing`'s `TestClient` and `TempPool` keeps every scenario a real request against a real router with a real (sqlite) DB.

### CI profiles that gate

Two profiles, because the suite spans two cost tiers:

- **`security-fast`** (runs on every PR, blocking). The whole `umbral-security-tests` crate against SQLite: `cargo test -p umbral-security-tests`. This is the un-skippable gate. A red here blocks merge, and because the crate is security-only, a red here *means* a security property regressed, which is the distinct signal the current undifferentiated `cargo test` cannot give.
- **`security-full`** (nightly + pre-release, blocking on the release branch). The same suite plus the `#[ignore]`d live-service scenarios against Postgres, Redis, and a MinIO S3, using the Testcontainers/compose matrix from gaps5 #93. This is where the RLS isolation tests (AUTH-3), the Postgres task double-claim guard (BROKEN-1, now `FOR UPDATE SKIP LOCKED`), the Redis-backed session store, and the S3 media-gate scenarios actually exercise the production backends. It is also a hard gate on the pre-release checklist in #99 below.

A dedicated `.github/workflows/security-tests.yml` runs `security-fast` on PR and push, and `security-full` on schedule and on the release branch. The workflow name and the crate name make the signal legible on the PR status list: "security-tests" going red is its own line, not buried in "test".

### Why a separate crate rather than a `#[cfg]` feature or a test tag

- A crate gives the suite a name that shows up in CI, in the workspace graph, and in `SECURITY.md` ("the security regression suite is `umbral-security-tests`"). A cargo test-name filter (`cargo test security_`) would be fragile (any rename silently drops a test from the gate) and invisible in tooling.
- A crate can depend on the *facade plus every plugin* at once, which is exactly what the cross-plugin attack scenarios need and what no single plugin's own `tests/` can express without a dependency it should not have.
- `publish = false` keeps it out of the lockstep release and off crates.io; it is infrastructure, not surface.
- It satisfies the STABILITY.md 1.0 gate directly: gate #3 there is "secure-by-default is on and covered by an always-on security regression suite (gaps5 #98)". A named crate is the thing that gate points at.

### What this is not

It is not a penetration-test replacement, a fuzzer (that is gaps5 #95, `cargo-fuzz` on parsers/planners), or a chaos harness (gaps5 #94). It is a regression suite: every entry pins a property that was audited or fixed, so it can never silently un-fix. New audit findings land here as a failing test first, then the fix.

---

## #99 (tf #312): release automation and provenance pipeline

### Problem

Publishing today is a manual `release-plz` dispatch with `cargo audit` as the only gate. There is no license/ban check, no SBOM, no signature or build attestation on the artifacts, no automated changelog validation, and no confirmation that the docs for the shipped version went live. An adopting organization cannot verify what it is installing or trace it back to a build. `SECURITY.md` already promises this pipeline as a roadmap; this makes it concrete.

### Design: a gated release pipeline in stages

The release stays a manually dispatched, "slow and silent" event (the existing `release-plz.yml` posture is deliberate and kept). What changes is that the dispatch runs through an ordered set of gates, and produces provenance artifacts. Every gate is a hard stop: a failure aborts the release before anything is published.

**Stage 0 - preflight gates (must pass before publish).**

1. **Full workspace verify.** `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo build`, `cargo test` across the whole workspace, exactly the pre-commit contract from CLAUDE.md, run once more on the release ref.
2. **Security suite.** The `security-full` profile from #98 (SQLite + live Postgres/Redis/MinIO). A security regression blocks the release, not just the merge.
3. **Advisory + license/ban gate.** `cargo audit` (already wired) plus a new `cargo deny check` covering advisories, licenses (allow-list of acceptable SPDX ids), banned/duplicate crates, and unknown sources. Config lands in a committed `deny.toml`; like `.cargo/audit.toml`, exceptions are justified inline.
4. **Semver check.** Already on via release-plz `semver_check`; the pipeline surfaces its verdict explicitly. A detected breaking change on the Stable tier (per STABILITY.md) must correspond to a MINOR bump and a migration note, or the release is held.
5. **Changelog validation.** A `scripts/validate-changelog` step asserts, for the version being cut: (a) every publishable crate has a changelog entry generated from conventional commits (release-plz produces these; the check is that none is empty); (b) any commit that release-plz's semver check flagged as breaking has a corresponding migration note in the changelog (STABILITY.md's Stable-tier promise); (c) the em-dash / en-dash linter the repo already enforces on changelog text passes (normalize to hyphens). A missing or empty changelog for any crate aborts.

**Stage 1 - publish (unchanged mechanism).** `release-plz release` publishes every crate in dependency order under the one lockstep version, cuts the per-crate git tags, and creates the GitHub release (all existing behavior).

**Stage 2 - provenance artifacts (new, produced from the published state).**

6. **SBOM.** `cargo cyclonedx --all --format json` generates a CycloneDX SBOM for the workspace, attached to the GitHub release as `umbral-<version>.cdx.json`. This is the "what is in this release" bill of materials `SECURITY.md` promised; it is also the input an adopter's own `cargo audit`/`grype`/Dependency-Track can consume later against newly filed advisories.
7. **Signing and attestation.** Two layers, both keyless where possible:
   - **Artifact signing** of the SBOM and the release tarball via Sigstore `cosign sign-blob` (keyless, GitHub OIDC identity), producing a `.sig` + certificate attached to the release. No long-lived signing key to manage or leak.
   - **Build provenance** via the SLSA GitHub provenance generator (the `actions/attest-build-provenance` action), which records the builder identity, the source commit, and the build parameters as an in-toto attestation on the GitHub release. This is the "how and from what was this built" record; it is what lets an adopter confirm a crate came from this repo's CI and not a re-upload.
8. **Docs-publish verification.** After `deploy-docs.yml` runs for the new version, a verification step fetches the published docs site and asserts the version selector lists the new version and a known new page resolves (HTTP 200, expected title). A release whose docs did not actually publish is flagged; the release notes link to the exact docs version.

**Stage 3 - post-release record.** The pipeline appends a one-line entry to a `RELEASES.md` provenance ledger (version, date, commit SHA, SBOM digest, cosign cert identity, provenance attestation URL) so the chain from a published crate back to its build is auditable without digging through Actions logs.

### The human-readable checklist

The automation is backed by a committed `docs/release-checklist.md` that names each gate in order, so a maintainer cutting a release (or an auditor reviewing the process) reads the same sequence the CI enforces. It doubles as the SLSA-style "release process" document adopters ask for. The checklist explicitly records the two deliberate manual decisions the automation cannot make: (a) confirming the version bump matches intent (patch vs minor per STABILITY.md), and (b) dispatching the workflow (releases stay pull, not push).

### Sequencing and dependencies

- Stage 0.3 (`cargo deny`) and 0.5 (changelog validation) are independent and land first; they are pure additions to the existing PR CI and give value before any release.
- Stage 2 (SBOM, signing, provenance) depends on nothing but the publish step and can land next.
- The docs-publish verification (2.8) depends on `deploy-docs.yml` exposing a stable version-selector endpoint to probe.
- The whole pipeline extends, and does not replace, `release-plz.yml` and `audit.yml`. Nothing here changes the lockstep versioning contract in `release-plz.toml`.

### Why these tools

- **cargo-deny over hand-rolled license grep.** It is the standard Rust supply-chain gate, config-driven, and covers advisories + licenses + bans + sources in one pass; it complements (does not duplicate) cargo-audit, which is advisory-only.
- **CycloneDX over SPDX for the SBOM.** `cargo-cyclonedx` is the maintained first-party-ish generator for Cargo, emits the format Dependency-Track and most scanners ingest, and needs no external service.
- **Sigstore/cosign keyless over a maintained GPG key.** Keyless signing binds the signature to the CI's OIDC identity with no key custody, which removes the single worst supply-chain liability (a leaked signing key) and matches how the wider ecosystem is moving.
- **SLSA provenance via GitHub's attestation action** because it is native to the existing GitHub Actions release path and produces a verifiable in-toto statement without standing up separate infrastructure.

### What this closes and what stays open

Closes the four concrete `SECURITY.md` roadmap bullets: cargo-audit + cargo-deny in CI, SBOM per release, release signing / provenance. The fifth roadmap bullet, the SOC 2 / GDPR **controls matrix**, is a documentation deliverable that consumes this pipeline's outputs (SBOM, signing, advisory scan results) as evidence; it is tracked separately and is out of scope here. Consumer-facing verification instructions ("how to verify an umbral release's signature and SBOM") ship as a docs page alongside the pipeline, per the "ship a feature, ship its doc page" rule.

## See also

- `SECURITY.md` (the roadmap these two items make concrete) and `STABILITY.md` (the 1.0 gates #98 satisfies).
- `planning/review/` (the audits that seeded #98's regressions), especially `README.md`, `broken-features.md`, `security-web-surface.md`, `security-auth-session.md`, `security-orm-sql.md`.
- `docs/maturity-matrix.md` (gaps5 #100): the per-subsystem status view these two guarantees underpin.
- `release-plz.toml`, `.github/workflows/audit.yml`, `.github/workflows/release-plz.yml`, `.github/workflows/deploy-docs.yml` (the existing plumbing #99 extends).
- `docs/decisions/2026-08-08-enterprise-preset-design.md` (the secure-by-default wiring #98's harness builds on).
