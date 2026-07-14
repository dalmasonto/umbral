# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.9](https://github.com/dalmasonto/umbral/compare/umbral-logs-v0.0.8...umbral-logs-v0.0.9) - 2026-07-14

### Other

- derive the test schema from the models in 166 suites (gaps3 #78)
- install snippets say `cargo add`, so they cannot go stale

## [0.0.7](https://github.com/dalmasonto/umbral/compare/umbral-logs-v0.0.6...umbral-logs-v0.0.7) - 2026-07-12

### Fixed

- *(migrate)* a bad env prefix made `migrate` succeed against nothing (gaps3 #59/#60/#61)

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-logs-v0.0.5...umbral-logs-v0.0.6) - 2026-07-07

### Added

- *(orm)* BEGIN IMMEDIATE for SQLite write transactions — the root-cause flake fix

### Other

- *(workspace)* cargo fmt + save in-flight edits before gaps3 #28
- pin file-based SQLite test pools to max_connections(1) (the real flake fix)
- give raw SQLite test pools a busy_timeout to end SQLITE_BUSY flakes

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-logs-v0.0.4...umbral-logs-v0.0.5) - 2026-07-05

### Added

- *(config)* trusted-proxy client-IP resolution (audit_2 H9)

### Fixed

- *(logs)* unforgeable user attribution + document min_status security gap
- *(logs)* bound the request-capture handle list to avoid OOM (audit_2 H13)

### Other

- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-logs-v0.0.1...umbral-logs-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
