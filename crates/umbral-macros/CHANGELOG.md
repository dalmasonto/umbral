# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.7](https://github.com/dalmasonto/umbral/compare/umbral-macros-v0.0.6...umbral-macros-v0.0.7) - 2026-07-12

### Added

- *(typegen)* #[derive(Dto)] — custom response types in the client (gaps3 #29.5)
- *(web)* Valid<T> + #[derive(Validate)] for request bodies (gaps3 #29.4)
- *(orm)* database views, regular and materialized (features #73)
- *(orm)* AppBuilder::auto_models() — models register themselves (gaps3 #46)
- *(tasks)* type-safe enqueue — #[task] generates a typed handle (gaps3 #48)
- *(orm)* model-level audit trail — #[umbral(audited)] (gaps3 #54)
- *(orm)* auto_user_add / auto_user — stamp who wrote the row (gaps3 #55)

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-macros-v0.0.5...umbral-macros-v0.0.6) - 2026-07-07

### Added

- *(orm)* #[umbral(case_insensitive)] — DB-level case-insensitive columns (gaps3 #35)
- *(orm)* #[umbral(trim)] / #[umbral(lowercase)] field normalization (gaps3 #34)

### Fixed

- *(macros)* Choices decodes from VARCHAR columns on Postgres

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-macros-v0.0.4...umbral-macros-v0.0.5) - 2026-07-05

### Added

- *(signals)* #[umbral(signal_skip)] strips fields from signal payloads (audit_2 core-app-config #10)
- *(orm)* #[umbral(privileged)] — default-deny mass assignment on write paths (audit_2 H3)

### Fixed

- *(macros)* parse Form FK into target PK type, not i64 (audit #8)
- *(orm)* seal Masked<T> on the dynamic write path (audit_2 C1)

### Other

- *(macros)* refresh task_macro trybuild stderr for current rustc
- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-macros-v0.0.1...umbral-macros-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- add per-crate READMEs for crates.io
