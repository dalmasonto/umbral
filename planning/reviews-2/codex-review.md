# Umbral ORM Review

Scope reviewed: `crates/umbral-core` ORM/query/migration/dynamic-row paths and the plugin surfaces in `plugins/umbral-admin`, `plugins/umbral-rest`, `plugins/umbral-openapi`, and related permission models.

## Findings

### 1. High: `join_related` treats non-integer related primary keys as missing rows

`join_related` now supports nested FK/M2M joins, but the joined-row presence checks still decode every related primary key as `Option<i64>`. SQLite does this in `hydrate_joined_rels`, M2M child decode, and onward M2M FK-chain decode (`crates/umbral-core/src/orm/queryset/backend_sqlite.rs:102`, `crates/umbral-core/src/orm/queryset/backend_sqlite.rs:159`, `crates/umbral-core/src/orm/queryset/backend_sqlite.rs:193`). Postgres has the same pattern (`crates/umbral-core/src/orm/queryset/backend_pg.rs:78`, `crates/umbral-core/src/orm/queryset/backend_pg.rs:127`, `crates/umbral-core/src/orm/queryset/backend_pg.rs:156`). `values()` traversal repeats the same `Option<i64>` presence check for joined related objects (`crates/umbral-core/src/orm/queryset/mod.rs:2281`, `crates/umbral-core/src/orm/queryset/mod.rs:2317`).

That conflicts with the rest of the ORM's PK lift work: `HydrateRelated::fk_id_for` explicitly carries `serde_json::Value` so `String` and `Uuid` keys can flow through relation hydration (`crates/umbral-core/src/orm/model.rs:44`), and migration metadata already resolves the effective FK type from the target PK (`crates/umbral-core/src/migrate.rs:153`). With a `ForeignKey<T>` or M2M target whose PK is `String`/`Uuid`, a real joined row fails the `Option<i64>` decode, falls into `unwrap_or(true)`, and is treated like a left-join miss. The result is an unresolved FK, an empty M2M slot, or a `null` nested value from `values("fk__field")`.

Recommended fix: centralize an alias presence helper that checks the related PK using the related column's effective SQL type, not `i64`. Add SQLite tests for `join_related` and `values` against a String-PK child, and Postgres tests for UUID-PK child joins where the environment is available.

### 2. High: `QuerySet::first()` ignores `join_related` and `prefetch_related`

`fetch()` clones `select_related`, `prefetch_related`, and `join_related`, validates joins, applies joined SQL, decodes joined rows, then hydrates select/prefetch relations (`crates/umbral-core/src/orm/queryset/mod.rs:1499`, `crates/umbral-core/src/orm/queryset/mod.rs:1532`, `crates/umbral-core/src/orm/queryset/mod.rs:1594`). `first()` only preserves `select_related`, builds a plain query, and hydrates only select-related fields (`crates/umbral-core/src/orm/queryset/mod.rs:1682`, `crates/umbral-core/src/orm/queryset/mod.rs:1692`, `crates/umbral-core/src/orm/queryset/mod.rs:1719`).

So `Model::objects().join_related("author").first().await` returns a row but does not hydrate the joined relation, and `prefetch_related("tags").first().await` silently returns an unprefetched model. This diverges from `get()`, which goes through `fetch()`, and it is especially surprising because the builder API accepts those relation directives before any terminal.

Recommended fix: make `first()` delegate to the same terminal path as `fetch()` with `LIMIT 1`, or port the same join/prefetch handling into `first()`. Add tests for `join_related(...).first()` and `prefetch_related(...).first()`.

### 3. High: admin dynamic string decoding still assumes FK values are `i64`

Admin list/detail/edit paths use `DynQuerySet::fetch_as_strings()` for display rows (`plugins/umbral-admin/src/rows.rs:104`, `plugins/umbral-admin/src/rows.rs:114`, `plugins/umbral-admin/src/rows.rs:175`, `plugins/umbral-admin/src/rows.rs:181`). The dynamic string decoder behind that path decodes every `SqlType::ForeignKey` as `i64` on both backends (`crates/umbral-core/src/orm/dynamic.rs:1512`, `crates/umbral-core/src/orm/dynamic.rs:1542`, `crates/umbral-core/src/orm/dynamic.rs:1619`, `crates/umbral-core/src/orm/dynamic.rs:1652`).

This breaks plugin models that are already PK-agnostic. `umbral-permissions` intentionally uses string-shaped permissions and includes `UserPermission.permission_id: ForeignKey<Permission>` (`plugins/umbral-permissions/src/models.rs:33`, `plugins/umbral-permissions/src/models.rs:259`). A row containing a FK to a string-PK permission can be valid in the database but fail when admin tries to render it as a string.

Recommended fix: mirror the JSON decoder path and decode FK string cells through `fk_effective_type` / target PK metadata. Add an admin or core dynamic-string test for a `ForeignKey<StringPkModel>` column.

### 4. High: REST filters and OpenAPI schemas expose all FK/M2M IDs as `int64`

REST filtering treats `SqlType::ForeignKey` as integer in free-text search, `__in`, and typed value coercion (`plugins/umbral-rest/src/filtering.rs:246`, `plugins/umbral-rest/src/filtering.rs:501`, `plugins/umbral-rest/src/filtering.rs:527`). That means valid FK values such as permission codenames or UUIDs are rejected before the query reaches the ORM.

OpenAPI has the same contract bug: ordinary FK columns always become `integer/int64` (`plugins/umbral-openapi/src/lib.rs:675`, `plugins/umbral-openapi/src/lib.rs:708`), and M2M write properties are documented as arrays of integer IDs (`plugins/umbral-openapi/src/lib.rs:439`, `plugins/umbral-openapi/src/lib.rs:451`). Existing OpenAPI tests only assert the integer shape, so they lock in the wrong contract for non-integer target PKs.

Recommended fix: carry effective target PK type into REST filter coercion and OpenAPI schema generation. For M2M, resolve the child model PK type and render `items` accordingly. Add REST filter and OpenAPI snapshot-style tests for FK/M2M targets keyed by `String` and `Uuid`.

### 5. Medium: transactional `count()` still renders `COUNT("*")`

`QuerySetTx::count()` clears selects and then builds `Func::count(Expr::col(Alias::new("*")))` (`crates/umbral-core/src/orm/queryset/tx.rs:101`, `crates/umbral-core/src/orm/queryset/tx.rs:106`). The non-transactional count path already uses `sea_query::Asterisk`, with a comment explaining that Postgres must see a bare `*` rather than a quoted `"*"` (`crates/umbral-core/src/orm/queryset/mod.rs:1882`, `crates/umbral-core/src/orm/queryset/mod.rs:1884`).

That leaves the transaction API with the old rendering path. On Postgres this can generate `COUNT("*")`, which refers to a quoted identifier instead of the SQL asterisk token.

Recommended fix: switch `QuerySetTx::count()` to `Func::count(Expr::col(sea_query::Asterisk))` and add a transaction count test, ideally one that checks rendered Postgres SQL or runs under `UMBRAL_TEST_POSTGRES_URL`.

## Test Results

Ran focused tests from `/home/dalmas/E/projects/umbral/crates`:

- `cargo test -p umbral-core --test join_related --test join_related_m2m --test values_traversal --test dyn_string_pk_include --test pk_string_m2m --test pk_uuid_postgres --test transactions`
  - Passed: 38 tests.
  - Ignored: `pk_uuid_postgres::uuid_pk_relations_round_trip_on_postgres` because `UMBRAL_TEST_POSTGRES_URL` is not configured.
- `cargo test -p umbral-rest --test filtering`
  - Passed: 9 tests.
- `cargo test -p umbral-openapi --test integration`
  - Passed: 8 tests.
- `cargo test -p umbral-admin --test integration --test phase3_fk_picker`
  - Passed: 9 tests.

The passing tests cover the current integer-key happy paths and some String/UUID PK work, but they do not cover the failing combinations above: `join_related`/`values` with non-integer child PKs, relation directives on `first()`, admin string rendering of FK-to-string-PK columns, REST filters for FK-to-string/UUID targets, or transactional Postgres `count()`.

---

## Resolution (2026-06-14)

All five findings fixed, each with a test (several verified against a live Postgres via `UMBRAL_TEST_POSTGRES_URL`). The review was accurate: #1, #3, #4 were genuine gaps in the PrimaryKey-lift work (the JSON hydration / in_bulk / join-dedup / backup paths were lifted, but the `join_related` presence checks, the dynamic *string* decoder, and the REST-filter/OpenAPI FK coercion were missed); #2 and #5 were pre-existing.

| # | Sev | Fix | Commit | Test |
|---|-----|-----|--------|------|
| 1 | High | `join_related`/`values` presence checks decode the related PK via `decode_*_to_json_aliased` (its `SqlType`), not `Option<i64>` — 8 sites across `backend_sqlite`/`backend_pg`/`queryset::mod` | `fb5577d` | `pk_string_join_values.rs` (values + join_related on a String-PK FK) |
| 2 | High | `first()` sets `LIMIT 1` and delegates to `fetch()` (which hydrates select/prefetch/join) instead of handling only select_related | `8bbe9c0` | `first_hydrates_relations.rs` (prefetch/select/join via first) |
| 3 | High | dynamic **string** decoder (`decode_to_string`/`decode_pg_to_string`, the admin display path) dispatches FK cells on `fk_target_pk_sql_type`, matching the JSON decoder — 4 arms | `42e4a16` | `dyn_string_pk_include.rs::fetch_as_strings_renders_string_pk_fk_cell` |
| 4 | High | REST `build_in_predicate`/`coerce_value`/`parse_search` + OpenAPI FK schema use `fk_effective_type`; OpenAPI M2M `items` use the child PK type via `pk_meta_for_table`. (Bonus: `?search=` now runs FTS on `tsvector` columns.) | `6d81f0b`, `2e0bb0e` | `filtering.rs` (FK-to-slug `?cat=`/`?cat__in=`), `rest_fts_pg.rs` (live PG FTS), `openapi/integration.rs` (FK/M2M-to-String-PK render as `string`) |
| 5 | Med | `QuerySetTx::count()` uses `sea_query::Asterisk` (`COUNT(*)`) not `Alias::new("*")` (`COUNT("*")`) | `57bd7ab` | `tx_count_pg.rs` (live PG `COUNT(*)` in a transaction) |

Two helpers (`fk_effective_type`, `pk_meta_for_table`) were re-exported from the facade (`umbral::migrate`) for the plugin fixes. Full workspace test re-run after the changes.

### Follow-up (same day)

Running the **full workspace** suite after the fixes caught a regression in fix #1 itself: decoding the joined PK with the column's declared `nullable: false` let SQLite coerce a NULL (left-join miss) to `0`, so a parent with no M2M children got a phantom child. Fixed in `790b092` — a `joined_pk_is_null` helper per backend decodes the PK as *nullable* (correct miss detection) while staying PK-agnostic; all 8 presence sites use it. `join_related`/`join_related_m2m`/`joins_nested`/`values_traversal`/`pk_string_join_values` are green.

**Known, pre-existing, unrelated:** the full-workspace run is non-deterministic because many test `boot()` helpers share one `connect_sqlite("sqlite::memory:")` pool across per-`#[tokio::test]` tokio runtimes; the in-memory DB doesn't reliably survive a runtime drop, so tests intermittently see "no such table" (e.g. `filter_m2m`, `annotate_autodiscover`, `phase2_sheet`) — all pass 100% in isolation. Two `connect_sqlite`-level fixes (single-connection; named shared-cache) were tried against the deterministic `--test-threads=1` repro and BOTH failed, confirming it's the test pattern, not the helper. The real fix is test-level (temp-file SQLite DBs, or a shared multi-thread runtime); tracked as a separate test-infra task.
