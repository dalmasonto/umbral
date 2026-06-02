# umbra-playground — design spec

**Status:** approved design, ready for implementation planning
**Date:** 2026-06-02
**Scope:** MVP frontend, reuses the existing `umbra-openapi` JSON spec, no auth-aware spec extensions, no server-side persistence.

## 1. Goal

Ship a new plugin `umbra-playground` that mounts a Postman-style API playground UI at `/api/playground/`. The plugin takes its input from the existing `umbra-openapi` plugin's JSON spec, fetched at runtime over HTTP. The UI is a 3-pane React app bundled at compile time by esbuild and tailwindcss, both invoked from `build.rs`.

This is the MVP for `bugs/features.md` item 6. Auth-aware spec extensions, schema-driven form body editors, and a server-side `SavedRequest` model are explicit non-goals for v1 — they are listed in §11 as follow-up work.

## 2. Architecture

**New crate:** `plugins/umbra-playground/`, added to `crates/Cargo.toml` `[workspace.members]`.

**Dependency graph (after this change):**

```
umbra-playground  →  umbra (facade)         (no umbra-openapi dep)
umbra-openapi     →  umbra, umbra-rest      (unchanged)
umbra-rest        →  umbra                  (unchanged)
```

`umbra-core` continues to depend on no plugins. The architectural rule that the framework's center has no edges to plugin crates is preserved.

**Cross-plugin integration:** the playground reads the spec at runtime via `fetch('/openapi/openapi.json')`. It does not import the `umbra-openapi` Rust crate. This keeps the two plugins coupled at the data layer (JSON over HTTP) rather than the Rust code layer, and means the playground renders any OpenAPI 3.0 spec, not just `umbra-openapi`'s output.

**Mount points:**

- `GET /api/playground/` — returns the HTML shell.
- `GET /api/playground/assets/*` — serves files from the bundled `dist/` directory. Path-traversal-safe (resolves under `dist/`, 404s otherwise).

Both are configurable via `PlaygroundPlugin::new().at("/api/playground/")`, matching the `OpenApiPlugin::new().at(...)` shape.

## 3. Crate layout

```
plugins/umbra-playground/
  Cargo.toml
  build.rs                     # esbuild + tailwind CLI invocations
  README.md
  src/
    lib.rs                     # PlaygroundPlugin (impl Plugin)
    routes.rs                  # 2 routes
    static.rs                  # serves dist/ with traversal protection
    generated_assets.rs        # build.rs writes; lists hashed asset filenames
  frontend/
    index.html                 # shell; references hashed assets
    index.tsx                  # React entry, mounts <App />
    components/
      App.tsx
      Header.tsx
      EndpointTree.tsx
      RequestBuilder.tsx
      ResponseViewer.tsx
      AuthTab.tsx
      MethodBadge.tsx
      JsonView.tsx
      Tabs.tsx
      KeyValueTable.tsx
      EmptyState.tsx
      ErrorBanner.tsx
    state/
      store.ts                 # zustand store
      spec.ts                  # OpenAPI loader
      history.ts               # localStorage adapter
      curl.ts                  # request → curl string
      buildFetchArgs.ts        # RequestDraft → fetch args (pure)
    styles/
      tailwind.config.js
      app.css
    __tests__/
      buildFetchArgs.test.ts
      store.test.ts
      RequestBuilder.test.tsx
      ResponseViewer.test.tsx
    vitest.config.ts
  tests/
    rust_integration.rs        # boots the plugin, asserts HTML shell is served
```

**Generated, gitignored:**

```
plugins/umbra-playground/dist/playground.<hash>.js
plugins/umbra-playground/dist/playground.<hash>.css
```

## 4. Build pipeline

`build.rs` (~80 lines, no Cargo build-dependencies) does the following on every build that touches `frontend/`:

1. Check `$PATH` for `esbuild` and `tailwindcss`.
   - If either is missing, emit a `cargo:warning=...` and write a `generated_assets.rs` pointing at a static placeholder HTML. The plugin still compiles; the UI is degraded but the crate is usable.
2. Otherwise:
   - Run `esbuild frontend/index.tsx --bundle --minify --sourcemap=inline --outfile=dist/playground.<hash>.js --define:process.env.NODE_ENV='"production"'`. When `cfg!(debug_assertions)` is true, drop `--minify` and use `--sourcemap` (external) so dev builds are inspectable. The hash is derived from the *output* bundle contents, so a non-minified dev build and a minified release build produce different hashes — the served HTML always references the right one for the current profile.
   - Run `tailwindcss -c frontend/styles/tailwind.config.js -i frontend/styles/app.css -o dist/playground.<hash>.css --minify`. Same debug-vs-release distinction: drop `--minify` when `cfg!(debug_assertions)` is true.
   - Compute the content hash from the *output* filenames. The hash is in the filename so it busts caches automatically.
3. Write `src/generated_assets.rs` with `pub const JS: &str = "playground.abc123.js";` and `pub const CSS: &str = "playground.abc123.css";`.
4. Emit `cargo:rerun-if-changed=frontend/` and `cargo:rerun-if-changed=build.rs`.

**Required toolchain for full functionality:** Node 20+, `esbuild` and `tailwindcss` binaries in `$PATH`. The README documents `npm i -g esbuild tailwindcss` as the install path. A `npx` fallback is documented for users who prefer not to install globally.

**`.gitignore` updates:**

```
plugins/umbra-playground/dist/
plugins/umbra-playground/src/generated_assets.rs
```

## 5. Component model

**Top-level layout (`App.tsx`):**

```
<App>
  <Header />
  <ThreePane>           // CSS grid: grid-cols-[240px_1fr_1fr]
    <EndpointTree />    // left
    <RequestBuilder />  // center
    <ResponseViewer />  // right
  </ThreePane>
</App>
```

Resizable panes are a v2 candidate. v1 is a fixed grid.

**`<Header />`:** spec `info.title` + `info.version` (read-only) and a "Reload spec" button.

**`<EndpointTree />` (left):** `<details>`-grouped by tag, method badge + path per row, search box filters by path/method/summary (case-insensitive substring). On select, dispatches `selectEndpoint(operationId)`.

**`<RequestBuilder />` (center):**

- Top strip: method dropdown · URL input · Send button. Path-template params (segments matching `\{[a-zA-Z_][a-zA-Z0-9_]*\}` in the path) become inline inputs *below* the strip, rendered only when the selected path contains at least one template segment, not in the URL bar.
- Tabs: `Params` · `Body` · `Headers` · `Auth`.
  - **Params:** table of declared query params, plus ad-hoc rows.
  - **Body:** monospace `<textarea>` with a "Format" button (pretty-prints JSON). Schema-driven form is v2.
  - **Headers:** key/value rows. Defaults: `Content-Type: application/json`, plus any spec-declared headers.
  - **Auth (v1):** a "Bearer token" input that gets added as `Authorization: Bearer <token>` on Send. v2 will render per-scheme UIs from `securitySchemes`.

**`<ResponseViewer />` (right):**

- Status strip: status code · duration · size. Color by class (2xx emerald, 3xx amber, 4xx/5xx rose).
- Tabs: `Body` · `Headers` · `History` · `cURL`.
  - **Body:** auto-detect `application/json` → collapsible JSON tree; otherwise raw `<pre>`.
  - **Headers:** sorted read-only key/value table.
  - **History:** last 50 sends for the *current* endpoint. Click a row to restore; "Clear history" with confirm.
  - **cURL:** read-only equivalent `curl` command for the current request.

## 6. State

A single zustand store (`state/store.ts`) with `persist` middleware writing to `localStorage` under `umbra-playground:history:v1`.

```ts
interface PlaygroundState {
  // spec
  spec: OpenAPIV3.Document | null;
  specError: string | null;
  loadSpec: () => Promise<void>;
  reloadSpec: () => Promise<void>;

  // selection
  selectedOperationId: string | null;
  selectEndpoint: (id: string | null) => void;

  // current request
  current: RequestDraft;
  setMethod: (m: string) => void;
  setUrl: (u: string) => void;
  setParam: (name: string, value: string) => void;
  setHeader: (name: string, value: string) => void;
  setBody: (raw: string) => void;
  setBearerToken: (t: string) => void;

  // response
  lastResponse: ResponseRecord | null;
  inFlight: boolean;
  send: () => Promise<void>;

  // history (per-endpoint, localStorage-backed)
  history: Record<string, ResponseRecord[]>;
  clearHistory: (operationId: string) => void;
  restoreFromHistory: (operationId: string, index: number) => void;
}
```

**Why zustand:** ~3KB, no provider boilerplate, `persist` middleware handles localStorage in 3 lines. Redux Toolkit would be 5× the code; Context + `useState` would re-render the whole tree on every keystroke.

**Why no React Router:** single-screen tool. Selection is state, not a URL.

**Type safety for the spec:** `openapi-types` (npm, zero runtime) provides `OpenAPIV3.Document`, `OpenAPIV3.OperationObject`, etc.

## 7. Request execution

`buildFetchArgs(RequestDraft) → { url, init }` is a pure function. It:

1. Resolves the path template (`{id}` → value) against `current.params`.
2. Builds the query string from params where `in === 'query'`.
3. Adds headers from `current.headers` plus `Authorization: Bearer <token>` if `current.bearerToken` is non-empty.
4. Serializes `current.body` as JSON if method ∉ {GET, HEAD} and `Content-Type` is unset or `application/json`.

`fetch()` is called *from the browser* to the user's own server, same-origin. CORS is not a concern in normal setups. A CORS-proxy toggle for cross-origin APIs is a v2 candidate.

## 8. Error handling

Three categories, each with a distinct UI:

1. **Spec load errors.** `loadSpec()` catches and sets `specError`. Red banner above the tree with the message and a Retry button. The rest of the UI renders empty states.
2. **Request build errors.** `buildFetchArgs` returns `Result<FetchArgs, BuildError>`. BuildErrors are:
   - **Missing required path param:** a `parameters[].required: true` entry with `in: 'path'` whose value in `current.params` is empty.
   - **Invalid JSON in body:** `JSON.parse` throws when the body is non-empty and `Content-Type` is unset or `application/json`.
   Surfaced inline next to the offending field (red border + one-line message). Send button stays enabled.
3. **Response errors.** `fetch()` failures (network down, server crashed mid-response) are stored as a `ResponseRecord` with `status: 0, error: "..."` so history still works. The viewer shows the error in red with a Retry button.

4xx and 5xx are *valid responses*, not errors. They render with the rose status badge and the response body, matching Postman/Insomnia behavior. This avoids a "why is the UI red?" confusion class.

**History persistence edge cases:**

- Per-endpoint cap: 50 records. Older entries dropped FIFO.
- Total storage cap: 5MB (localStorage limit). If exceeded, oldest records across all endpoints are dropped.
- Versioned key (`:v1`) for forward-compat migrations.
- Save is debounced 500ms.

**CORS / session-cookie note:** a one-liner in the Auth tab documents that for session-based auth, the user must be logged into the app in the same browser.

## 9. Testing

Three layers, scaled to project conventions:

1. **Pure function tests (Vitest, no DOM):** `buildFetchArgs` is the only meaningful pure logic. ~10 cases: GET no params, POST with body, path template resolution, query string encoding, bearer token header.
2. **Component tests (Vitest + @testing-library/react):** ~5 critical interactions:
   - Selecting an endpoint populates the URL strip with the templated path.
   - 2xx response renders the JSON tree.
   - 4xx renders the body with the rose status badge.
   - History tab shows entries after a send; restoring a row repopulates the builder.
   - Auth tab's bearer token appears as `Authorization` on the outgoing request.
3. **Rust integration test (`tests/rust_integration.rs`):** boots the plugin via `tower::ServiceExt::oneshot`, asserts `/api/playground/` returns 200 and the body contains the expected shell HTML.

Vitest is invoked from `cargo test` via `build.rs`-driven `npx vitest run` in test mode. Documented `npm test` path for users who want to run JS tests in isolation.

**E2E with Playwright is a v2 candidate.** Not in v1.

## 10. Documentation

**Internal:**

- `plugins/umbra-playground/README.md` — what it is, install steps, builder shape, v1 limitations.
- This spec lives at `docs/superpowers/specs/2026-06-02-rest-playground-design.md`.

**User-facing (per project rule "ship a feature, ship its doc page"):**

- `documentation/docs/v0.0.1/plugins/_category_.json` — sidebar registration.
- `documentation/docs/v0.0.1/plugins/playground.mdx` — purpose, one example showing registration alongside `RestPlugin` and `OpenApiPlugin` (the *exact* code is finalized in the user-facing doc, not pinned here to avoid spec drift), link to this spec.

## 11. Non-goals (v1)

Explicitly deferred:

- **Auth-aware spec extensions.** v1 has a manual bearer-token input. v2 will extend `umbra-openapi` to emit `securitySchemes`/`security` from the registered `Authentication` trait, and the playground will render scheme-specific UIs.
- **Schema-driven form body editor.** v1 is a JSON `<textarea>` with Format. v2 could use a generated form from the spec's schema.
- **Server-side request history.** v1 is localStorage only. A `SavedRequest` model + migration is a future plugin.
- **CORS proxy / cross-origin API targets.** v1 is same-origin only.
- **Resizable panes.** v1 is a fixed CSS grid.
- **Playwright E2E.** Layer 3 testing is Rust integration only in v1.
- **A pre-built playground binary.** Users must run `npm i -g esbuild tailwindcss` once. Pre-bundling assets into the repo was considered and rejected: it would couple the plugin to a specific esbuild version and complicate git diffs.

## 12. Milestones

Each milestone ships as one commit. Total: 6 commits, each independently revertable.

**M1 — Plugin skeleton.** Crate scaffolds, builds, registers 0 routes. `cargo build -p umbra-playground` succeeds. Workspace members updated. Cargo.toml committed.

**M2 — `build.rs` + esbuild + tailwind + placeholder HTML.** Build pipeline proven. `/api/playground/` returns a styled "Hello, playground" page on a machine with the tools, and a degraded placeholder without. Degrades gracefully.

**M3 — Spec loader + 3-pane shell + endpoint tree.** Frontend fetches the spec, renders the tree, populates a static URL strip on select. Cross-plugin HTTP contract proven.

**M4 — Request builder + send + response viewer.** Full request execution path. All tabs functional. Response renders with status, headers, JSON tree, cURL.

**M5 — History + persistence + polish.** Per-endpoint history tab. localStorage save/load. Error states, empty states, loading states. All component tests pass. Rust integration test passes.

**M6 — Docs + example integration.** MDX page, `_category_.json`, `examples/derive-demo` extended. README finalized. A new user finds it, follows the page, has it running in 5 minutes.

## 13. Acceptance criteria

- `cargo build` succeeds with Node + esbuild + tailwindcss installed.
- `cargo build` succeeds *with a warning* (and a degraded UI) without them.
- `cargo test -p umbra-playground` passes.
- `npx vitest run` in the plugin dir passes.
- A user can register `PlaygroundPlugin::new().at("/api/playground/")` next to `RestPlugin` and `OpenApiPlugin`, and `GET /api/playground/` returns a working 3-pane UI.
- The MDX page is linked from the sidebar and renders.
- `examples/derive-demo` runs the playground end-to-end against the existing demo models.

## 14. Estimated size

~30 files. ~2000 LOC: ~250 Rust, ~1500 TS/TSX, ~200 CSS/config, ~50 doc. Bundle size: ~40KB JS + ~10KB CSS after esbuild minification + Brotli.
