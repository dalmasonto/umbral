# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.9](https://github.com/dalmasonto/umbral/compare/umbral-oauth-v0.0.8...umbral-oauth-v0.0.9) - 2026-07-14

### Other

- derive the test schema from the models in 166 suites (gaps3 #78)
- install snippets say `cargo add`, so they cannot go stale

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-oauth-v0.0.5...umbral-oauth-v0.0.6) - 2026-07-07

### Added

- *(auth)* log in with username OR email; real password hash for social accounts
- *(orm)* BEGIN IMMEDIATE for SQLite write transactions — the root-cause flake fix

### Fixed

- *(auth)* store + match usernames and emails case-insensitively (gaps3 #33)
- *(oauth)* atomic create-user + create-social (gaps3 #28 OAU-4)

### Other

- *(workspace)* cargo fmt + save in-flight edits before gaps3 #28
- pin file-based SQLite test pools to max_connections(1) (the real flake fix)
- give raw SQLite test pools a busy_timeout to end SQLITE_BUSY flakes

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-oauth-v0.0.4...umbral-oauth-v0.0.5) - 2026-07-05

### Fixed

- *(oauth)* unknown provider key returns 404, not 500 (gaps3 #12)
- *(oauth,tenants)* fail-closed tenant routing, error leak, email-verified allowlist (audit_2)

### Other

- cargo fmt across the workspace

## [0.0.3](https://github.com/dalmasonto/umbral/compare/umbral-oauth-v0.0.2...umbral-oauth-v0.0.3) - 2026-06-29

### Added

- *(auth)* add email_verified_at + AuthChallenge model

### Other

- Merge remote-tracking branch 'origin/main'

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-oauth-v0.0.1...umbral-oauth-v0.0.2) - 2026-06-26

### Other

- add health + oauth plugin pages, repoint their READMEs
- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
