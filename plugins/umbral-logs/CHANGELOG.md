# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-logs-v0.0.4...umbral-logs-v0.0.5) - 2026-07-05

### Added

- *(config)* trusted-proxy client-IP resolution (audit_2 H9)

### Fixed

- *(logs)* unforgeable user attribution + document min_status security gap
- *(logs)* bound the request-capture handle list to avoid OOM (audit_2 H13)

### Other

- cargo fmt across the workspace

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-logs-v0.0.1...umbral-logs-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
