# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.7](https://github.com/dalmasonto/umbral/compare/umbral-tasks-v0.0.6...umbral-tasks-v0.0.7) - 2026-07-12

### Added

- *(tasks)* admin model for the schedules (gaps3 #49)
- *(tasks)* type-safe enqueue — #[task] generates a typed handle (gaps3 #48)

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-tasks-v0.0.5...umbral-tasks-v0.0.6) - 2026-07-07

### Added

- *(orm)* BEGIN IMMEDIATE for SQLite write transactions — the root-cause flake fix

### Other

- *(workspace)* cargo fmt + save in-flight edits before gaps3 #28
- pin file-based SQLite test pools to max_connections(1) (the real flake fix)
- give raw SQLite test pools a busy_timeout to end SQLITE_BUSY flakes

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-tasks-v0.0.4...umbral-tasks-v0.0.5) - 2026-07-05

### Added

- *(orm,tasks)* FOR UPDATE SKIP LOCKED claim primitive (audit_2 plugin-storage-tasks #6)

### Fixed

- *(storage)* S3 active-content guard, upload cap, media symlink guard (audit_2 H25)

### Other

- *(tasks)* composite claim index on TaskRow (status, run_at) (audit_2 plugin-storage-tasks #5)
- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-tasks-v0.0.1...umbral-tasks-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
- add per-crate READMEs for crates.io
