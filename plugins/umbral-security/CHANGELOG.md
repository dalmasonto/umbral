# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.7](https://github.com/dalmasonto/umbral/compare/umbral-security-v0.0.6...umbral-security-v0.0.7) - 2026-07-12

### Added

- *(security)* mark personalised responses no-store, private

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-security-v0.0.5...umbral-security-v0.0.6) - 2026-07-07

### Added

- *(security)* SecurityConfig::production_hardened() one-call prod preset (audit_2 S1)

### Fixed

- *(security)* resolve the CSRF secret per request, not at wrap_router (audit_2 S3, gaps3 #27)

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-security-v0.0.4...umbral-security-v0.0.5) - 2026-07-05

### Other

- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-security-v0.0.1...umbral-security-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
- add per-crate READMEs for crates.io
