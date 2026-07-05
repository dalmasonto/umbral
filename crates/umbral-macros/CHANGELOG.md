# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-macros-v0.0.4...umbral-macros-v0.0.5) - 2026-07-05

### Added

- *(signals)* #[umbral(signal_skip)] strips fields from signal payloads (audit_2 core-app-config #10)
- *(orm)* #[umbral(privileged)] — default-deny mass assignment on write paths (audit_2 H3)

### Fixed

- *(macros)* parse Form FK into target PK type, not i64 (audit #8)
- *(orm)* seal Masked<T> on the dynamic write path (audit_2 C1)

### Other

- *(macros)* refresh task_macro trybuild stderr for current rustc
- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-macros-v0.0.1...umbral-macros-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- add per-crate READMEs for crates.io
