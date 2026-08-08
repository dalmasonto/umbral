# Security policy

Status: draft (planning/gaps5.md #20). Pairs with `STABILITY.md` and `docs/decisions/2026-08-08-enterprise-preset-design.md`.

## Reporting a vulnerability

Please report security issues privately. Do not open a public issue for a vulnerability.

- Preferred: GitHub private vulnerability reporting on the repository (the Security tab, "Report a vulnerability"). This opens a private advisory with the maintainers.
- We aim to acknowledge a report within a few working days and to agree on a disclosure timeline with the reporter.

Coordinated disclosure is expected: we will credit reporters (unless they prefer otherwise) and publish an advisory once a fix is available.

## Supported versions

umbral is pre-1.0. Security fixes land against the latest published minor (currently the 0.0.x line) and are released as a patch. See `STABILITY.md` for the version and support model. All `umbral-*` crates share one lockstep version, so "fixed in 0.0.N" applies across the whole framework.

## Secure-by-default posture

A freshly scaffolded umbral app boots with these on by default (verified 2026-08-08):

- CSRF protection, including signed / session-bound CSRF tokens.
- Security headers: `X-Content-Type-Options: nosniff` and `X-Frame-Options: DENY`.
- Argon2 password hashing (auth).
- Template autoescaping (minijinja).
- Always-parameterized SQL through the ORM.
- Secure session cookies (marked `Secure` under the production environment), with sensitive-header redaction and a private cache for authenticated responses.

HSTS and a Content-Security-Policy are intentionally opt-in, because a wrong value breaks local development or a CDN-using app. They are turned on by `EnterprisePreset` (gaps5 #3) and documented in the deployment reference (gaps5 #4).

## Supply chain and provenance (roadmap)

These are planned and tracked, not yet all in place:

- `cargo-audit` and `cargo-deny` run in CI (advisory and license/ban checks).
- SBOM generation on each release.
- Release signing and build provenance (SLSA-style).
- A controls matrix mapping umbral's guarantees to common frameworks (SOC 2, GDPR) for adopter due diligence.

## Scope

This policy covers the umbral framework crates and the built-in `umbral-*` plugins. Applications built with umbral are the responsibility of their authors; the `EnterprisePreset` and its production system checks exist to make the safe configuration the default one.
