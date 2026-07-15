# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.10](https://github.com/dalmasonto/umbral/compare/umbral-realtime-v0.0.9...umbral-realtime-v0.0.10) - 2026-07-15

### Fixed

- *(auth,cache,realtime)* coalesce writes, cap page cache, bound broker

## [0.0.9](https://github.com/dalmasonto/umbral/compare/umbral-realtime-v0.0.8...umbral-realtime-v0.0.9) - 2026-07-14

### Other

- install snippets say `cargo add`, so they cannot go stale

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-realtime-v0.0.5...umbral-realtime-v0.0.6) - 2026-07-07

### Added

- *(realtime)* safe-by-default publish authz seam (gaps3 #28 realtime #2)

### Other

- *(realtime)* send presence:sync only to the joining conn (gaps3 #28 realtime #5)

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-realtime-v0.0.4...umbral-realtime-v0.0.5) - 2026-07-05

### Added

- *(realtime)* can_send policy hook for inbound message authz (audit_2 realtime #2)

### Fixed

- *(realtime)* default connection cap + per-connection message-rate cap (audit_2 realtime #4)
- *(realtime,email)* redact Redis URL, default WS message cap, redacting Debug (audit_2)

### Other

- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-realtime-v0.0.1...umbral-realtime-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
