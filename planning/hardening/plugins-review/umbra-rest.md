# umbra-rest — holistic review

Read-only review, 2026-06-16. Scope: `plugins/umbra-rest/src/{lib.rs (2668 LOC), filtering.rs, pagination.rs, auth.rs, permission.rs, resource.rs}` + `tests/` (23 files, ~111 test fns). All findings below are **NET-NEW** unless they reference an already-filed gap; prior-review items (CSV cap bypass #72, per-row registry clone #72, block-list-count doc, OrPermission-error doc, `Authentication→umbra-rest` boundary #76) are noted as **already-filed** and not re-counted.

## Verdict

**Strong, genuinely DRF-shaped, and safe-by-default.** umbra-rest is the most complete plugin in the tree: ViewSet-equivalent auto-CRUD, pluggable Authentication + Permission classes with sane combinators, three paginators, full django-filter grammar, `?search=`/`?include=`/`?fields=`, transactional nested writes, CSV export, and `@action` endpoints with JSON-Schema validation. The architecture is clean: facade-only imports, no `umbra-core` leak, no `sqlx::query`/`query_as` raw SQL (every row op routes through `DynQuerySet`), no migrations of its own (correct — it owns no tables). Completeness one-liner: **~90% of DRF's everyday surface ships; the real holes are `?ordering=` (reserved but never applied — a silent no-op), no throttling/versioning (deferred), and no bulk endpoints.** Worst net-new finding: **`?ordering=` is a documented-looking reserved key that the list handler never reads → clients sort and get unsorted data with no error.**

## Completeness (vs Django REST Framework)

| DRF capability | umbra-rest | Notes |
|---|---|---|
| ViewSets / auto-CRUD | ✅ | list/retrieve/create/update(PUT+PATCH)/destroy auto-mounted per model |
| `@action` (detail/collection) | ✅ | `ResourceConfig::action(name, Method, ActionScope, closure)`; trailing-slash mirror; method→405 fallthrough |
| Action input/output schema | ✅ | `action_input_schema` validated at runtime (subset validator); `action_output_schema` doc-only |
| Serializers (model-as-serializer) | ✅ | `hide`/`hide_model`/`transform`/`computed` + `ResourceConfig` ("serializers.py next to models.py") |
| Permission classes | ✅ | AllowAny/IsAuthenticated/IsStaff/ReadOnly + And/Or combinators + custom trait; **default ReadOnly** (safe) |
| Authentication classes | ✅ | trait + NoAuth/Fn/Chain; Session/Bearer live in umbra-auth (the #76 boundary) |
| Filtering (django-filter) | ✅ | full lookup grammar (eq/ne/gte/lte/gt/lt/in/contains/icontains/startswith/isnull), type-validated, choice-validated, LIKE-escaped |
| Search (`SearchFilter`) | ✅ | `?search=` ORs across searchable cols incl. FTS `@@ websearch_to_tsquery` on Postgres; `search_fields` allow-list |
| **Ordering (`OrderingFilter`)** | ❌ **MISSING** | `"ordering"` is in `RESERVED_KEYS` (filtering.rs:65) but **no handler ever reads `?ordering=`** — see Finding 1 |
| Pagination | ✅ | NoPagination (default) / PageNumber / LimitOffset; client page-size clamp; COUNT skip on NoPagination |
| `?fields=` sparse fieldset | ✅ | shipped, nested-projection-aware, depth-capped (gap #81) |
| `?include=` (select_related) | ✅ | shipped, batched IN(), FK-graph-validated, depth-capped, recursion-safe overrides |
| Nested writes | ✅ | **transactional now** (`insert_json_in_tx` + `tx.commit()`); FK auto-wired; closes orm_fixes #2 |
| Content negotiation | 🟡 partial | JSON always; CSV via `?format=csv`. No XML/`.xlsx`; no `Accept`-header negotiation (only `?format=`) |
| Bulk endpoints | ❌ | no bulk create/update/delete; `bulk_create` exists on the ORM but isn't surfaced |
| Throttling / rate-limit | ❌ | none. No gaps2 #46 entry exists; backlog only sketches a future `umbra-ratelimit` plugin (gaps2 #10 middleware-slots) |
| Versioning | ❌ deferred | `at("/v1")` gives URL-prefix versioning, but no header/accept/namespace versioning — **gap #108 (open)** |
| Error envelope | ✅ | DRF-flat field errors + `non_field_errors` + machine `code`; dev-only 404 endpoint discovery; DB text never echoed (WEB-5) |
| `.resource()`/`.resources()` config | ✅ | single + batch; additive per-table merge |
| API root / browsable index | ✅ | `/api/` lists resources + every plugin's `api_endpoints()` |

**Stubs / no-ops / TODOs found:** one — the `ordering` RESERVED_KEYS entry (Finding 1). No `todo!()`, no `unimplemented!()`, no `// TODO` in `src/`. One `#[allow(dead_code)]` on `FilterClause::into_condition` (filtering.rs:94) — already noted in static-analysis review.

## Findings

### NEW — Important

**1. `?ordering=` is reserved but never applied — sorts silently no-op.** `filtering.rs:65` lists `"ordering"` in `RESERVED_KEYS` (so the filter parser skips it instead of 400ing it as an unknown field), and the comment at `:56-57` claims it's "consumed elsewhere (… + ordering)". It is **not**: the `list` handler (`lib.rs:1706-1772`) parses filters, search, include, fields, format, and pagination, but never reads `params.get("ordering")` and never calls `.order_by(...)` on the queryset. `DynQuerySet` has `order_by_col` (used by admin). Effect: a client that sends `?ordering=-created_at` (the DRF/Django muscle-memory spelling, which `RESERVED_KEYS` deliberately accommodates) gets **unsorted rows and no error** — the worst failure mode (looks like it worked). This is also why the OpenAPI spec emits no ordering param. Fix: implement `?ordering=field,-field2` in the list handler against `model.fields` (reject unknown columns with 400, like filters do), then advertise it as an OpenAPI param. → file **NEW gap** (REST ordering). Severity: Important (silent-wrong-result on a standard DRF query param the code half-wired).

**2. OpenAPI CRUD paths hardcode `/api/...`, ignoring `RestPlugin::at(...)`.** (Cross-plugin, surfaces from the REST side: `base_path()` is `pub` on `RestPlugin` at `lib.rs:396` *specifically* so the spec can mirror live routes — its doc-comment says exactly that.) But `umbra-openapi/lib.rs:282,293` build collection/item paths with `format!("/api/{}/", model.table)` — a hardcoded literal. Only `@action` paths read the real base (`action.base_path`, openapi:316-320). So `RestPlugin::default().at("/v1")` produces a spec whose CRUD paths say `/api/post/` while the server serves `/v1/post/` — Swagger UI "Try it" hits 404. Fix: expose the REST base_path through a `umbra_rest::base_path()` free reader (parallel to `is_exposed`/`registered_action_schemas`) and use it in `build_spec`. → file **NEW gap**. Severity: Important (spec/route divergence breaks the playground for any versioned/re-based API). *Logged primarily in the openapi report; noted here because the fix needs a new reader on the REST side.*

### NEW — Optional

**3. OpenAPI pagination params are always `page`/`page_size`, regardless of the configured paginator.** `openapi/lib.rs:264` unconditionally emits `pagination_parameters()` (page/page_size, capped 100, default 20) on every list op. But the **default** paginator is `NoPagination` (which ignores those params), and `LimitOffsetPagination` uses `?limit`/`?offset` — so the spec documents query params that do nothing (NoPagination) or the wrong names (LimitOffset). The REST plugin doesn't expose the active paginator's param shape to openapi. Fix: have `Pagination` advertise its OpenAPI params and read them at spec-build. Severity: Optional (misleading docs, not a security/data issue). → fold into the Finding-2 gap.

**4. CSV writer errors are swallowed.** `rows_to_csv` (`lib.rs:1818-1821`) ends `.ok().and_then(|b| String::from_utf8(b).ok()).unwrap_or_default()`, and per-record `wtr.write_record(...)` results are `let _ =` discarded (`:1813,1816`). A serialization failure yields a **silently truncated or empty CSV with a 200 OK** — the consumer can't tell a partial export from a complete one. The perf review already flags the cap bypass (#72); this is the orthogonal *correctness* leg the backlog's "REST CSV writer errors dropped" line (P1) refers to. Fix: propagate the writer error as a 500 (`ApiError::Sqlx`/a new `Internal`). Severity: Optional→Important if CSV exports are load-bearing. → fold into **#72** (CSV path already owned there) or the existing P1 "REST CSV writer errors dropped" line.

**5. `From<sqlx::Error>` maps every `Protocol(_)` to 400 BadInput.** `lib.rs:1454` treats `sqlx::Error::Protocol(_)` as client `BadInput`, and `sqlx_err_clone`/the `WriteError::Sqlx` fallthrough (`:1487,1502`) *manufacture* `Protocol(stringified)` errors for genuine server-side failures. So a real infra error that happens to be (or gets re-wrapped as) `Protocol` surfaces to the client as a 400 with the stringified DB message — both a wrong status and the WEB-5 "don't echo DB text" concern the `Sqlx` arm exists to prevent. The `non-validation → Protocol(e.to_string())` round-trip at `:1487` is the load-bearing offender. Fix: carry a dedicated `Internal(String)` variant instead of reusing `Protocol` as a 400 sentinel. Severity: Optional (narrow trigger). → file **NEW gap** or fold into a REST error-taxonomy cleanup.

### NEW — FYI / clean

- **Custom-action default permission is correctly safe.** `view_exposed` returns `true` for `Custom` (so actions aren't blocked by `views(...)` scope), but the permission gate still runs (`gate` → `permission_for` → default `ReadOnly`). `ReadOnly::check` denies `Custom(_)` (permission.rs:178-184, `is_read()`==false), so an `@action` on a resource with no explicit `.permission(...)` returns **403 by default** — writes-are-opt-in holds for actions too. Good.
- **Plugin contract: clean.** Facade-only (`umbra = path`), no `umbra-core` import, no raw `sqlx::query`/`query_as` (every row op via `DynQuerySet`; the only `sqlx` use is the `Error` type in the error-translation layer). No `models()`/migrations — correct, REST owns no tables. `q_seg` asserts no `{}/?#` in route segments (defense-in-depth past the `is_action_name_char` gate).
- **Architecture / split (already #78):** `lib.rs` is 2668 LOC and mixes builder, error envelope, handlers, CSV, include-parser, action-dispatch. Already a split candidate under #78 — confirmed, no new entry. Cohesive sub-modules: `{builder, errors, handlers, csv, include, action_dispatch}`.
- **Mass-assignment is closed (WEB-2):** `strip_hidden_for_write` runs before create/update so a `hide`d field can't be set via POST/PATCH. `apply_overrides` recurses into `?include=` nested objects so a hidden field can't leak through a relation (verified `:769-815`).

## Tests

**Coverage is good (23 files, ~111 fns)** and behavioral, not assert-only: real rows through the public HTTP path, read-back of the object graph (`integration.rs`, `nested_writes.rs`, `m2m_writethrough.rs`, `constraint_errors.rs`, `field_overrides.rs`). Specific strengths: `default_safe_permission.rs` (anonymous POST→403), `actions_gated.rs`, `action_schemas.rs`, `csv_export.rs`, `boolean_round_trip.rs`/`auto_now.rs` (serialization edge cases), `rest_fts_pg.rs`/`search_pg.rs` (Postgres FTS), `nested_overrides.rs` (recursion). Unit suites for allow/block, sparse-fields (15 cases), pagination envelopes, choice validation.

**Gaps in coverage:**
- **No `?ordering=` test** — which is *why* Finding 1 went unnoticed: nothing exercises the reserved key, so the silent no-op never tripped a test. Adding the feature must come with a round-trip test (sort asc/desc + unknown-column 400).
- **No test asserting OpenAPI paths track `RestPlugin::at(...)`** — Finding 2 is uncaught because the openapi integration test (`openapi/tests/integration.rs`) boots REST at the default `/api` only. A test booting `.at("/v1")` and asserting the spec's CRUD paths would catch the hardcode.
- **No CSV-error-path test** (Finding 4) — `csv_export.rs` covers the happy path only.
- No throttling/versioning/bulk tests — expected (features absent).
