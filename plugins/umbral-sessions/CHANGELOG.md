# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-sessions-v0.0.5...umbral-sessions-v0.0.6) - 2026-07-07

### Added

- *(admin)* warn on CSRF-exposing SameSite=None config (gaps3 #28 admin #5)
- *(orm)* BEGIN IMMEDIATE for SQLite write transactions — the root-cause flake fix

### Fixed

- *(sessions)* emit session cookie beside an unrelated (CSRF) cookie

### Other

- *(workspace)* cargo fmt + save in-flight edits before gaps3 #28
- pin file-based SQLite test pools to max_connections(1) (the real flake fix)
- give raw SQLite test pools a busy_timeout to end SQLITE_BUSY flakes

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-sessions-v0.0.4...umbral-sessions-v0.0.5) - 2026-07-05

### Added

- *(sessions)* configurable cookie SameSite policy (audit_2 plugin-sessions #7)
- *(sessions)* absolute session-age cap alongside sliding expiry (audit_2 plugin-sessions #5)

### Fixed

- *(sessions)* route out-of-request set_data through the installed store (audit_2 plugin-sessions #6)
- *(migrate,sessions)* single-column index flips emit AddIndex, not a PG-broken AlterColumn (audit_2 plugin-sessions #4)
- *(sessions)* revocation works on every store via SessionStore::destroy_user (audit_2 H7)
- *(sessions)* hard-fail Prod boot on empty CookieStore secret (audit_2 H8)

### Other

- cargo fmt across the workspace

## [0.0.3](https://github.com/dalmasonto/umbral/compare/umbral-sessions-v0.0.2...umbral-sessions-v0.0.3) - 2026-06-29

### Added

- *(sessions)* add revoke_user_sessions (log-out-everywhere primitive)

### Other

- Merge remote-tracking branch 'origin/main'
- *(auth)* add email_verified_at to stale auth_user DDLs in sessions/admin/rest tests

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-sessions-v0.0.1...umbral-sessions-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
- add per-crate READMEs for crates.io
