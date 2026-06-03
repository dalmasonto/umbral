# Playground introspection + Dexie migration

**Date:** 2026-06-03
**Status:** Implemented
**Scope:** `plugins/umbra-playground/`, `plugins/umbra-openapi/`

## Context

The playground v1 shipped a working request/response loop over a localStorage history. Two follow-on asks landed at once: emulate the DRF browsable API (show model fields with their FKs, choices, multichoices, filter affordances) and replace localStorage with Dexie/IndexedDB for persistence.

These two changes are independent in code but share one thing: both surface latent gaps in the umbra-rest / umbra-openapi data contract. The introspection feature is only as rich as the OpenAPI document. The Dexie work is unblocked the moment we accept a small async-boundary cost.

## Decisions

### 1. Extend `umbra-openapi` to emit choices + maxLength + default + readOnly + vendor extensions

The `Column` struct (in `umbra-core/migrate.rs`) carries `choices`, `choice_labels`, `fk_target`, `max_length`, `default`, `noedit`, `is_string_repr`, and `is_multichoice` — but `column_schema()` in `umbra-openapi` was only emitting `type` + `format` + `nullable`. Surfacing the rest unlocks substantial Swagger UI and playground value without changing any other layer.

Choice fields (`enum`), `maxLength`, `default`, and `readOnly` use **standard** OpenAPI keys. FK target, multichoice, `__str__` marker, and per-position choice labels use **`x-umbra-*` vendor extensions**, which the OpenAPI 3.0 spec explicitly reserves for tool-specific data. Choosing extensions here (rather than re-mapping FKs to a `$ref`) was deliberate: a future pass can promote FK to a proper `$ref` without breaking the playground, but the vendor extension is the right interim signal.

Multichoice columns deliberately **do not** emit a flat `enum`: the wire value is a CSV subset of the choices, not one choice. Emitting an enum there would mislead generated clients. Instead, `x-umbra-multichoice: true` plus `x-umbra-choices: [...]` lets aware tooling render the right widget.

Six unit tests in `plugins/umbra-openapi/src/lib.rs` pin the shape per column kind (plain text, choices, multichoice, FK, noedit, maxLength+default).

### 2. The playground infers filter affordances from the response item schema

umbra-rest's runtime filters (`?status=draft`, `?title__contains=foo`) are not declared in OpenAPI today (see `bugs/improvements.md` #1). Rather than block the playground feature on that bigger fix, the frontend infers what's filterable by walking the **list response item schema**:

- `listItemSchema()` in `lib/openapiSchema.ts` recognises the `{results: [Item], count}` envelope, resolves `Item` through `components.schemas`, and returns the introspected fields.
- `RequestBuilder` renders a "Suggested filters" panel above the Params table whenever the method is GET and a list item schema is detected. Each scalar field becomes a row of clickable chips: `+ = eq`, `+ contains`, `+ icontains`, etc. — filtered by the field's type and enum-ness.

Clicking a chip pushes a new param row into the existing params editor; no new state surface, no new persistence. When the REST plugin eventually publishes filter parameters in OpenAPI, the playground will already render them via the existing declared-params table — the inferred chips can stay as a "discover more" affordance or be deprecated.

### 3. Schema tab in RequestBuilder, not ResponseViewer

The schema (request body shape + response shapes) is **input** information — what to send, what to expect — so it belongs alongside the request builder's params/body/headers/auth tabs, not after the response. The Response viewer already shows the actual response; duplicating the declared schema there would split attention.

`SchemaTable` is a single reusable component (`components/SchemaTable.tsx`) rendered three times in the Schema tab: once for the request body, once per response status code. It consumes the `FieldInfo` list produced by `fieldInfosFromSchema()`, so adding a fourth call site later is a one-liner.

### 4. Dexie for history, localStorage for settings

The user asked for Dexie generally, but the right answer differs per data set:

- **History** (unbounded growth, large records with embedded response bodies) — IndexedDB / Dexie. The previous 5MB localStorage cap was already enforced via `enforceCaps()`; removing it lets the playground actually remember a long session. Per-operation cap (50 records) stays as the eviction policy.
- **Settings** (~1KB structured config, must be available synchronously at store init) — localStorage stays. Making `loadSettings()` async would cascade into the zustand store's init pattern; the cost is high and the data fits localStorage by an order of magnitude.

The interface change is one boundary: `loadHistory()` went from sync to `Promise<...>`. The single call site (in `App.tsx`'s mount effect) became `void loadHistory().then(...)`. `saveHistoryDebounced()` keeps its sync signature because callers fire-and-forget; the debounced timer now `void persistHistory()`s into Dexie.

### 5. One-shot localStorage → Dexie migration

`migrateFromLocalStorage()` runs at the top of every `loadHistory()` call but is gated on `db.history.count() === 0`. On the first run for an existing user, it reads the legacy `umbra-playground:history:v1` blob, bulkAdds the records, and removes the localStorage key. Subsequent loads short-circuit (legacy key absent). This is best-effort: if the legacy blob is unparseable, drop it; the in-memory history still works.

## Consequences

- **REST plugin gains 6 OpenAPI keys** — additive, no breaking change.
- **Playground reads richer specs** — Schema tab + filter chips fall back gracefully when the keys are missing.
- **History persistence is now async-first** — one new file (`state/db.ts`), one rewritten file (`state/history.ts`), one effect hook touched (`App.tsx`).
- **`bugs/improvements.md`** captures the remaining REST/OpenAPI gaps the playground surfaces (filter parameter emission, FK `$ref`, securitySchemes, descriptions, examples, pagination) — all out of scope for this pass but documented for the next round.

## Things deliberately deferred

- **Filter parameter emission in umbra-openapi.** The frontend covers the gap for now. Proper fix requires `RestPlugin` to publish per-resource filter config to `OpenApiPlugin` — a new plugin-to-plugin seam that deserves its own spec.
- **History click-to-replay.** Logged as improvement #7.
- **Total history record cap.** Per-op cap survives the migration; a global cap (improvement #11) is the next obvious eviction tightening.
- **Nested schema drill-down.** `→ ProfileRef` shows the target name; clicking through is improvement #8.
