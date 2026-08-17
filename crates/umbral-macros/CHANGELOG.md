# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.12](https://github.com/dalmasonto/umbral/compare/umbral-macros-v0.0.11...umbral-macros-v0.0.12) - 2026-08-16

### Added

- *(orm)* add NaiveDateTime field type (TIMESTAMP without time zone)
- *(orm)* honor #[sqlx(rename)] as a field's column name
- *(orm)* #[umbral(precision, scale)] for numeric(N, M) decimals
- *(orm)* PostGIS geometry/geography spatial columns (feature-gated)
- *(orm)* arbitrary-precision BigDecimal + sub-second Time fix + round-trip sweep
- *(orm)* reusable model bases via #[derive(ModelBase)] + #[umbral(flatten)]
- *(orm)* #[umbral(auto_uuid)] — generate a public v4 UUID on create

### Fixed

- *(macros)* match serde's kebab rename for edge-case underscores

### Other

- *(readmes)* add the umbralrs.dev website link to every crate + plugin
- *(worktree-agent-af5489d741ac3423e)* review_3 dedup batch
- *(macros)* correct Validate/Model attribute-vocabulary claim
- *(macros)* route inline PascalCase through to_pascal_case
- *(macros)* lift OneToOne→unique-FK rewrite to one classifier

## [0.0.11](https://github.com/dalmasonto/umbral/compare/umbral-macros-v0.0.10...umbral-macros-v0.0.11) - 2026-08-02

### Added

- *(plugin-contract)* autodiscovery for task handlers + plugin models (gaps4 #40)
- *(auth)* validate email format at every entry point (gaps4 #35)
- *(orm)* choices columns filter by the enum variant (gaps4 #39)

## [0.0.9](https://github.com/dalmasonto/umbral/compare/umbral-macros-v0.0.8...umbral-macros-v0.0.9) - 2026-07-14

### Other

- finish the schema conversion — 205 suites derive from the models (gaps3 #78)

## [0.0.8](https://github.com/dalmasonto/umbral/compare/umbral-macros-v0.0.7...umbral-macros-v0.0.8) - 2026-07-13

### Added

- *(orm)* private/secret field tiers, enforced in the ORM

### Other

- clippy --fix the mechanical warnings in core + macros

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
