# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.7](https://github.com/dalmasonto/umbral/compare/umbral-core-v0.0.6...umbral-core-v0.0.7) - 2026-07-12

### Added

- *(typegen)* #[derive(Dto)] — custom response types in the client (gaps3 #29.5)
- *(rest)* ResourceConfig::under — parent-scoped sub-resources (gaps3 #29.2)
- *(web)* Valid<T> + #[derive(Validate)] for request bodies (gaps3 #29.4)
- *(orm)* database views, regular and materialized (features #73)
- *(orm)* register_cleaner — custom per-field clean/validate hooks (features #83)
- *(orm)* order by an annotation, and get annotation rows typed (gaps3 #29)
- *(orm)* AppBuilder::auto_models() — models register themselves (gaps3 #46)
- *(storage)* built-in thumbnails behind the `images` feature (gaps3 #50)
- *(storage)* upload content-type allow-list (gaps3 #51)
- *(orm)* model-level audit trail — #[umbral(audited)] (gaps3 #54)
- *(orm)* auto_user_add / auto_user — stamp who wrote the row (gaps3 #55)
- *(rest)* generate a typed TypeScript query client (umbral gen-client)
- *(app)* drain readiness on shutdown for zero-downtime rollouts
- *(health)* gate readiness on pending migrations
- *(migrate)* emit help text as a Postgres column comment
- *(orm)* generate TypeScript types from the model registry

### Fixed

- *(website)* umbral is a WEB framework, and the version badge is now data (gaps3 #69)
- *(templates)* static() emitted &#x2f; instead of / — and it worked anyway (gaps3 #66)
- *(settings)* `umbral startproject` emitted a project that would not compile (gaps3 #64)
- *(website)* stop handing raw database errors to visitors (gaps3 #58, #62)
- *(migrate)* a bad env prefix made `migrate` succeed against nothing (gaps3 #59/#60/#61)
- *(permissions)* a UUID-keyed user was silently forbidden from everything (gaps3 #59)
- *(examples)* the scaffold generated an information leak into every new app (gaps3 #57)
- *(orm)* filter_eq_string fails CLOSED — a bad id no longer deletes the table (gaps3 #56)
- *(orm)* #[umbral(trim, lowercase)] applies on every write path
- *(orm)* create() and delete() now emit per-row signals (gaps3 #29)
- *(orm)* soft delete now cascades (gaps3 #53)
- *(orm)* reject DST-ambiguous local times instead of shifting them
- *(app)* fire on_ready when the app is up, not when it is built
- *(app)* order plugins by the foreign keys their models declare

### Other

- *(orm)* one implicit-filter seam per path, before tenant scoping

## [0.0.6](https://github.com/dalmasonto/umbral/compare/umbral-core-v0.0.5...umbral-core-v0.0.6) - 2026-07-07

### Added

- *(orm)* #[umbral(case_insensitive)] — DB-level case-insensitive columns (gaps3 #35)
- *(orm)* #[umbral(trim)] / #[umbral(lowercase)] field normalization (gaps3 #34)
- *(authz)* deny_ungated_mutations() makes the H19 audit a build error (gaps3 #28 P1)
- *(migrate)* rename M2M junction tables on a parent-model rename (gaps.md #93)
- *(orm)* Model::table_name() accessor for the SQL table name
- *(migrate)* squashmigrations — non-destructive history collapse (gaps2 #100)
- *(migrate)* a choices-only column delta no longer rebuilds the table on SQLite (gaps3 #24)
- *(app)* AppBuilder::auto_migrate_on_serve() — serve-only migrate lifecycle (gaps3 #23)
- *(core)* ApiError — a handler-facing error with From<ORM error> + IntoResponse (gaps3 #15)
- *(orm)* BEGIN IMMEDIATE for SQLite write transactions — the root-cause flake fix
- *(auth)* Identity::user_pk::<T>() — typed access to the stringified PK (gaps3 #17)

### Fixed

- *(oauth)* atomic create-user + create-social (gaps3 #28 OAU-4)
- *(macros)* Choices decodes from VARCHAR columns on Postgres
- *(web)* guard open-redirect // paths + skip escaping symlinks in collectstatic (audit_2 core-web #6/#7, gaps3 #27)
- *(templates)* render_str HTML-autoescapes by default (audit_2 core-templates-forms #3)

### Other

- *(workspace)* cargo fmt + save in-flight edits before gaps3 #28
- *(migrate)* engine-driven regression for AlterColumn on inbound-FK hub + data (gaps3 #30)
- pin file-based SQLite test pools to max_connections(1) (the real flake fix)
- give raw SQLite test pools a busy_timeout to end SQLITE_BUSY flakes

## [0.0.5](https://github.com/dalmasonto/umbral/compare/umbral-core-v0.0.4...umbral-core-v0.0.5) - 2026-07-05

### Added

- *(orm,tasks)* FOR UPDATE SKIP LOCKED claim primitive (audit_2 plugin-storage-tasks #6)
- *(migrate)* Postgres advisory lock serializes concurrent migrators (audit_2 core-migrate #7)
- *(signals)* #[umbral(signal_skip)] strips fields from signal payloads (audit_2 core-app-config #10)
- *(db)* alias-aware begin_for / transaction_on (audit_2 core-app-config #5)
- *(migrate)* autodetect unique_together / composite-index changes (audit_2 core-migrate #10 follow-up)
- *(db)* open settings.databases pools at build via lazy connect (audit_2 H17)
- *(config)* trusted-proxy client-IP resolution (audit_2 H9)
- *(settings)* warn on misspelled UMBRAL_ keys (audit_2 core-app-config #16)
- *(app)* graceful shutdown on SIGTERM/SIGINT + pool drain (audit_2 core-app-config #13)
- *(authz)* boot audit of ungated routes + permission-recording gated builders (audit_2 H19)
- *(config)* default Environment to Prod in release builds (audit_2 H14)
- *(orm)* #[umbral(privileged)] — default-deny mass assignment on write paths (audit_2 H3)
- *(web)* ship minimal hardening headers by default (audit_2 H10)
- *(db,rls)* per-request RLS GUC via pool hook, no cross-request leak (C2 pt.2)
- *(web)* default request body-size limit + timeout, enforce multipart cap
- *(check)* warn on SQLite-in-Prod and wildcard allowed_hosts

### Fixed

- *(migrate)* backfill NULLs when tightening a column to NOT NULL (audit_2 core-migrate #5)
- *(signals)* run handlers outside the registry lock (audit_2 core-app-config #8)
- *(migrate,sessions)* single-column index flips emit AddIndex, not a PG-broken AlterColumn (audit_2 plugin-sessions #4)
- *(migrate)* SQLite AlterColumn dance preserves indexes + unique_together (audit_2 core-migrate #10)
- *(signals)* bound async subscribers with a timeout on the write path (audit_2 observability #10)
- *(migrate)* fail closed on an ambiguous column-shape rename (audit_2 H23)
- *(migrate)* SQLite combined alter+add/drop applies (audit_2 H21)
- *(db)* honor UMBRAL_DB_* on the default pool + warn on dead settings.databases (audit_2 H16/H17)
- *(migrate)* SQLite AlterColumn on a table with inbound FKs (gaps3 #13)
- *(orm)* update_or_create fires per-row post_save on both branches (gaps3 #14)
- *(hosts)* fall back to :authority when the Host header is absent
- *(ratelimit)* bound the key map with a periodic global sweep
- *(orm)* atomic dynamic pool writes + real rows_affected
- *(backup)* FK-ordered transactional restore with PG sequence reset
- *(migrate)* correct FK target PK + escape raw DDL identifiers
- *(timezone)* dedupe the unknown-tz warning to one line per name
- *(db)* warn when a plugin's router install loses to an existing one
- *(settings)* redact secrets in Debug + case-insensitive Environment
- *(signals)* log serde errors instead of silently dropping signals
- *(templates)* close reflected-XSS + info-leak in core error pages/forms
- *(core)* hard-fail Prod boot on a too-short secret_key (audit_2 H15)
- *(orm)* seal Masked<T> on the dynamic write path (audit_2 C1)

### Other

- *(orm)* filter_sql injection contract + masked at-rest note
- *(plugin)* correct static_files conflict + Settings.databases claims
- *(db)* correct stale "Postgres arrives in Phase 2" panic/doc text
- cargo fmt across the workspace

## [0.0.3](https://github.com/dalmasonto/umbral/compare/umbral-core-v0.0.2...umbral-core-v0.0.3) - 2026-06-29

### Added

- *(core)* add umbral::web::api_base ambient for cross-plugin base-path discovery

### Other

- Merge remote-tracking branch 'origin/main'

## [0.0.2](https://github.com/dalmasonto/umbral/compare/umbral-core-v0.0.1...umbral-core-v0.0.2) - 2026-06-26

### Other

- link READMEs to the documentation site
- remove Django framing across the codebase
- add per-crate READMEs for crates.io
