# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-cli-v0.0.5...umbral-cli-v0.0.6) - 2026-07-07

### Added

- *(migrate)* squashmigrations — non-destructive history collapse (gaps2 #100)
- *(app)* AppBuilder::auto_migrate_on_serve() — serve-only migrate lifecycle (gaps3 #23)

### Fixed

- *(cli)* run project-free commands (maskkeygen) directly instead of forwarding
- *(cli)* scaffold a random per-project dev secret_key (audit_2 macros-cli#7, gaps3 #27)

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-cli-v0.0.4...umbral-cli-v0.0.5) - 2026-07-05

### Added

- *(cli)* refuse destructive migrations at apply without --allow-destructive (audit_2 core-migrate #6)
- *(migrate)* autodetect unique_together / composite-index changes (audit_2 core-migrate #10 follow-up)
- *(cli)* forward 'umbral <cmd>' to 'cargo run -- <cmd>'

### Fixed

- *(cli)* warn on maskkeygen private-key stdout (audit #4)
- *(scaffold)* harden generated project defaults (audit #1/#3/#5)

### Other

- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-cli-v0.0.1...umbral-cli-v0.0.2) - 2026-06-26

### Added

- *(cli)* scaffold projects with crates.io version deps, not git

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
- add per-crate READMEs for crates.io
