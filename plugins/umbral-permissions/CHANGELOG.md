# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-permissions-v0.0.4...umbral-permissions-v0.0.5) - 2026-07-05

### Added

- *(authz)* boot audit of ungated routes + permission-recording gated builders (audit_2 H19)

### Fixed

- *(permissions)* PK-agnostic perm layer + bounded perm fetch (audit_2 P4, P6)
- *(permissions)* deny deactivated accounts in the perm layer (audit_2 P3)

### Other

- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-permissions-v0.0.1...umbral-permissions-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- add per-crate READMEs for crates.io
