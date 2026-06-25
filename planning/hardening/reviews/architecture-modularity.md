done hardening

# Architecture & Modularity Review — umbral workspace

> Read-only review. No source files were modified.
> Date: 2026-06-16
> Reviewer: Claude (automated, claude-sonnet-4-6)

---

## LOC table — 10 biggest files

| Rank | File | LOC |
|------|------|-----|
| 1 | `crates/umbral-core/src/orm/queryset/mod.rs` | 4 846 |
| 2 | `crates/umbral-core/src/migrate.rs` | 4 660 |
| 3 | `crates/umbral-macros/src/lib.rs` | 4 521 |
| 4 | `crates/umbral-core/src/orm/dynamic.rs` | 3 009 |
| 5 | `crates/umbral-core/src/orm/column.rs` | 2 845 |
| 6 | `plugins/umbral-rest/src/lib.rs` | 2 668 |
| 7 | `plugins/umbral-openapi/src/lib.rs` | 1 658 |
| 8 | `crates/umbral-core/src/forms.rs` | 1 561 |
| 9 | `crates/umbral-cli/src/scaffold.rs` | 1 467 |
| 10 | `crates/umbral-core/src/app.rs` | 1 409 |

---

## File-size / module-split candidates

### 1. `orm/queryset/mod.rs` (4 846 lines) — **top priority**

The file already spawned five sub-modules (`backend_pg.rs`, `backend_sqlite.rs`, `errors.rs`, `hydration.rs`, `tx.rs`, `write_helpers.rs`) but the parent `mod.rs` itself remains the largest file in the workspace. The problem is that all non-trivial logic stayed in `mod.rs` rather than following its extracted content.

**Proposed directory: `orm/queryset/`** (already exists — fill in the gaps)

| New sub-module | Move from `mod.rs` | Approx. lines |
|---|---|---|
| `orm/queryset/builder.rs` | `QuerySet<T>` struct + all chainable methods (lines 227–937): `filter`, `exclude`, `order_by`, `limit`, `offset`, `on`/`on_pg`/`on_tx`, `select_related`/`select_related_many`, `join_related*`, `prefetch_related*`, `into_subquery`, `union`/`intersect`/`except`, `distinct`, `with_deleted`, `only_deleted`, `hard_delete`, `only`, `atomic`/`non_atomic`, `build_query_for` | ~720 |
| `orm/queryset/joins.rs` | Free functions for join resolution (lines 939–1161): `JoinHop`, `resolve_pool`, `validate_join_related_fields`, `resolve_join_hops`, `resolve_join_hops_for`, `resolve_m2m_chain`, `only_with_typed_terminal_error`, `warn_right_join_on_sqlite`, `apply_join_related` | ~230 |
| `orm/queryset/read.rs` | Read terminals (lines 1162–1943, minus join helpers): `to_sql`, `to_sql_pg`, `apply_only_projection`, `fetch`, `try_for_each`, `first`, `earliest`, `latest`, `in_bulk`, `explain`, `count`, `exists`, `get`, `fetch_pg`, `first_pg`, `count_pg`, `exists_pg`, `get_pg` | ~780 |
| `orm/queryset/values.rs` | `values`, `aggregate`, `annotate`, `annotate_related`, `annotate_count`, `annotate_count_where`, `check_annotations`, `fetch_annotated` (lines 1944–2791) | ~848 |
| `orm/queryset/write.rs` | Write terminals (lines 2793–3302): `delete`, `update_expr`, `update_values`, `build_delete_for`, `soft_delete_update`, `build_update_for` | ~510 |
| `orm/queryset/manager.rs` | `Manager<T>` delegation surface + all write methods (lines 3305–4362): `create`, `bulk_create`, `get_or_create`, `update_or_create`, `upsert`, `create_pg`, `bulk_create_pg`, `bulk_update`, `raw`, `on_tx`, `create_in_tx`, `bulk_create_in_tx` | ~1 060 |
| `orm/queryset/m2m_dedup.rs` | `dedup_decode_sqlite`, `dedup_decode_pg`, `SoftOrHardStatement`, `save`, `delete_instance` (lines 4364–4846) | ~483 |

`mod.rs` after the split retains only: `Manager<T>` struct declaration, `QuerySet<T>` struct declaration, type aliases, `JoinKind`, `JoinReq`, `RelatedAnnotation`, `AutoDiscovery`, and `snake_case` helper — roughly 280 lines of declarations, re-exports, and the `pub use` surface.

---

### 2. `migrate.rs` (4 660 lines) — **top priority**

This single file contains: the global plugin/model registry, the `ModelMeta`/`Column`/`Operation` types, the migration file system layer, the drift detection + diff engine, the SQL rendering layer, and the tracking table helpers. These are four logically independent responsibilities.

**Proposed directory: `crates/umbral-core/src/migrate/`**

| New sub-module | Contents | Key fns/types | Approx. lines |
|---|---|---|---|
| `migrate/registry.rs` | Global plugin/model registration, `OnceLock` state | `REGISTRY`, `init_plugins`, `registered_models`, `is_initialised`, `pk_meta_for_table`, `fk_effective_type`, `registered_plugins`, `PLUGIN_ORDER`, `MODEL_ALIASES`, `init_plugin_order`, `plugin_order`, `API_ENDPOINTS`, `init_api_endpoints`, `registered_api_endpoints`, `init_model_aliases`, `table_alias`, `model_alias`, `models_for_plugin` | ~250 |
| `migrate/types.rs` | Core data types | `ModelMeta`, `M2MRelation`, `Snapshot`, `Operation`, `Column`, `MigrationFile`, `MigrationRef`, `MigrationStatus`, `MigrationEntry`, `DriftReport`, `MigrateError`, `OpSafety`, `ClassifiedOp`, `M2MPair` | ~600 |
| `migrate/engine.rs` | User-facing commands + orchestration | `make`, `make_in`, `run`, `run_checked`, `run_checked_in`, `run_in`, `detect_drift`, `detect_all_drift`, `fake_apply`, `fake_initial`, `show`, `check_pending_safety`, `classify_operation` | ~500 |
| `migrate/diff.rs` | Schema diffing | `diff`, `collect_m2m_pairs`, `build_create_m2m_op`, `column_shape`, `is_safe_cast`, `diff_columns`, `column_shape_matches`, `suffix_for`, `build_m2m_junction_meta` | ~400 |
| `migrate/render.rs` | SQL rendering for both backends | `render_operation`, `render_operation_for`, `render_operation_sqlite`, `render_operation_postgres`, `render_alter_column_dance_sqlite`, `render_alter_column_postgres`, `build_column_def_sqlite`, `build_column_def_postgres`, `postgres_type_name`, `quote_pg_ident`, `fk_target_pk`, `sqlite_bool_default`, `check_min_max_sql`, `fk_action_suffix`, `create_index_stmt`, `create_gin_index_stmt`, `create_multi_index_stmt`, `m2m_pk_sql_type_sqlite`, `m2m_pk_sql_type_postgres` | ~900 |
| `migrate/tracking.rs` | Per-backend tracking tables | `ensure_tracking_table_sqlite`, `ensure_tracking_table_postgres`, `applied_names_sqlite`, `applied_names_postgres`, `record_applied`, helper backend-dispatch fns | ~200 |

`migrate/mod.rs` retains only the top-level `pub use` re-exports to preserve the existing `umbral::migrate::*` public surface — roughly 40 lines.

---

### 3. `umbral-macros/src/lib.rs` (4 521 lines) — **top priority**

A single proc-macro crate `lib.rs` hosts four independent derive/attribute macros plus 15–20 private helper modules worth of code all flattened into one file. Cargo proc-macro crates require a single `lib.rs` entry point but nothing prevents using `mod` sub-modules within it.

**Proposed directory: `crates/umbral-macros/src/`** (add sub-modules)

| New file | Contents | Key fns | Approx. lines |
|---|---|---|---|
| `src/derive_model/mod.rs` | `#[derive(Model)]` top-level expand | `expand_model`, `expand_model_from_fields` (lines ~1–400) | ~400 |
| `src/derive_model/field_meta.rs` | Field attribute parsing | `ModelFieldAttr`, `ModelStructAttr`, `parse_field_attrs`, `parse_struct_attrs`, `FieldSpec` construction | ~500 |
| `src/derive_model/column_const.rs` | Column constant generation | `column_const_for`, `to_snake_case`, `to_screaming_snake_case`, `to_pascal_case` (lines 3067–3224) | ~158 |
| `src/derive_model/kind.rs` | Field kind classification | `FieldKind`, `classify_field_kind`, all `*_inner` extractors (`foreign_key_inner`, `multichoice_inner`, etc.), type-predicate helpers (`is_vec_u8`, `is_ipnetwork`, `is_tsvector`, `is_decimal`, `is_wide_or_unsigned_int`, `is_option_type`, etc.) (lines ~2400–2970) | ~580 |
| `src/derive_form.rs` | `#[derive(Form)]` macro | `FormFieldAttr`, `parse_form_attrs`, `FormStructAttr`, `parse_form_struct_attrs`, `FormFieldKind`, `classify_form_field_type`, `form_field_is_masked`, `expand_form` (lines 3225–4251) | ~1 026 |
| `src/derive_choices.rs` | `#[derive(Choices)]` macro | `RenameAll`, `apply_rename`, `expand_choices` (lines 4252–4521) | ~270 |
| `src/task_macro.rs` | `#[task]` attribute macro | `TaskArgs`, `parse_task_args`, `expand_task`, `is_result_unit_string` (lines 3475–3711) | ~237 |

`lib.rs` after the split becomes ~200 lines of `mod` declarations + the four public `#[proc_macro*]` entry points.

Key seam: `to_snake_case` at line 3167 is private to macros and used by both `column_const_for` and the derive model output. Move it to `src/derive_model/column_const.rs` with `pub(super)` visibility.

---

### 4. `orm/dynamic.rs` (3 009 lines)

`dynamic.rs` fuses: the error type, the query builder (`DynQuerySet`), SQLite/Postgres row decoders, form/JSON coercion helpers, M2M hydration, select-related FK expansion, shared INSERT infrastructure, and CSV import.

**Proposed directory: `crates/umbral-core/src/orm/dynamic/`**

| New sub-module | Contents | Key fns | Approx. lines |
|---|---|---|---|
| `dynamic/mod.rs` | Re-exports + `DynError`, `DynQuerySet` struct + all builder chain methods (lines 1–508) | `DynQuerySet::for_meta`, `select_cols`, `filter_*`, `order_by_col`, `limit`, `offset` | ~510 |
| `dynamic/read.rs` | Read terminals | `count`, `fetch_distinct_strings`, `fetch_as_strings`, `fetch_as_json`, `first_as_json` (lines 510–998) | ~490 |
| `dynamic/write.rs` | Write terminals | `delete`, `update_one`, `update_form`, `insert_form`, `insert_json`, `insert_json_in_tx`, `update_json` (lines 999–1462) | ~464 |
| `dynamic/decode.rs` | Row → string/JSON decoders | `decode_to_string`, `decode_pg_to_string`, `decode_to_json_aliased`, `decode_pg_to_json_aliased`, `decode_to_json`, `decode_pg_to_json` (lines 1465–1959) | ~495 |
| `dynamic/coerce.rs` | Form/CSV value coercion | `form_str_to_sea_value`, `hex_encode`, `bytes_to_json`, `panic_array_unsupported`, `panic_pg_only_unsupported`, `classify_or_sqlx`, `json_pk_to_sea`, `coerce_csv_cell` (lines 1961–2082) | ~122 |
| `dynamic/hydration.rs` | M2M + FK expansion helpers | `normalize_sr_token`, `validate_sr_chain`, `hydrate_select_related_into`, `dedup_by_pk_key`, `hydrate_m2m_batched`, `hydrate_m2m_into`, `pk_json_key`, `read_junction_id_sqlite/pg`, `collect_parent_pks` (lines 2083–2567) | ~485 |
| `dynamic/insert.rs` | Shared INSERT infrastructure | `normalise_insert_body`, `InsertPlan`, `build_insert_plan`, `write_m2m_junctions_in_tx`, `hydrate_m2m_into_tx`, `write_m2m_junctions` (lines 2568–2850) | ~283 |
| `dynamic/csv_import.rs` | CSV import | `CsvImportReport`, `import_table_rows` (lines 2852–2940) | ~89 |

---

### 5. `orm/column.rs` (2 845 lines)

`column.rs` is a catalogue of typed column sentinel structs, each with its own filter-builder methods. The file's structure is already logical (grouped by type family with section-header comments) but it is a flat 2 845-line file.

**Proposed directory: `crates/umbral-core/src/orm/column/`**

| New sub-module | Contents | Approx. lines |
|---|---|---|
| `column/scalar.rs` | Base scalar types: `IntCol`, `StrCol`, `DateTimeCol`, `NullableDateTimeCol`, `F64Col`, `BoolCol`, `UuidCol`, `DateCol`, `TimeCol` and all nullable mirrors (lines 1–1329) | ~1 330 |
| `column/json.rs` | `JsonCol`, `NullableJsonCol`, `JsonPathText`, `json_has_key_predicate` (lines 1331–1628) | ~298 |
| `column/array.rs` | `ArrayCol`, `NullableArrayCol`, operator helpers (lines 1630–1869) | ~240 |
| `column/network.rs` | `InetCol`, `NullableInetCol`, `CidrCol`, `NullableCidrCol`, `MacAddrCol`, `NullableMacAddrCol` (lines 1871–2235) | ~365 |
| `column/fulltext.rs` | `FullTextCol`, `NullableFullTextCol` (lines 2093–2194) | ~102 |
| `column/fk.rs` | `ForeignKeyCol`, `NullableForeignKeyCol` (lines 2237–2392) | ~156 |
| `column/special.rs` | `BytesCol`, `NullableBytesCol`, `DecimalCol` (lines 2394–2547) | ~154 |
| `column/expr.rs` | `ColExpr<T>`, `StrColExt`, `DateTimeColExt` and their impls (lines 2549–2845) | ~297 |

`column/mod.rs` provides `pub use` for the full column surface — ~30 lines.

Note: `scalar.rs` at ~1 330 lines is still large; a further split into `column/scalar_non_nullable.rs` and `column/scalar_nullable.rs` is optional.

---

### 6. `plugins/umbral-rest/src/lib.rs` (2 668 lines)

Most of `lib.rs` is already modularized (`filtering.rs`, `pagination.rs`, `resource.rs`, `auth.rs`, `permission.rs`). The remaining oversized `lib.rs` conflates: the `RestPlugin` builder, static config bridge, JSON Schema mini-validator, CRUD handlers, CSV export, and custom action dispatch.

**Proposed split within `plugins/umbral-rest/src/`:**

| New file | Move from `lib.rs` | Key fns | Approx. lines |
|---|---|---|---|
| `handlers.rs` | CRUD handler fns | `list`, `retrieve`, `create`, `create_nested`, `update`, `destroy` (lines 1704–2101) | ~398 |
| `actions.rs` | Custom action dispatch | `custom_action_dispatch`, `parse_query_string`, `parse_action_route`, schema validators: `validate_against_schema`, `validate_schema_node`, `json_type_matches`, `schema_label` (lines 1133–1207 + 2103–2207) | ~283 |
| `row_helpers.rs` | DB-facing helpers | `fetch_rows`, `parse_include`, `count_rows_filtered`, `allowed_model`, `pk_column`, `meta_for_table`, `child_fk_to`, `request_origin`, `api_root` (lines 1614–2358) | ~744 |
| `csv.rs` | CSV format helpers | `csv_response`, `rows_to_csv`, `csv_cell` (lines 1774–1834) | ~61 |

`lib.rs` after extraction: `RestPlugin` struct + all builder methods + static `CONFIG` + public bridge fns + `ApiError` + `Plugin` impl + routing helpers + `HideFields` trait — roughly 1 150 lines (still large, but cohesive).

---

### 7. `plugins/umbral-openapi/src/lib.rs` (1 658 lines)

The file is well-structured and its split value is lower. The one concrete improvement: extract the column schema logic into a dedicated module.

**Proposed:**

| New file | Move from `lib.rs` | Key fns | Approx. lines |
|---|---|---|---|
| `schema_gen.rs` | Column → OpenAPI schema | `column_schema`, `column_schema_with_refs`, `model_schema`, `openapi_type`, `schema_ref`, `list_envelope` (lines 406–731 + 1109–1125) | ~345 |
| `parameters.rs` | Query parameter building | `search_parameter`, `fields_parameter`, `include_parameter`, `pagination_parameters`, `filter_parameters`, `filter_parameter` (lines 733–954) | ~222 |
| `paths.rs` | Path item building | `collection_paths`, `item_paths`, `action_path_item` (lines 361–404 + 956–1107) | ~195 |

`lib.rs` retains: `OpenApiPlugin` struct + builder + `Plugin` impl + two handlers + `build_spec` (orchestrator, ~45 lines) + `pascal_case` helper + tests.

---

### 8. `crates/umbral-core/src/forms.rs` (1 561 lines)

The file has a clean natural boundary around line 835: everything before is the form primitive layer (validators, fields, HTML rendering), everything after is the axum integration layer (`FormErrors`, `Form<T>` extractor, conversions).

**Proposed:**

| New file | Contents | Key types/fns |
|---|---|---|
| `forms/primitives.rs` | Validator trait + concrete validators, `PkKind`, `InputKind`, `Field` (with all builder + render methods), `html_escape`, `IntegerFormat`, `FloatFormat` (lines 67–834) | `Validator`, `Required`, `MinLength`, `MaxLength`, `EmailFormat`, `RegexFormat`, `Field::text/email/regex/…/render_html/render_html_async` |
| `forms/extractor.rs` | axum integration | `FormErrors`, `Form<T>`, all `From` impls between `ValidationErrors`/`WriteError`/`FormErrors` (lines 836–1217) | `FormErrors`, `Form<T>`, `impl FromRequest` |

`forms/mod.rs` or `forms.rs` retains `FormValidate` trait, `ValidationErrors`, `pub use` of both sub-modules. Tests can be split to follow the content they exercise, or kept in `mod.rs` as now.

This is the cleanest single-seam split of any file in the list: the boundary is already commented in the source.

---

### 9. `crates/umbral-cli/src/scaffold.rs` (1 467 lines)

The bulk of the file is embedded string literals (generated `Cargo.toml`, `main.rs`, template files) inside the three scaffold functions. The code logic itself is modest.

**Recommended approach (lower effort than a module split):**

Extract the long embedded template strings into `include_str!` files under `crates/umbral-cli/templates/scaffold/`:
- `project/main_rs.template`
- `project/cargo_toml.template`
- `project/umbral_toml.template`
- `project/base_html.template`
- `project/home_html.template`
- `app/lib_rs.template`
- `app/cargo_toml.template`
- `plugin/lib_rs.template`
- `plugin/cargo_toml.template`
- etc.

This reduces `scaffold.rs` from ~1 467 to ~400 lines of pure logic with no content changes. If a module split is preferred, the three public functions map directly:

| New file | Contents |
|---|---|
| `scaffold/project.rs` | `scaffold_project` + its embedded templates |
| `scaffold/app.rs` | `scaffold_app` + its embedded templates |
| `scaffold/plugin.rs` | `scaffold_plugin` + its embedded templates |
| `scaffold/util.rs` | `validate_name`, `pascal_case`, `localize_deps`, `rewrite_line`, `rust_ident`, `write_file`, `ScaffoldError`, `ScaffoldReport`, `RESERVED_PLUGIN_NAMES` |

---

### 10. `crates/umbral-core/src/app.rs` (1 409 lines)

`app.rs` is cohesive — `App`, `AppBuilder`, and `BuildError` all belong together. The maintainability issue is `AppBuilder::build` at ~628 lines (lines 503–1131). The function is a 10-phase sequential boot sequence; each phase is already clearly commented.

**Recommended: extract private helpers within `app.rs` (not a module split)**

| New private fn | Phase(s) | Lines saved |
|---|---|---|
| `fn validate_plugins(plugins, settings) -> Result<Vec<Box<dyn Plugin>>, BuildError>` | Phases 1.5 + 2.5 + 2.5b (plugin toposort, alias validation, cross-DB FK guard) | ~150 |
| `fn publish_ambient_state(settings, pools, backend, models, plugins, routes, ...)` | Phase 3 (all `OnceLock` writes) | ~80 |
| `fn assemble_router(plugins, static_state, settings) -> Router` | Phases 5–5.95 (plugin routes, middleware stack, 404 fallback, CORS, host guard) | ~230 |

`build()` after extraction: ~170 lines (phases 1, 2, 4 system-checks, 6 on_ready, and the three helper calls).

---

## Boundaries

### No core → plugin dependency violations (Clean)

All 19 plugins depend only on the `umbral` facade crate. No plugin imports `umbral_core` directly in production source. The one occurrence (`umbral-media/src/lib.rs:68`) is a doc-comment reference, not a `use` statement. **The boundary is clean.**

### Plugin → plugin dependencies (Informational, not violations)

Cross-plugin prod deps are:

```
umbral-admin      → umbral-auth, umbral-permissions, umbral-sessions, umbral-security
umbral-auth       → umbral-rest, umbral-sessions
umbral-oauth      → umbral-auth, umbral-sessions
umbral-openapi    → umbral-rest
umbral-permissions → umbral-auth, umbral-rest (optional)
umbral-realtime   → umbral-auth
```

These are all legal directed edges (no cycles). However one edge deserves attention:

### `umbral-auth → umbral-rest` (Important)

`umbral-auth/Cargo.toml` lists `umbral-rest` as a prod dependency to get the `Authentication` and `Identity` traits. This means every app that uses `umbral-auth` (nearly all apps) also pulls in `umbral-rest` transitively — even apps that have no REST API. This contradicts the documented goal: "A REST-free app has to compile and run with zero serializer code."

The fix is to lift `Authentication` and `Identity` out of `umbral-rest` into the `umbral` facade (or `umbral-core`) and have `umbral-rest` import them from there. Then `umbral-auth` depends only on `umbral`, and `umbral-rest` can remain optional.

**Severity: Important** — not a circular dep, but it breaks the "REST is truly optional" invariant stated in `CLAUDE.md`.

### `umbral-admin` depends on `umbral-security` (Informational)

`umbral-security` is a plugin. `umbral-admin` depending on it as a prod dep means the CSRF middleware is implicitly required. This should be documented as an explicit design decision (admin requires security) rather than left as an implicit transitive dep.

### `umbral-core` dev-dep on `umbral` (Safe)

`crates/umbral-core/Cargo.toml` has `umbral = { path = "../umbral" }` as a **dev-dependency** only. This is legal — Cargo dev-deps cannot create cycles. There is an explanatory comment in the Cargo.toml. No action needed.

### Facade completeness check

`crates/umbral/src/lib.rs` re-exports comprehensively. One gap found: `DynError` is re-exported via `umbral::orm` (from `umbral_core::orm::mod`) but is defined in `umbral_core::orm::dynamic`. The path resolves correctly. No missing public items were found.

---

## Duplication

### D1. `to_snake_case` — three independent implementations (Important)

Three camel-to-snake conversion functions exist:

| Function | File | Line | Use |
|---|---|---|---|
| `to_snake_case(camel: &str) -> String` | `crates/umbral-macros/src/lib.rs` | 3167 | Table name generation in `#[derive(Model)]` |
| `derive_table_name(camel: &str) -> String` | `crates/umbral-core/src/inspect.rs` | 582 | Table name inference in `inspectdb` |
| `snake_case(name: &str) -> String` | `crates/umbral-core/src/orm/queryset/mod.rs` | 279 | Query alias generation |

`inspect.rs:582` has a comment explicitly documenting the duplication: *"Mirror of `umbral_macros::to_snake_case`. Kept identical to the derive's body so the two agree byte-for-byte."* The reason is that proc-macro crates cannot be called as libraries at compile time.

The clean fix is to extract `to_snake_case` into a small non-proc-macro helper crate (e.g. `umbral-naming`) that both `umbral-macros` and `umbral-core` depend on. The queryset `snake_case` function could also move there. Until such a crate exists, the current arrangement with the documented comment is acceptable but creates a maintenance hazard: if the two implementations diverge, `inspectdb` will generate model definitions that map to different table names than `#[derive(Model)]` would.

### D2. `pascal_case` — two independent implementations (Nit)

| Function | File | Line |
|---|---|---|
| `fn pascal_case(s: &str) -> String` | `plugins/umbral-openapi/src/lib.rs` | 1144 |
| `fn pascal_case(name: &str) -> String` | `crates/umbral-cli/src/scaffold.rs` | 131 |

Both convert snake/kebab identifiers to PascalCase. Both are private to their module. No immediate action needed, but if an `umbral-naming` helper crate is created (see D1), these belong there.

### D3. `classify_sql_error` vs `classify_or_sqlx` — partial overlap (Nit)

| Function | File | Line |
|---|---|---|
| `pub fn classify_sql_error(e: &sqlx::Error, body: &Map<String, Value>) -> Option<WriteError>` | `crates/umbral-core/src/orm/validation.rs` | 167 |
| `fn classify_or_sqlx(e: sqlx::Error, ...) -> DynError` | `crates/umbral-core/src/orm/dynamic.rs` | 2061 |

`classify_or_sqlx` is a thin wrapper that calls `classify_sql_error` and then converts the result. No code duplication, but the naming divergence (`classify_sql_error` vs `classify_or_sqlx`) hides the relationship.

### D4. Direct `sea-query` use in `umbral-rest/src/filtering.rs` (Important)

`filtering.rs` builds `sea_query::Condition` objects directly (~26 call sites). The ORM's `Q` builder and `F` expressions exist precisely to avoid plugins writing raw sea-query. The filtering module is a grey area — it needs to compose complex filter conditions dynamically from URL parameters — but the dependency is currently hidden in plain sight: `umbral-rest` lists `sea-query = "0.32"` in its own `Cargo.toml`, duplicating the same version the core already pins. If the core upgrades sea-query, rest must follow. This is not a bug today, but it is a hidden coupling that will bite during a sea-query major version bump.

---

## Dead code

### DC1. `#[allow(dead_code)]` in production source (Important)

One hit in production code (not test files):

```
plugins/umbral-rest/src/filtering.rs:94
```

Read the surrounding code before the review closes to see what is suppressed. If it is a struct field that is parsed but never read, it is either a placeholder for future filter logic (which should be a `gaps2.md` entry) or a genuine unused field (remove it).

### DC2. `fn on_ready` in `umbral-admin` does nothing (Nit)

`plugins/umbral-admin/src/lib.rs:788` — `on_ready` has a comment "No bootstrap DDL here" and returns `Ok(())`. The Plugin contract treats `on_ready` as the startup hook. If admin genuinely has no work to do at startup, this is fine. No action needed.

### DC3. `#[allow(dead_code)]` in `umbral-macros/src/lib.rs:2041` and `:2083` (Nit)

Line 2041: a struct field in the macro expansion that is generated but not consumed by the expansion path for Cidr/NullableCidr (line 2083 comment: "Cidr / NullableCidr are matched but the derive..."). This is a known limitation documented inline. The suppression is intentional; the real fix is to use the field or remove it from the generated code.

### DC4. No `// TODO: wire later` patterns found

The grep for `// TODO.*wire` returned zero hits. This is a positive finding — there are no admitted unfinished wiring comments.

---

## Abstraction

### A1. `AppBuilder::build` — one 628-line function (Important)

`crates/umbral-core/src/app.rs:503–1131` is a single method doing 10 sequential phases. Each phase is clearly commented, but the function has no testable decomposition — its correctness is only verifiable by booting the full stack. If any phase fails, the error message must be cross-referenced back to the code by line number.

Extracting three private helpers (`validate_plugins`, `publish_ambient_state`, `assemble_router`) would make each phase independently testable and reduce cognitive load when reading the boot sequence. No external API change.

### A2. `DynQuerySet` builder chain in a single file (Important)

The 500-line chainable filter builder in `dynamic.rs:134–508` has 14 distinct filter methods each following the same pattern: validate field name → build `sea_query::Condition` → push to `where_clauses`. This pattern is repeated in both `DynQuerySet` and `QuerySet<T>`. There is no shared abstraction. A `WhereClauseBuilder` trait or common private helper that both types use would eliminate the parallel maintenance burden, but this requires care to avoid over-engineering — the two types differ materially in their type constraints.

### A3. `validate_against_schema` in `umbral-rest/src/lib.rs:1133` — single caller (Optional)

The mini JSON Schema validator (four functions, ~70 lines, lines 1133–1204) is called only from `custom_action_dispatch`. It is a small abstraction with one caller. However it does encapsulate a real concern (input validation for custom actions) and the code is unlikely to be called from a second site. Extracting it to `actions.rs` (see split proposal above) is sufficient; a separate crate is not justified.

### A4. `HideFields` trait — 8 identical impls (Nit)

`plugins/umbral-rest/src/lib.rs:122–173` defines `HideFields` and 8 impls for different string container types (`&str`, `String`, `[&str; N]`, `[String; N]`, `&[&str]`, `&[String]`, `Vec<&str>`, `Vec<String>`). This is standard Rust ergonomics boilerplate — not over-engineering. The macro `impl_hide_fields!` would shrink the code but is unnecessary.

### A5. `decode_to_string` + `decode_pg_to_string` + `decode_to_json` + `decode_pg_to_json` — four near-parallel decode functions (Important)

`dynamic.rs` contains four public decode functions (lines 1465–1959, ~495 lines) that are paired SQLite/Postgres versions of two logical operations (row → String, row → JSON). The Postgres and SQLite paths are separate functions rather than a single generic path dispatched by backend. This means each `SqlType` arm must be maintained in two or four places simultaneously. If a new column type is added (e.g., a future `TimestampTzCol`), all four functions must be updated.

The pattern of `match col_type { SqlType::Integer => row.try_get::<i64, _>(alias)?... }` repeated four times is the most concentrated maintenance hazard in the ORM. A future refactor could unify via the `DatabaseBackend` dispatch enum, but that requires a design decision about the decode trait surface. Flag for the ORM gap backlog.

---

## Summary — top 5 split candidates ranked by pain-relief

| Rank | File | Pain | Why |
|------|------|------|-----|
| 1 | `orm/queryset/mod.rs` (4 846 L) | Highest | Every ORM change touches this file; the Manager write methods, the chainable builder, the annotation engine, the M2M dedup logic, and the read terminals are all in one namespace, making it impossible to navigate to any single concern without scrolling thousands of lines |
| 2 | `migrate.rs` (4 660 L) | Highest | Registry state, type definitions, diff engine, SQL renderer, and tracking helpers are completely independent concerns sharing a file; the SQL rendering layer alone (~900 lines, 8 fns) is never touched when editing the diff logic, yet both live in the same scrollable surface |
| 3 | `umbral-macros/src/lib.rs` (4 521 L) | High | Four derive/attribute macros, 15+ private helper modules worth of type predicates, and the case conversion utilities are all flattened; adding a new field type requires editing the same file as fixing a `#[derive(Form)]` bug |
| 4 | `orm/dynamic.rs` (3 009 L) | High | The SQLite/Postgres decode functions (~495 L), M2M hydration (~485 L), and shared INSERT infrastructure (~283 L) are unrelated to the `DynQuerySet` chainable builder they share a file with; each of those blocks has its own internal state and helpers that are invisible from a function list |
| 5 | `migrate.rs` rendering vs. `orm/column.rs` | Moderate | `column.rs` is already well-grouped by type family but at 2 845 lines with no sub-modules; the Postgres-only column families (array, network, full-text) could move to sub-modules that are only compiled when the Postgres feature is active, improving SQLite-only build times |

---

## Boundary summary (no critical violations found)

| Check | Result |
|---|---|
| `use umbral_core::` in plugin prod source | **None** — clean |
| `umbral-core` → plugin dep | **None** — clean |
| Circular deps | **None** found |
| `umbral-auth → umbral-rest` (REST forced on all apps) | **Important** — breaks "REST optional" invariant |
| Facade re-exports `umbral-core` internals correctly | **Yes** — no gaps found |
| `sea-query` version pinned in both core and umbral-rest | **Important** — hidden upgrade coupling |
