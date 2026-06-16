# Review: umbra-playground

Read-only audit, 2026-06-16. Scope: `plugins/umbra-playground/src/` (lib.rs, routes.rs), `build.rs`, `tests/` (m2_build.rs, rust_integration.rs). Cross-referenced against `planning/hardening/backlog.md`, `reviews/security.md`, and `reviews/performance-scalability.md`.

---

## Verdict

**A real plugin, but the "belongs as a built-in" question deserves an honest answer: conditionally yes.** The playground is an interactive REST API explorer for `umbra-rest` — the framework's equivalent of DRF's Browsable API or Swagger UI (but opinionated/React-based). It is shippable and not a scratchpad. It has a sensible prod-gating story (`allow_in_prod` opt-in), escape-safe app-name injection, a graceful placeholder build path, and a clean `Plugin` implementation.

**Why it is controversial as a built-in:** It carries a React/Vite frontend that must be built with `npm` before the plugin is useful. The `build.rs` degrades gracefully to a placeholder, but any contributor who lacks Node.js gets a non-functional playground silently. It also has no direct equivalent in the plugin-contract checklist (no models, no migrations, no routes beyond the shell) — it is purely a dev/DX tool. Its only real runtime value is in conjunction with `umbra-rest` and `umbra-openapi`, which it is not declared to depend on at the Cargo level (it reads the OpenAPI URL via a registry call at route-build time).

**Honest recommendation:** Keep it as a built-in plugin (it is not "just" a scratchpad — the code is complete and the UX motivation is real), but document the Node.js build dependency more prominently, and consider making `umbra-openapi` / `umbra-rest` an optional Cargo-level soft-dep (currently it reads the OpenAPI URL via a registry call that returns a fallback if openapi is not installed).

**Worst finding:** `app_name` flows into an inline `<script>` tag. The JSON-escape (`serde_json::to_string`) is correct, but the double-rendering through `SHELL_HTML.replace("__APP_NAME_JSON__", &app_js)` is a string-replace into an HTML context — if `app_js` itself contains `__APP_NAME_JSON__` (as a literal string in the app name), the replace would iterate. This is an edge-case XSS breakout vector. See PG-1.

---

## Completeness

| Area | Status |
|---|---|
| HTML shell at `/api/playground/` | Complete. `GET {base_path}/` renders the shell with hashed asset references. |
| Vite bundle build pipeline | Complete. `build.rs` runs `npm run build`, mirrors `frontend/dist/` → `dist/`, writes `generated_assets.rs`. |
| Graceful degradation (no npm) | Complete. Falls back to `PLACEHOLDER_HTML` with instructions. |
| Prod gating | Complete. `allow_in_prod: false` default; `routes()` returns empty router in Prod. |
| Per-app storage scope | Complete. `app_name` injected as `<meta>` + `window.__UMBRA_PLAYGROUND_APP__`, escaped. |
| Static asset serving | Complete. Via `Plugin::static_dirs()` → framework unified static pipeline. |
| CDN/`static_url` support | Complete. `asset_prefix` snapshotted from `settings.static_url` at route-build time. |
| `umbra-openapi` spec URL | Partial — see PG-2. Read via registry call with hardcoded fallback. |
| `umbra-rest` dependency | Implicit — the playground is useless without REST + OpenAPI but has no Cargo dep on either. |
| Stubs / todo | None found in Rust code. |

---

## Findings

### PG-1 — `SHELL_HTML.replace(...)` is naive string-replace into HTML (XSS edge-case) (NEW)

**Severity: Important**

`routes.rs:87-92`:

```rust
SHELL_HTML
    .replace("__CSS_PATH__", &css)
    .replace("__JS_PATH__", &js)
    .replace("__APP_NAME_ATTR__", &app_meta)
    .replace("__APP_NAME_JSON__", &app_js)
    .replace("__OPENAPI_URL_JSON__", &spec_url_json)
```

Each `.replace` substitutes a placeholder token with a value. The escape functions (`html_escape_attr` and `json_escape`) are individually correct. However, the chain is not idempotent or isolated:

1. **Recursive-replace hazard:** If `css` (an asset path like `/static/playground/assets/index-Bus4dlCi.js`) happens to contain the literal string `__APP_NAME_JSON__` — however unlikely from the Vite hash — the subsequent `.replace("__APP_NAME_JSON__", ...)` would replace it. The probability is negligible for machine-generated hashes but non-zero for a user-controlled `app_name` if `app_name` itself contains `__CSS_PATH__` etc. Example: `PlaygroundPlugin::new("__OPENAPI_URL_JSON__")` would cause the `OPENAPI_URL_JSON` replacement to expand into its own slot.

2. **`spec_url` is partially controlled:** `spec_url` comes from `umbra::routes::registered_openapi_spec_url()` which reads from the route registry — a framework-internal value, not user input. But the fallback string is hardcoded (`"/openapi/openapi.json"`), and if the openapi plugin somehow registered a URL containing a placeholder token, the replace chain would misbehave.

**Fix:** Use a single-pass substitution function (a regex replace with a callback, or a hand-rolled O(n) scan for `__PLACEHOLDER__` tokens) so each token is substituted exactly once, regardless of what the substitution values contain. Alternatively, adopt a minimal template engine (even `format!` with an indexed approach) that doesn't scan the already-substituted output.

**Gap:** NEW.

---

### PG-2 — `registered_openapi_spec_url()` called at route-build time; boot-race if OpenAPI plugin registers after playground (NEW)

**Severity: Optional**

`routes.rs:85`: `let spec_url = umbra::routes::registered_openapi_spec_url().unwrap_or("/openapi/openapi.json");` is called inside `render_shell`, which is called per-request (inside the `shell` handler at `routes.rs:122-130`). The comment in the code says "falls back to the historical default when OpenApiPlugin isn't installed OR the registry isn't populated yet (boot-time race, shouldn't happen in practice since Plugin::routes() runs in dependency order...)".

The boot-time race concern is acknowledged. The per-request call means the URL is correctly read after all plugins are initialized. But the comment's "shouldn't happen" is not enforced: if an app registers `PlaygroundPlugin` without `umbra-openapi`, every `/api/playground/` request silently serves a shell pointing at a non-existent `/openapi/openapi.json` endpoint, and the playground fails to load the spec with no visible error.

**Fix:** In `on_ready` (or at the end of `routes()`), check if `registered_openapi_spec_url()` returns `None` and log a `tracing::warn!("umbra-playground: no OpenAPI spec URL found — is umbra-openapi installed? The playground will not function.")`. This makes the missing-openapi misconfiguration visible at boot rather than silently at runtime.

**Gap:** NEW.

---

### PG-3 — `app_name` with XSS-dangerous characters escapes correctly in the attribute but the `<meta>` test is fragile (NEW)

**Severity: Nit**

`tests/rust_integration.rs:340-360`: The test `shell_scope_escapes_dangerous_chars` uses `r#"my"shop & <test>"#` as the app name and asserts:
- `content="my&quot;shop &amp; &lt;test&gt;"` — correct HTML attribute escaping.
- `window.__UMBRA_PLAYGROUND_APP__ = "my\"shop & <test>";` — correct JSON string escaping.

The escaping is correct. However the test's second assertion uses a raw string literal that does NOT assert the `<` and `>` are HTML-escaped in the JSON context — `<test>` appears literally in the JSON string, which is correct (JSON strings do not require HTML-escaping `<>`), but a reader might worry it represents an XSS surface in the inline `<script>` tag.

It is not an XSS surface: the `<script>` tag contains `window.__UMBRA_PLAYGROUND_APP__ = <json-string>;` and `<test>` inside a JS string literal cannot break out of the script block (a `</script>` sequence would, but `<test>` alone is safe). The test is correct. This is a clarifying comment opportunity, not a bug.

**Gap:** None.

---

### PG-4 — `build.rs` wipes `dist/` on every successful Vite build; git-tracked `dist/` accumulates stale hashed files otherwise (NEW)

**Severity: Optional**

`build.rs:166-176`: Before mirroring `frontend/dist/` → `dist/`, the script deletes the current contents of `dist/`. This is correct for preventing hash accumulation. However the `dist/` directory itself is committed to git (visible in the file tree: `dist/assets/index-Bus4dlCi.js`, `dist/assets/index-C9L6ovjc.css`, woff2 fonts, etc.). Each Vite build produces new hashed filenames; if a contributor builds the frontend and commits the changed `dist/`, git history accumulates large binary blobs.

**Fix:** Add `dist/assets/` (or the whole `dist/` below a canonical skeleton) to `.gitignore` for `plugins/umbra-playground/`. Commit only a minimal skeleton (`dist/.gitkeep` or nothing) and document that contributors run `npm run build` to populate it. The `build.rs` graceful-degradation path already handles the empty-`dist/` case correctly. Alternatively, keep the committed snapshot as the "pre-built for CI without npm" story, but then document the canonical update procedure.

**Gap:** NEW.

---

### PG-5 — No auth/rate-limit on `/api/playground/` itself; `allow_in_prod` opt-in is the only guard (NEW)

**Severity: Important**

The playground shell route (`routes.rs:143-149`) carries no authentication. In `Dev` (the default), any client that can reach the server can load the playground and send arbitrary REST requests with the visitor's ambient session cookies. The prod-gating (`allow_in_prod: false`) prevents mount in `Environment::Prod`, which is the primary defense.

The gap is in staging/review environments that are configured as `Dev` but exposed to the internet (common in CI review apps). The playground offers a clickable console against the live API with no login requirement.

**Fix:** Add an optional `require_staff()` builder (`PlaygroundPlugin::new("app").require_staff()`) that gates the shell route behind `is_staff` using the same `require_staff` guard from `umbra-admin`. When mounted in non-localhost `Dev`, consider logging a boot warning "playground is publicly reachable without authentication".

**Gap:** NEW.

---

## "Does it belong as a built-in?" — honest assessment

**Yes, with caveats.**

The plugin is not a scratchpad. It provides the interactive REST explorer that every DRF user reaches for (`/api/` in Django), implemented correctly with:
- Prod gating by default (unlike DRF's browsable API, which ships on in all environments and must be explicitly disabled).
- App-name storage scoping (the `gap #71` fix is already shipped).
- A graceful no-npm degradation path so the Rust build never fails.
- Clean `Plugin` contract with no side-effects outside its own routes/static dir.

The concerns:
- **Node.js build step is unusual for a Rust plugin.** Contributors without Node.js see a placeholder, not an error — acceptable but should be prominent in the docs.
- **No declared Cargo dep on `umbra-rest` or `umbra-openapi`.** The playground is useless without them, but the Cargo manifest lists neither. This makes the dependency graph misleading (a CI that builds `umbra-playground` alone would produce a seemingly-working plugin that renders an empty spec).
- **It is dev/DX tooling, not application logic.** The admin plugin at least provides real CRUD UI an app uses. The playground is only useful for exploring the REST API, which puts it closer to `umbra-livereload` (dev tool) than to `umbra-auth` (core functionality). If the goal is "thin core, plugin-heavy", the playground is legitimately thin — it contributes nothing to the runtime beyond a shell page. It belongs alongside `umbra-openapi` as "optional developer tooling."

**Recommendation:** Keep it. Flag `umbra-openapi` as a soft peer-dependency in the README and docs.

---

## Plugin-contract

- **Facade-only imports:** Complete. `lib.rs:9` uses `umbra::prelude::*` which imports `Plugin`, `StaticDir`, etc. `routes.rs:17` uses `axum::...` directly (allowed — listed as a Cargo dep). No `umbra-core` internals or sibling plugin imports.
- **Migrations:** None. Correct — the playground has no persisted schema.
- **`Plugin` impl:** Complete. `name()`, `routes()`, and `static_dirs()` all present. `on_ready()` not needed — state is built at route-build time.
- **Prod gating:** Correctly implemented in `routes()` (not in `on_ready()`, which is the right hook to use for routes, per the Plugin contract).

---

## Tests

| Test | File | Covers |
|---|---|---|
| `build_pipeline_runs` | `tests/m2_build.rs` | Compile-time: `generated_assets.rs` was included and both constants exist |
| `shell_returns_200_html` | `tests/rust_integration.rs:61-83` | Shell route returns HTML |
| `static_dirs_maps_playground_to_dist` | `tests/rust_integration.rs:89-103` | `static_dirs()` namespace + source dir contract |
| `shell_points_assets_at_static_pipeline` | `tests/rust_integration.rs:114-155` | Asset URLs point at `/static/playground/assets/` (skips if no build) |
| `shell_follows_configured_static_url` | `tests/rust_integration.rs:165-201` | CDN prefix override |
| `vite_css_resolves_through_static_pipeline` | `tests/rust_integration.rs:210-243` | Real file served via static pipeline |
| `every_dist_asset_resolves_through_static_pipeline` | `tests/rust_integration.rs:249-289` | Fonts + entry chunks all resolve |
| `missing_asset_returns_404_through_pipeline` | `tests/rust_integration.rs:294-305` | 404 for absent asset |
| `shell_injects_per_app_scope` | `tests/rust_integration.rs:311-336` | `<meta>` + window global present |
| `shell_scope_escapes_dangerous_chars` | `tests/rust_integration.rs:339-361` | XSS escape of app name |

**Gaps:**
- No test for `allow_in_prod` behaviour (prod gating returns empty router).
- No test for the naive string-replace hazard (PG-1) — an `app_name` containing `__CSS_PATH__` etc. is not in the test suite.
- No test for the missing-openapi warning (PG-2) when `registered_openapi_spec_url()` returns `None`.
- No test for the `at(path)` builder or trailing-slash normalisation.
- `shell_scope_escapes_dangerous_chars` tests `<` and `>` in the JSON context but does not test `</script>` specifically (the breakout sequence from an inline script tag). Worth adding.
