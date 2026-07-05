# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
