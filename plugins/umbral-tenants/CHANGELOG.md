# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.7](https://github.com/dalmasonto/umbral/compare/umbral-tenants-v0.0.6...umbral-tenants-v0.0.7) - 2026-07-12

### Added

- *(orm)* database views, regular and materialized (features #73)

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-tenants-v0.0.5...umbral-tenants-v0.0.6) - 2026-07-07

### Added

- *(migrate)* squashmigrations — non-destructive history collapse (gaps2 #100)

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-tenants-v0.0.4...umbral-tenants-v0.0.5) - 2026-07-05

### Added

- *(tenants)* bind the resolved tenant to the caller via TenantMembership (audit_2 C3)

### Fixed

- *(oauth,tenants)* fail-closed tenant routing, error leak, email-verified allowlist (audit_2)

### Other

- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-tenants-v0.0.1...umbral-tenants-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
