# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-signals-v0.0.5...umbral-signals-v0.0.6) - 2026-07-07

### Added

- *(orm)* BEGIN IMMEDIATE for SQLite write transactions — the root-cause flake fix

### Other

- *(workspace)* cargo fmt + save in-flight edits before gaps3 #28
- *(signals)* drop the stale m2m_changed "Deferred past v1" bullet (audit_2 obs#12, gaps3 #27)
- pin file-based SQLite test pools to max_connections(1) (the real flake fix)
- give raw SQLite test pools a busy_timeout to end SQLITE_BUSY flakes

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-signals-v0.0.4...umbral-signals-v0.0.5) - 2026-07-05

### Other

- *(signals)* correct rustdoc — bulk methods fire bulk signals (audit_2 #12)

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-signals-v0.0.1...umbral-signals-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- add per-crate READMEs for crates.io
