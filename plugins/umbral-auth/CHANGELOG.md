# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-auth-v0.0.4...umbral-auth-v0.0.5) - 2026-07-05

### Added

- *(signals)* #[umbral(signal_skip)] strips fields from signal payloads (audit_2 core-app-config #10)
- *(auth)* bound argon2 concurrency to prevent hashing-flood OOM (audit_2 plugin-auth #4)
- *(config)* trusted-proxy client-IP resolution (audit_2 H9)
- *(orm)* #[umbral(privileged)] — default-deny mass assignment on write paths (audit_2 H3)

### Fixed

- *(auth)* register JSON auth routes at both slash forms (gaps3 #11)
- *(auth)* close enumeration timing oracle, error leaks, mailer secret print (audit_2)

### Other

- cargo fmt across the workspace

## [0.0.3](https://github.com/dalmasonto/umbral/compare/umbral-auth-v0.0.2...umbral-auth-v0.0.3) - 2026-06-29

### Added

- *(auth)* give AuthMailer the email kind + params for per-type customization
- *(auth)* form-action auth endpoints (form in, 303 redirect out) via with_form_routes
- *(auth)* ship overridable Jinja templates for the auth pages
- *(auth)* opt-in require_verified_email (auto-send on register, block login)
- *(auth)* OpenAPI path items for verify/resend/forgot/reset
- *(auth)* JSON verify/resend/forgot/reset endpoints under the REST base path
- *(auth)* password forgot/reset core (token issue + reset with revoke)
- *(auth)* email-verification core flow (code issue + verify)
- *(auth)* expose reusable umbral_auth::logout used by both surfaces
- *(auth)* challenge generation, hashing, and lifecycle helpers
- *(auth)* pluggable AuthMailer seam with ConsoleMailer default
- *(auth)* add email_verified_at + AuthChallenge model

### Fixed

- *(auth)* throttle email actions + log reset revocation failures
- *(auth)* atomic email verification + brute-force/variant test hardening
- *(auth)* atomic server-side increment for AuthChallenge::bump_attempts
- *(auth)* silence dead_code on active_mailer; smoke-test ConsoleMailer

### Other

- Merge remote-tracking branch 'origin/main'
- *(auth)* correct status codes, revocation/timing claims, flash-session framing
- Revert "feat(auth): ship overridable Jinja templates for the auth pages"
- *(auth)* cover JSON password-reset + verified-resend; doc reset_url_base host trust

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-auth-v0.0.1...umbral-auth-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
- add per-crate READMEs for crates.io
