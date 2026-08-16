# Changelog

All notable changes to umbral are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the project is pre-1.0 (`0.x`), a bump in the **patch** field may carry
breaking changes; the release notes call them out under **Changed**, **Deprecated**,
and **Removed**.

This is the workspace-level changelog. Per-crate, per-version detail (as generated
by release-plz from the commit history) lives in each crate's own `CHANGELOG.md`
under `crates/*` and `plugins/*`.

## [Unreleased]

## [0.0.12] - 2026-08-17

The database-porting release: a full `inspectdb` → `migrate` → `transferdata`
pipeline that introspects an existing database into umbral models and copies the
data across, verified end to end against a real 32-table Prisma schema.

### Added

- **`inspectdb` — introspect an existing database into models + an initial migration.**
  Accepts a source-database argument; recovers foreign keys, single-column unique
  and index constraints, composite (`unique_together` / `indexes`) groups, constant
  column defaults, and `auto_now` / `auto_now_add` timestamps. Generated models are
  guaranteed to compile (serde derives, non-`id` primary keys, Rust-keyword columns).
- **`inspectdb` framework awareness** — `--framework django|rails|laravel|prisma`
  undoes each ORM's column conventions (FK `_id` / camelCase shedding, snake-casing),
  folds join tables into `M2M<T>` fields (including Prisma's implicit `_AToB`), strips
  the app prefix from struct names (`--with-table-names`), and externalizes Django's
  `auth_user` onto umbral-auth's `AuthUser`.
- **`inspectdb` type coverage** — native Postgres enums recover as `#[derive(Choices)]`
  types; PostGIS geometry/geography columns recover their subtype + SRID; `numeric(N,M)`,
  unsigned integers, and network types map to the right Rust types.
- **`transferdata` — a resumable, PK-preserving data-migration engine.** Streams rows
  between two umbral databases (env-to-env) or from a foreign source via
  `--map django|rails|laravel|prisma`. Copies in foreign-key-topological order with
  keyset pagination and transactional checkpoints (crash-safe, exact resume), handles
  M2M junction rows, cross-backend SQLite↔Postgres copies, circular-FK deferral, and
  parallel per-table workers.
- **`NaiveDateTime` field type** — a Postgres `TIMESTAMP` *without* time zone (Prisma's
  `DateTime` default), distinct from `DateTime<Utc>` (`TIMESTAMPTZ`); stored and read as
  a naive wall-clock value with no timezone conversion.
- **`ForeignKey<T>` usable as a primary key** — identifying relations, where a model's
  primary key *is* a foreign key (Prisma `@id` on a relation, Django
  `OneToOneField(primary_key=True)`).
- **PostGIS spatial support** — `geometry` / `geography` columns (feature-gated),
  the typed `GeometryCol::dwithin_meters` filter, and the REST `__dwithin` / `__bbox`
  query-string filters.
- **Reusable model bases** — `#[derive(ModelBase)]` + `#[umbral(flatten)]`, with typed
  base-column constants via `mixin_cols!`.
- **Decimals** — `#[umbral(precision, scale)]` for `numeric(N, M)` plus arbitrary-precision
  `BigDecimal`, with a sub-second `Time` fix.
- **Authorized reveal of hidden columns** — `.revealed()` / `.reveal([..])` on the ORM
  and REST layers.
- **Auth** — a `resetforeignpasswords` management command that neutralizes password
  hashes an imported database carries but umbral can't verify (it targets the
  `AuthPlugin`'s configured user model); built-in auth endpoints now also accept form
  bodies (`JsonOrForm`).
- Auto-derive `slug_from` on the typed create path.

### Changed

- `#[sqlx(rename = "...")]` is now honored as a field's actual column name (the
  foundation for framework-aware inspectdb naming).
- Postgres foreign-key constraints are emitted `DEFERRABLE INITIALLY IMMEDIATE`, so
  `SET CONSTRAINTS ALL DEFERRED` can defer cyclic inserts.
- Backup dump/load routes through each model's resolved database pool.
- Sessions: `DbStore::save` routes through the ORM upsert rather than raw SQL.

### Deprecated

- `Identity::user_pk` — use `Identity::pk`.

### Fixed

- **inspectdb**: the generated `0001_initial` now emits `CreateTable` operations in
  FK-topological order, so an inline `REFERENCES` no longer fails on Postgres with
  `relation "…" does not exist`.
- **inspectdb**: reading a raw foreign Postgres source now decodes a tz-less `TIMESTAMP`
  (via `NaiveDateTime`) and a native enum column (via the dynamic read path), which
  previously errored on a type mismatch.
- **Migrations**: squash-aware drift detection; `UNIQUE ADD COLUMN` on SQLite; RLS-gate
  ordering.
- **Web**: CORS segment-boundary matching, JSON-500 passthrough, production error
  blanking, and a `safe_url` scheme guard.
- **Forms**: preserve repeated urlencoded keys in the `Form<T>` extractor.
- **ORM**: honor a caller's `.offset()` in `try_for_each`; don't re-seal a no-change
  `Masked` submission on a JSON update.
- **Macros**: match serde's kebab rename for edge-case underscores; exclude `privileged`
  fields from the `Form` derive; reject `set_null` on a non-nullable foreign key.
- **OpenAPI**: exclude hidden columns from generated filter parameters.
- **App**: route the slash-redirect probe through the global middleware stack.

### Security

- **Storage**: cap image-decode allocation and apply the upload type allow-list on every
  save method; validate `FileField` / `ImageField` storage keys on deserialize; route
  `FsStorage` key generation through the neutralized-upload path.
- **Auth/sessions**: route session resolution through the store-aware helper so a
  surface can't silently read an unauthenticated session.

## Previous releases

Full, themed notes for each prior round live in [`changelog/`](./changelog/), one
file per version:

- **[0.0.11](./changelog/0.0.11.md)** — 2026-08-02 — Plugin ergonomics & private media
- **[0.0.10](./changelog/0.0.10.md)** — 2026-07-15 — Security-hardening sweep (review_3)
- **[0.0.9](./changelog/0.0.9.md)** — 2026-07-14 — Testing ergonomics & admin dashboards
- **[0.0.8](./changelog/0.0.8.md)** — 2026-07-13 — The GraphQL release
- **[0.0.7](./changelog/0.0.7.md)** — 2026-07-13 — Typed client & data modeling
- **[0.0.6](./changelog/0.0.6.md)** — 2026-07-08 — Data ergonomics & authorization
- **[0.0.5](./changelog/0.0.5.md)** — 2026-07-05 — Security-hardening sweep + custom admin views
- **[0.0.4](./changelog/0.0.4.md)** — 2026-06-30 — Hotfix: Postgres FK migration ordering
- **[0.0.3](./changelog/0.0.3.md)** — 2026-06-29 — The auth release
- **[0.0.2](./changelog/0.0.2.md)** — 2026-06-26 — Packaging & polish
- **[0.0.1](./changelog/0.0.1.md)** — 2026-06-25 — First public release

[Unreleased]: https://github.com/dalmasonto/umbral/compare/umbral-v0.0.12...HEAD
[0.0.12]: https://github.com/dalmasonto/umbral/compare/umbral-v0.0.11...umbral-v0.0.12
