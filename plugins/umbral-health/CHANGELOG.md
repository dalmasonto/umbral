# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.7](https://github.com/dalmasonto/umbral/compare/umbral-health-v0.0.6...umbral-health-v0.0.7) - 2026-07-12

### Added

- *(app)* drain readiness on shutdown for zero-downtime rollouts
- *(health)* gate readiness on pending migrations

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-health-v0.0.4...umbral-health-v0.0.5) - 2026-07-05

### Fixed

- *(health)* don't leak raw DB error into unauthenticated /ready body (audit_2 #3)

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-health-v0.0.1...umbral-health-v0.0.2) - 2026-06-26

### Other

- add health + oauth plugin pages, repoint their READMEs
- link READMEs to the documentation site
- remove Django framing across the codebase
