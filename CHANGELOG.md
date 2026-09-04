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

### Added

- **Model-level presentation/query metadata, shared across plugins (gaps4 #95).** Declare a model's display + query intent ONCE on the model with per-field markers — `#[umbral(list_display)]`, `#[umbral(search)]`, `#[umbral(list_filter)]`, `#[umbral(inline_edit)]`, `#[umbral(readonly)]` — and `#[derive(Model)]` aggregates them (by column name, honoring `#[sqlx(rename)]`) into new `Model` consts (`LIST_DISPLAY`, `SEARCH_FIELDS`, `LIST_FILTER`, `INLINE_EDIT_FIELDS`, `READONLY_FIELDS`), mirrored onto `ModelMeta`. The **admin** auto-derives its changelist (list columns, search box, filter facets, inline-edit + readonly columns) from this when no `AdminModel` overrides it, and **REST** inherits the searchable columns for `?search=` with no `ResourceConfig` setup. Per-plugin config still wins. Removes the bulk of per-model admin wiring in `main.rs`. See `orm/model-metadata`.

- **`umbral-oauth` SPA-login ergonomics + boot-time safety checks (gaps4 #93).** Social login for a separate-origin SPA had several silent traps; this closes them:
  - **Mask-keyring boot check.** The OAuth callback seals provider tokens into a `Masked` column, so a missing keyring made it 500 with a generic message. When a provider is registered but no mask keyring is configured, the plugin now **warns at boot** (pointing at `umbral maskkeygen` / `UMBRAL_MASK_PUBLIC_KEY`) instead of failing at the first sign-in. New public probe `umbral::orm::mask_keyring_configured()`.
  - **Env-driven return allowlist.** `OAuthPlugin::from_settings` now reads `oauth_allow_return` (`UMBRAL_OAUTH_ALLOW_RETURN`, comma-separated), so the SPA return-URL allowlist is a config change like every other OAuth knob — no recompile to move the SPA origin dev→prod. Builder `.allow_return(...)` still works and appends. New read accessor `OAuthPlugin::allowed_returns()`.
  - **Token-mode visibility.** With token mode enabled, the plugin logs the `?next=` requirement at boot, and a login that completes with no allowlisted `?next` (so no bearer token is minted) is logged as a warning — the SPA "logged in on the backend but not the SPA" trap is now diagnosable.
  - Docs (`auth/oauth`) gained an explicit two-modes split (fullstack same-origin vs API + separate-origin SPA), the mask-keyring requirement callout, the env-driven allowlist, and a BFF httpOnly-cookie handoff recipe.

### Fixed

- **Admin list search no longer pollutes browser history (gaps4 #96).** Search-as-you-type in the admin table editor pushed one history entry per keystroke, so the back button had to be pressed once per character to escape a search. The server now sends `HX-Replace-Url` (not `HX-Push-Url`) when a `/rows` request is triggered by the live search input, so a keystroke replaces the current history entry; discrete navigation — pagination, page-size, filter-chip removal — still pushes a real entry. (The 300ms input debounce and partial-tbody swap that keep typing smooth were already in place.)

- **Dynamic M2M writes now bind junction ids against the referenced PK type, not the JSON shape (gaps4 #94).** An admin form or REST `PATCH` of a many-to-many field sends child ids as JSON strings (`["1","2"]`); the dynamic junction writer bound each as a TEXT parameter. SQLite coerced `text`↔`bigint` silently, but Postgres rejected it (`column "child_id" is of type bigint but expression is of type text`), so **every live M2M edit 500'd on Postgres**. Junction `parent_id`/`child_id` now coerce to the referenced PK's actual `SqlType` (integer PK → `BigInt`, `Text`/`Uuid` PK bind accordingly), matching the FK-column fix. No API change; a Postgres regression test covers the string-id write.

### Changed

- **Admin record form + detail read cleaner (gaps4 #61, pass 4).** On the live edit/create sheet, `#[umbral(help = "...")]` text now sits between the label and the input (you read what a field is before filling it; validation errors stay below the input), and the DB-jargon `(nullable)` badge became a quiet muted "Optional" hint. The record detail (preview) read-view dropped its all-caps labels for clean sentence case and swapped a hardcoded `border-gray-300` divider for the themed `outline-variant` hairline, so preview and edit read as the same field in light and dark. Live sheet + field-editor templates only — the HTMX form-ids, field `name=`/`id=` contract, and validation flow are unchanged; the legacy full-page `form.html`/`detail.html` fallbacks are a later pass.

- **Admin changelist reads as a table editor, not a list (gaps4 #61, pass 3).** The auto CRUD list page picked up a spreadsheet-style data grid: faint vertical column hairlines, a tighter-but-calm 10px row rhythm, and a genuinely visible row-hover band (the old hover tint equalled the surface color in light mode, so it was invisible). Column headers dropped the all-caps treatment for clean sentence case. Purely the admin's server-rendered templates + CSS — no ORM, plugin-surface, or `AdminView` change; the datatable's `#dt-*` ids, `.dt-row`/`.dt-col` hooks, sort/search/inline-edit/bulk behavior are untouched. Built entirely on the existing "umbra dusk" tokens, so it themes in light and dark. Next passes in the sequence: detail/form pages, dashboard, custom views.

- **`#[derive(ModelBase)]` now auto-emits `impl Default` + an inherent `new()`** so a based model constructs with `base: Default::default()` (the auto-managed PK / `auto_now`* columns are overwritten on insert anyway). **Potentially breaking:** a base struct that ALSO derives or hand-implements `Default` now hits a conflicting-impl error (E0119) — drop the redundant `#[derive(Default)]` / `impl Default` from `#[derive(ModelBase)]` structs on upgrade.

- **`#[derive(Choices)]` now emits its own `serde::Serialize` / `Deserialize`.** A Choices enum's `#[choices(rename_all = "...")]` casing now single-sources the serde/JSON wire form with the stored DB value / `CHECK` / validator, so a Choices field round-trips the typed write path (`create` / `get_or_create` / `update_or_create`) with only `#[choices(rename_all = "...")]` — no duplicated `#[serde(rename_all)]` needed. **Breaking:** a `#[derive(Choices)]` enum must **no longer** also derive `serde::Serialize` / `Deserialize` or carry `#[serde(...)]` attributes — the derive now owns those impls, so doing both is a conflicting-implementation error. On upgrade, remove the redundant `#[derive(Serialize, Deserialize)]` and `#[serde(...)]` from Choices enums.

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
