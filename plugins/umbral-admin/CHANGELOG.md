# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-admin-v0.0.4...umbral-admin-v0.0.5) - 2026-07-05

### Added

- *(orm)* #[umbral(privileged)] — default-deny mass assignment on write paths (audit_2 H3)
- *(admin)* enforce per-widget permission on render + data endpoint
- *(admin)* custom views in the sidebar nav
- *(admin)* register, route, and render custom admin views
- *(admin)* has_codename / require_codename permission check
- *(admin)* AdminView builder for custom admin views
- *(admin)* unified card recipe with shadow-card elevation
- *(admin)* neutral pure-white / near-black palette

### Fixed

- *(admin)* gate sidebar Dashboard link's ?dashboard=1 on restore_last_path
- *(admin)* delete actually deletes — CSRF on bulk/JS deletes + fix single-delete URL
- *(admin)* escapejs for inline-handler XSS + per-model View gates (audit_2 H5,H6)
- *(admin)* mount custom views under /custom-views/ namespace
- *(admin)* validate custom-view paths, don't panic (gaps3 #7)
- *(admin)* filter dashboard widget catalog by permission (gaps3 #6)
- *(admin)* gate custom-view widget data by the view's permission
- *(admin)* responsive changelist header (no button overflow on mobile)
- *(admin)* refresh changelist on save-and-continue
- *(admin)* make create/edit sheet responsive on small screens
- *(admin)* keep long numbered pagination after HTMX swaps
- *(admin)* single Tailwind theme source (theme.json)

### Other

- *(admin)* rebuild admin.css asset bundle
- cargo fmt across the workspace
- *(admin)* batch per-widget permission checks (gaps3 #8)
- *(admin)* custom-view behavioral tests + docs
- *(admin)* extract widget grid into a shared macro
- *(admin)* relocate divider-token rationale; fix test doc typo
- *(admin)* rebuild compiled admin.css for the visual refresh

## [0.0.3](https://github.com/dalmasonto/umbral/compare/umbral-admin-v0.0.2...umbral-admin-v0.0.3) - 2026-06-29

### Added

- *(auth)* add email_verified_at + AuthChallenge model

### Other

- Merge remote-tracking branch 'origin/main'
- *(auth)* add email_verified_at to stale auth_user DDLs in sessions/admin/rest tests

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-admin-v0.0.1...umbral-admin-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
- add per-crate READMEs for crates.io
