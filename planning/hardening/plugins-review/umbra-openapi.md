# umbral-openapi — holistic review

Read-only review, 2026-06-16. Scope: `plugins/umbral-openapi/src/lib.rs` (1658 LOC, single file) + `tests/integration.rs` (1 file, ~10 fns) + `templates/swagger_ui.html`. All findings **NET-NEW** unless they reference an already-filed gap (per-request spec rebuild was noted Optional in the perf review; `pascal_case` dup is #77).

## Verdict

**Accurate and surprisingly complete for an auto-spec generator, but with three spec-fidelity holes that make the published document diverge from what the API actually does.** The plugin walks the model registry, defers to `umbral_rest::is_exposed` for the block-list (no duplication), and emits a valid OpenAPI 3.0.3 document with per-model schemas, six operations per model, FK `$ref`-flavoured vendor extensions, M2M arrays, choices→`enum`, `help`→`description`, `example`, `min`/`max`, `noform`→`readOnly`, security schemes, and `@action` path items with inlined request/response schemas. Completeness one-liner: **the schema layer (auth schemes, hidden-field scrubbing, FK/M2M refs, action schemas, filter params) is thorough; the holes are in the *path + pagination + ordering* layer, where the spec hardcodes `/api/...` and `page`/`page_size` regardless of the live REST config.** Worst net-new finding: **CRUD paths are hardcoded `/api/{table}/`, so `RestPlugin::at("/v1")` yields a spec whose endpoints 404 in Swagger "Try it".**

## Completeness

| OpenAPI surface | Covered | Notes |
|---|---|---|
| Valid 3.0.3 envelope | ✅ | `openapi`/`info`/`paths`/`components`; tested |
| Per-model schema | ✅ | properties + `required` (PK/auto_now/auto_now_add/noform correctly dropped from required) |
| Six CRUD operations | ✅ | list/create on collection, retrieve/put/patch/delete on item; `operationId` per op |
| Hidden-field scrubbing | ✅ | `umbral_rest::is_hidden` consulted for `properties`, `required`, `?fields=`, `?include=` pickers — `password_hash` never leaks into the spec |
| FK schema refs | ✅ | `x-umbral-fk-ref` JSON pointer + `x-umbral-fk-target`; FK effective type (String/Uuid PK) honored |
| M2M relations | ✅ | array-of-PK property + `x-umbral-m2m*` extensions + target `$ref` |
| `@action` paths | ✅ | input/output schema inlined; `{id}` param for detail scope; method/operationId/tags |
| Auth / securitySchemes | ✅ | reads `umbral_rest::registered_security_schemes()`; emits `components.securitySchemes` + global `security` (OR) |
| Permissions in spec | 🟡 | no per-operation `security` reflecting per-resource `.permission(...)` — only the global auth chain. A staff-only resource looks identical to an open one in the spec |
| Filter params | ✅ | per-(column,lookup) query params with type-aligned schema; PK skipped |
| Search / fields / include params | ✅ | `?search=`/`?fields=`/`?include=` with vendor extensions for the playground |
| **Pagination params** | 🟡 wrong | always `page`/`page_size` — ignores the configured paginator (Finding 2) |
| **Ordering param** | ❌ | not emitted (REST doesn't apply `?ordering=` either — see umbral-rest Finding 1) |
| **Base-path fidelity** | ❌ | CRUD paths hardcode `/api/` (Finding 1); only `@action` paths read the real base |
| Swagger UI | ✅ | bundled `swagger_ui.html`, `{SPEC_URL}` injected; trailing-slash-less mount mirror |
| Spec param customization | ✅ | `at`/`title`/`version`/`description`/`exclude` all work (matches docs-audit: openapi.mdx clean) |

**Stubs / no-ops / TODOs:** none. No `todo!()`/`unimplemented!()`/`// TODO`. Several honest "v1 mapping flattens X; a future pass can…" comments (Array item type, JSON-as-object) — documented simplifications, not stubs. The module-doc header (`lib.rs:18-21`) is **stale**: it says "v1 only describes umbral-rest's auto-generated endpoints… the spec carries no `securitySchemes` entries. Pagination is also deferred because umbral-rest does not paginate yet." All three claims are now false (plugin openapi_paths ARE merged at `:304`, securitySchemes ARE emitted at `:340`, pagination params ARE emitted at `:264`). Contributor-facing only, but misleading — fix the `//!` block.

## Findings

### NEW — Important

**1. CRUD paths hardcode `/api/{table}/`, ignoring the REST plugin's configured base path.** `build_spec` emits `format!("/api/{}/", model.table)` (`lib.rs:282`) and `format!("/api/{}/{{id}}", model.table)` (`:293`) as string literals. The REST plugin exposes its real base via the `pub fn base_path()` reader (`umbral-rest/lib.rs:396`) whose doc-comment says it's "Public for the OpenAPI plugin to read so the spec mirrors the live routes" — but openapi never calls it. Only `@action` paths use the real base (`action.base_path`, `:316-320`), so a versioned API gets an *internally inconsistent* spec: actions at `/v1/...`, CRUD at `/api/...`. Effect: `RestPlugin::default().at("/v1")` → Swagger UI "Try it" on any CRUD op hits 404; generated clients target the wrong URLs. Fix: add a `umbral_rest::base_path()` free reader (the REST instance method isn't reachable cross-plugin at spec-build time — needs the same `OnceLock`-backed free-fn pattern as `is_exposed`/`registered_action_schemas`) and thread it through `build_spec`. → file **NEW gap** (REST↔OpenAPI base-path fidelity). Severity: Important.

**2. Pagination params are hardcoded `page`/`page_size` regardless of the active paginator.** `build_spec` unconditionally pushes `pagination_parameters()` (`:264`), which emits `page` (default 1) + `page_size` (max 100, default 20) on every list op (`:829-851`). But the REST **default** is `NoPagination` (those params are inert), and `LimitOffsetPagination` uses `?limit`/`?offset`. So the spec advertises params that either do nothing or have the wrong names for two of the three built-in paginators — and the hardcoded "max 100 / default 20" doesn't match `PageNumberPagination`'s actual `page_size`/`max_page_size` (default 50, max 200). Fix: extend the `Pagination` trait with an `openapi_parameters()` method and read the active paginator's shape from REST at spec-build (same reader plumbing as Finding 1). → fold into the Finding-1 gap. Severity: Important (the spec lies about how to page every list endpoint).

### NEW — Optional

**3. Per-resource permissions aren't reflected in per-operation `security`.** The spec emits one global `security` array from the auth chain (`:339-343,355-357`) but never reads the per-table `.permission(...)` classes. A `ResourceConfig::new("audit").permission(IsStaff)` resource is documented identically to an `AllowAny` one — Swagger UI shows no lock, codegen generates no auth requirement, and a client discovers the 403 only at runtime. REST would need to expose a per-table permission descriptor (e.g. "requires auth / requires staff") for openapi to attach operation-level `security`. Severity: Optional (doc-fidelity, not exploitable). → file **NEW gap** or defer.

**4. Spec is rebuilt from the full registry on every `/openapi.json` request.** `spec_handler` (`:192-194`) calls `build_spec(cfg)` per request; `build_spec` walks every plugin × model × field × lookup with no memoization, though the spec is static after `App::build()`. Already flagged **Optional** in the perf review (perf-scalability.md:37). The playground fetches this on every load. Fix: `OnceLock<Arc<str>>` cached at first request. Severity: Optional. → already in the perf review's NEW set; not re-filing.

**5. `pascal_case` is duplicated (`umbral-openapi` + `umbral-cli`).** Confirmed `fn pascal_case` at `lib.rs:1144`. Already captured as **#77** (dedup `to_snake_case`/`pascal_case`). No new entry.

### NEW — FYI / clean

- **No security hole:** the spec correctly scrubs hidden fields (`is_hidden` for properties/required/fields-picker/include-picker), defers the block-list to REST (so `auth_user`/`session`/`permissions_*` never appear — tested at `tests/integration.rs:210`), and only documents what REST actually serves (`is_exposed` gate at `:246`). `noform`→`readOnly` keeps server-managed fields out of request bodies. No raw SQL, no DB access (pure registry walk).
- **Plugin contract: clean.** Facade-only + `umbral_rest` (a legitimate cross-plugin dep, since it depends on `rest` — declared at `dependencies() → &["rest"]`). No `umbral-core` leak, no models/migrations. Single-file 1658 LOC is large but cohesive (one concern: spec emission); not on the #78 split list and doesn't need to be — it's flat generator code, splittable into `{schema, params, paths, plugin}` if desired but low pain.
- **`openapi_type` coverage is complete** across every `SqlType` including PG-only families (Inet/Cidr/MacAddr/FullText/Decimal/Array/Bytes) with honest "v1 flattens" notes — no missing arm.

## Tests

**Thin but well-targeted (1 file, ~10 fns).** `integration.rs` boots a real App with REST+OpenAPI and asserts over the served JSON: valid 3.0 envelope, every model in `components.schemas`, every CRUD op in `paths`, block-list keeps `auth_user` out, hidden field excluded from both schema AND the `?fields=` picker, Swagger UI HTML loads + references the spec URL, base-path override changes both routes, FK/M2M-to-string-PK render as string schema. Plus ~30 unit tests in `lib.rs` covering column_schema variants (choices/multichoice/maxLength/default/fk/noform/noedit/help/example), filter-parameter emission rules, pagination-param shape, and M2M/auto_now in model_schema.

**Gaps in coverage (each maps to a finding):**
- **No test that CRUD paths track `RestPlugin::at(...)`** — the `base_path_override` test only exercises *OpenApiPlugin*'s own mount, not whether the spec's `/api/...` paths follow REST's base. This is exactly why Finding 1 is uncaught. Add: boot REST `.at("/v1")`, assert spec paths start with `/v1/`.
- **No test asserting pagination params match the configured paginator** (Finding 2) — `pagination_parameters_shape_round_trips` pins the hardcoded page/page_size shape, which *enshrines the bug*.
- **No per-resource-permission→spec test** (Finding 3).
- No spec-caching/memoization test (Finding 4) — expected (feature absent).
