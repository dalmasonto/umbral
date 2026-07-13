# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.8](https://github.com/dalmasonto/umbral/compare/umbral-storage-v0.0.7...umbral-storage-v0.0.8) - 2026-07-13

### Other

- clippy --fix across the plugins

## [0.0.7](https://github.com/dalmasonto/umbral/compare/umbral-storage-v0.0.6...umbral-storage-v0.0.7) - 2026-07-12

### Added

- *(storage)* built-in thumbnails behind the `images` feature (gaps3 #50)
- *(storage)* upload content-type allow-list (gaps3 #51)

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-storage-v0.0.5...umbral-storage-v0.0.6) - 2026-07-07

### Added

- *(orm)* BEGIN IMMEDIATE for SQLite write transactions — the root-cause flake fix

### Other

- *(workspace)* cargo fmt + save in-flight edits before gaps3 #28
- pin file-based SQLite test pools to max_connections(1) (the real flake fix)
- give raw SQLite test pools a busy_timeout to end SQLITE_BUSY flakes

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-storage-v0.0.4...umbral-storage-v0.0.5) - 2026-07-05

### Added

- *(storage)* access-control hook for media serving (audit_2 plugin-storage-tasks #3)
- *(storage)* bound media-processing concurrency (audit_2 plugin-storage-tasks #4)

### Fixed

- *(storage)* S3 active-content guard, upload cap, media symlink guard (audit_2 H25)

### Other

- *(deps)* drop EOL rustls/hyper via rust-s3 0.37, patch audit findings (audit_2 H24)
- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-storage-v0.0.1...umbral-storage-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- add per-crate READMEs for crates.io
