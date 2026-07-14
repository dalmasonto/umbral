# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.9](https://github.com/dalmasonto/umbral/compare/umbral-cache-v0.0.8...umbral-cache-v0.0.9) - 2026-07-14

### Other

- install snippets say `cargo add`, so they cannot go stale

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-cache-v0.0.5...umbral-cache-v0.0.6) - 2026-07-07

### Fixed

- *(cache)* bypass shared cache on Proxy-Authorization + Vary identity (audit_2 realtime#1, gaps3 #27)

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-cache-v0.0.4...umbral-cache-v0.0.5) - 2026-07-05

### Fixed

- *(cache)* bypass shared page cache for Authorization + private/no-cache (audit_2 H26)

### Other

- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-cache-v0.0.1...umbral-cache-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- add per-crate READMEs for crates.io
