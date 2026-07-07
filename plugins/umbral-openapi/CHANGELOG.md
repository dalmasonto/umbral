# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-openapi-v0.0.5...umbral-openapi-v0.0.6) - 2026-07-07

### Added

- *(orm)* #[umbral(case_insensitive)] — DB-level case-insensitive columns (gaps3 #35)
- *(orm)* #[umbral(trim)] / #[umbral(lowercase)] field normalization (gaps3 #34)
- *(orm)* BEGIN IMMEDIATE for SQLite write transactions — the root-cause flake fix

### Fixed

- *(openapi)* add SRI to the pinned Swagger UI assets (audit_2 obs#9, gaps3 #27)

### Other

- *(workspace)* cargo fmt + save in-flight edits before gaps3 #28
- pin file-based SQLite test pools to max_connections(1) (the real flake fix)
- give raw SQLite test pools a busy_timeout to end SQLITE_BUSY flakes

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-openapi-v0.0.4...umbral-openapi-v0.0.5) - 2026-07-05

### Added

- *(orm)* #[umbral(privileged)] — default-deny mass assignment on write paths (audit_2 H3)

### Fixed

- *(openapi)* pin + make Swagger UI asset source configurable (audit_2 plugin-observability #9)
- *(openapi)* don't mount spec + Swagger UI in Prod by default (audit_2 H12)

## [0.0.3](https://github.com/dalmasonto/umbral/compare/umbral-openapi-v0.0.2...umbral-openapi-v0.0.3) - 2026-06-29

### Added

- *(rest)* make views([...]) read-only everywhere (405/OPTIONS/OpenAPI)

### Fixed

- *(rest)* list custom @actions in OpenAPI even without a declared schema

### Other

- Merge remote-tracking branch 'origin/main'

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-openapi-v0.0.1...umbral-openapi-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
- add per-crate READMEs for crates.io
