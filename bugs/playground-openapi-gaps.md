# Playground-surfaced REST/OpenAPI improvements

Captured 2026-06-03 while wiring DRF-style introspection into umbra-playground. The playground exposed which schema metadata the REST + OpenAPI plugins currently omit; this file is the punch-list to close the loop.

## Done in this pass

- [x] **Column choices → OpenAPI `enum`.** Closed-set columns (`ArticleStatus = Draft | Review | Published | Archived`) now render as `"enum": ["draft", ...]` on the property schema. Swagger UI, generated clients, and the playground all benefit. Skipped for multichoice (the value is a CSV of the set, not one of the set).
- [x] **`max_length` → OpenAPI `maxLength`.** Standard validation key surfaces in the spec.
- [x] **`default` → OpenAPI `default`.** Standard hint that Swagger renders inline.
- [x] **`noedit` → OpenAPI `readOnly`.** PUT/PATCH consumers can now grey out the field.
- [x] **Vendor extensions for playground-rich UX.** Added `x-umbra-fk-target`, `x-umbra-multichoice`, `x-umbra-choices`, `x-umbra-choice-labels`, `x-umbra-string-repr` — none break OpenAPI 3.0 compatibility (extensions are scoped to `x-*` by spec); they let the playground (and any future custom tool) build richer affordances without re-introspecting the model registry. Covered by 6 new unit tests in `plugins/umbra-openapi/src/lib.rs`.

## Still open — REST/OpenAPI

1. **List endpoints don't declare filter parameters.** `ResourceConfig::enable_filters()` exposes `?status=draft`, `?title__contains=foo`, etc. at runtime but the OpenAPI spec doesn't enumerate them. Clients (and Swagger UI) can't discover which columns are filterable, which lookups are supported, or what types the filter values should be.
   - **Workaround in playground:** the playground now infers filter affordances from the model schema (`listItemSchema()` in `lib/openapiSchema.ts`) — for each scalar field, suggest `field=`, `field__contains=`, `field__gte=`, etc. as clickable chips. This works today because list responses are envelope-shaped (`{results: [Item], count}`) and the item schema is rich enough to drive lookup picking.
   - **Proper fix:** in `umbra-openapi`, when a `ResourceConfig` has filters enabled, emit each filterable column × each lookup as an OpenAPI `parameters` entry on the GET list operation. Requires the OpenAPI plugin to read `ResourceConfig` (currently only walks `migrate::registered_plugins()` → `ModelMeta`); needs a new accessor on `RestPlugin` to expose its per-model config.

2. **FK columns surface as `int64` with no relationship hint.** `x-umbra-fk-target` (added this pass) names the target *table*, but the OpenAPI standard idiom is to point at the target *schema* with a `$ref`. Until that's wired, generated clients (openapi-generator, orval, etc.) can't navigate from an `Article` to its `User`.
   - **Fix sketch:** when emitting an FK column, instead of `{"type": "integer", "format": "int64"}`, emit `{"$ref": "#/components/schemas/User/properties/id"}` — or define a `FK<User>` reusable component. Either way needs the target table → schema-name mapping that `build_spec()` already computes.

3. **No pagination parameters declared.** umbra-rest doesn't paginate yet (per `plugins/umbra-openapi/src/lib.rs` doc), but when it does, list responses will need `?page`/`?limit`/`?offset` parameters in the spec.

4. **No `securitySchemes` block.** `AuthPlugin` registers cookie + session auth backends; OpenAPI is silent about them. Swagger UI's "Authorize" button doesn't appear, and clients can't generate auth-aware code. Needs a hook on `RestPlugin` (or a callback on the OpenAPI plugin) to publish which schemes are active.

5. **No column descriptions.** `Column` has no `description: Option<String>` field. Django's `help_text=` would map naturally. Adding `#[umbra(help = "...")]` and threading it through `FieldSpec` → `Column` → OpenAPI `description` would make Swagger UI vastly more readable.

6. **No examples.** OpenAPI `example` / `examples` are missing. Could be auto-generated from `default` (already in `Column`) for primitives, or from a new `#[umbra(example = "...")]` attribute.

## Still open — playground frontend

7. **History tab: per-row click-to-replay.** A history row is informational only. Clicking should re-populate the request builder with that record's exact request (URL, params, headers, body, auth). Needs a `replay(record)` action on the store.

8. **Schema tab: nested object navigation.** When a request body field is a `$ref` to another schema (e.g. `User.profile -> Profile`), the schema table shows `→ Profile` but you can't drill in. Either render nested tables inline (with a depth limit) or add a navigation breadcrumb.

9. **Filter chips need value pickers.** Today, clicking `+ __in` adds an empty `status__in=` row. For `enum` fields we have the choice list, so the value cell could be a multi-select. For booleans, a Yes/No toggle. Right now everything is a plain text input.

10. **Per-record history delete.** The store only exposes `clearHistory(operationId)` (whole op). A `deleteHistoryRecord(opId, timestamp)` action plus a row-level trash icon would close the polish gap that the misleading per-row trash hinted at.

11. **History total cap.** With Dexie/IndexedDB the 5MB localStorage ceiling is gone, but there's no upper bound. A long-running playground session could accumulate 10,000+ records. Reasonable cap: 5,000 records total, evict oldest. Add to `state/history.ts:persistHistory`.

12. **Bulk import/export of saved requests.** Export the current settings + collection as JSON; re-import in another browser. Useful for team sharing without a server.
