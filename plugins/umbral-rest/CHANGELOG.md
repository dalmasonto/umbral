# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-rest-v0.0.4...umbral-rest-v0.0.5) - 2026-07-05

### Added

- *(config)* trusted-proxy client-IP resolution (audit_2 H9)
- *(rest)* object-level row scoping on built-in CRUD (audit_2 H1/P2)
- *(orm)* #[umbral(privileged)] — default-deny mass assignment on write paths (audit_2 H3)
- *(rest)* recursive N-level writable nested writes (gaps3 #9, #10)

### Fixed

- *(rest)* warn when an IP throttle runs without a trusted proxy (audit_2 H9 follow-up)
- *(rest)* boot warnings + block-list gates + no error-string leaks (audit_2)
- *(rest)* enforce hidden-strip, child perms, and a node cap on nested writes

## [0.0.3](https://github.com/dalmasonto/umbral/compare/umbral-rest-v0.0.2...umbral-rest-v0.0.3) - 2026-06-29

### Added

- *(rest)* publish base path into umbral::web::api_base at build
- *(rest)* make views([...]) read-only everywhere (405/OPTIONS/OpenAPI)

### Fixed

- *(rest)* list custom @actions in OpenAPI even without a declared schema

### Other

- Merge remote-tracking branch 'origin/main'

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-rest-v0.0.1...umbral-rest-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
- add per-crate READMEs for crates.io
