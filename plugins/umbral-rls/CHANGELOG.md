# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.7](https://github.com/dalmasonto/umbral/compare/umbral-rls-v0.0.6...umbral-rls-v0.0.7) - 2026-07-12

### Added

- *(auth)* publish the session user id to the DB connection for RLS

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-rls-v0.0.5...umbral-rls-v0.0.6) - 2026-07-07

### Fixed

- *(rls)* drop policies no longer declared on reapply (gaps3 #28 R5)

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-rls-v0.0.4...umbral-rls-v0.0.5) - 2026-07-05

### Added

- *(db,rls)* per-request RLS GUC via pool hook, no cross-request leak (C2 pt.2)

### Fixed

- *(rls)* FORCE row-level security + fail closed on SQLite (audit_2 C2 pt.1)

### Other

- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-rls-v0.0.1...umbral-rls-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- add per-crate READMEs for crates.io
