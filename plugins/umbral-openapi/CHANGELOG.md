# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
