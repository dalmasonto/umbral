# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.10](https://github.com/dalmasonto/umbral/compare/umbral-graphql-v0.0.9...umbral-graphql-v0.0.10) - 2026-07-15

### Added

- *(graphql)* row-level mutation scope via owned_by (gaps4 #9)

### Fixed

- *(graphql)* window reverse-FK child lists per parent (gaps4 #13)
- *(graphql)* carry per-request context across SSE and WebSocket (gaps4 #12)
- *(graphql)* apply a default query depth/complexity budget

### Other

- *(graphql)* rustfmt the gaps4 #12/#13 additions

## [0.0.9](https://github.com/dalmasonto/umbral/compare/umbral-graphql-v0.0.8...umbral-graphql-v0.0.9) - 2026-07-14

### Fixed

- *(rest,graphql)* `private` is a read policy — it no longer blocks writes

### Other

- derive the test schema from the models in 166 suites (gaps3 #78)
- install snippets say `cargo add`, so they cannot go stale
- *(graphql)* add the missing README and plugin docs page

## [0.0.8](https://github.com/dalmasonto/umbral/compare/umbral-graphql-v0.0.7...umbral-graphql-v0.0.8) - 2026-07-13

### Added

- *(graphql)* allow_private_if — the same unlock, one honest schema
