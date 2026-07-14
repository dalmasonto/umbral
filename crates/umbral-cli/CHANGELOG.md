# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.9](https://github.com/dalmasonto/umbral/compare/umbral-cli-v0.0.8...umbral-cli-v0.0.9) - 2026-07-14

### Other

- install snippets say `cargo add`, so they cannot go stale

## [0.0.8](https://github.com/dalmasonto/umbral/compare/umbral-cli-v0.0.7...umbral-cli-v0.0.8) - 2026-07-13

### Fixed

- *(docs)* four dead links, one of them in every scaffolded project

### Other

- clippy --fix across the plugins
- *(cli)* compile the scaffold's output; warn on a source-checkout scaffold

## [0.0.7](https://github.com/dalmasonto/umbral/compare/umbral-cli-v0.0.6...umbral-cli-v0.0.7) - 2026-07-12

### Added

- *(scaffold)* a real design — compiled Tailwind, the umbral palette, docs links
- *(orm)* database views, regular and materialized (features #73)
- *(rest)* generate a typed TypeScript query client (umbral gen-client)
- *(migrate)* emit help text as a Postgres column comment
- *(orm)* generate TypeScript types from the model registry

### Fixed

- *(settings)* `umbral startproject` emitted a project that would not compile (gaps3 #64)
- *(migrate)* a bad env prefix made `migrate` succeed against nothing (gaps3 #59/#60/#61)
- *(examples)* the scaffold generated an information leak into every new app (gaps3 #57)
- *(app)* fire on_ready when the app is up, not when it is built

### Other

- *(scaffold)* a new project talks about itself, not about the generator

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
