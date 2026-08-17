# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.12](https://github.com/dalmasonto/umbral/compare/umbral-playground-v0.0.11...umbral-playground-v0.0.12) - 2026-08-16

### Other

- *(readmes)* add the umbralrs.dev website link to every crate + plugin
- *(playground)* rebuild the frontend bundle
- *(brand)* wire the umbral favicon into docs, website, playground, scaffold

## [0.0.11](https://github.com/dalmasonto/umbral/compare/umbral-playground-v0.0.10...umbral-playground-v0.0.11) - 2026-08-02

### Fixed

- *(core)* slash-redirect probes 404s from MATCHED routes too (gaps4 #50)
- *(playground)* default mount moves off /api to /playground (gaps4 #43)
- *(playground)* schema tab shows real per-field metadata (gaps4 #34)

### Other

- *(playground)* document mount path and the /api trailing-slash gotcha

## [0.0.9](https://github.com/dalmasonto/umbral/compare/umbral-playground-v0.0.8...umbral-playground-v0.0.9) - 2026-07-14

### Other

- *(plugins)* verify the install path for all 22 plugins, end to end

## [0.0.3](https://github.com/dalmasonto/umbral/compare/umbral-playground-v0.0.2...umbral-playground-v0.0.3) - 2026-06-29

### Added

- *(rest)* make views([...]) read-only everywhere (405/OPTIONS/OpenAPI)

### Fixed

- *(playground)* hydrate the persisted operation's draft on page load
- *(playground)* stop the loaded draft clobbering params typed during hydration
- *(playground)* commit the in-progress header/param row on focus-out
- *(playground)* persist per-request headers + query params; send default headers

### Other

- Merge remote-tracking branch 'origin/main'

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-playground-v0.0.1...umbral-playground-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- drop Django framing from crate metadata and code
