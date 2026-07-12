# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.7](https://github.com/dalmasonto/umbral/compare/umbral-v0.0.6...umbral-v0.0.7) - 2026-07-12

### Added

- *(typegen)* #[derive(Dto)] — custom response types in the client (gaps3 #29.5)
- *(rest)* ResourceConfig::under — parent-scoped sub-resources (gaps3 #29.2)
- *(web)* Valid<T> + #[derive(Validate)] for request bodies (gaps3 #29.4)
- *(orm)* database views, regular and materialized (features #73)
- *(orm)* register_cleaner — custom per-field clean/validate hooks (features #83)
- *(orm)* AppBuilder::auto_models() — models register themselves (gaps3 #46)
- *(rest)* generate a typed TypeScript query client (umbral gen-client)
- *(app)* drain readiness on shutdown for zero-downtime rollouts
- *(health)* gate readiness on pending migrations
- *(orm)* generate TypeScript types from the model registry

### Fixed

- *(templates)* static() emitted &#x2f; instead of / — and it worked anyway (gaps3 #66)
- *(settings)* `umbral startproject` emitted a project that would not compile (gaps3 #64)
- *(permissions)* a UUID-keyed user was silently forbidden from everything (gaps3 #59)
- *(examples)* the scaffold generated an information leak into every new app (gaps3 #57)
- *(orm)* reject DST-ambiguous local times instead of shifting them
- *(app)* order plugins by the foreign keys their models declare

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-v0.0.5...umbral-v0.0.6) - 2026-07-07

### Added

- *(migrate)* squashmigrations — non-destructive history collapse (gaps2 #100)
- *(core)* ApiError — a handler-facing error with From<ORM error> + IntoResponse (gaps3 #15)

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-v0.0.4...umbral-v0.0.5) - 2026-07-05

### Added

- *(db)* alias-aware begin_for / transaction_on (audit_2 core-app-config #5)
- *(config)* trusted-proxy client-IP resolution (audit_2 H9)
- *(admin)* has_codename / require_codename permission check

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-v0.0.1...umbral-v0.0.2) - 2026-06-26

### Other

- remove Django framing across the codebase
- drop Django framing from crate metadata and code
- *(docs)* deploy the Specra docs site to GitHub Pages
- add per-crate READMEs for crates.io
