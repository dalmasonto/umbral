# review_4 · umbral-playground

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 1,128 LOC (`plugins/umbral-playground`) |
| **Reviewer** | review_4 slice agent (static-only) |
| **Verdict** | healthy |
| **Findings** | 0 critical · 0 high · 0 medium · 1 low · 1 info — 2 new, 4 known |

## Summary

- The plugin renders one page — the playground shell at `GET {base}/` — and registers the pre-built bundle under the framework's unified static pipeline. One route, no state, no DB, no proxy: the SPA runs in the browser and calls the app's own API with the visitor's cookies. That small surface is why this review is short.
- It is notably disciplined about its own history: the naive sequential-`replace` escaping (hardening PG-1) is now a documented single-pass scan; the prod gate is an explicit empty router with a warning; and `asset_prefix` is snapshotted from `settings.static_url` so rendering is deterministic and CDN-prefixed.
- One LOW finding: both doc comments claim the inline-script injection is render-safe via escaping, but `json_escape` is bare `serde_json::to_string`, which does not escape `<` or `/` — so an `app_name` (or spec URL) containing `</script>` breaks out of the shell's inline script. Developer-controlled input only; the crate's own test documents that `<` passes through literally.
- One INFO: `json_escape`'s `unwrap_or_else` fallback is dead code that would silently blank the per-app storage scope if it ever fired.
- Known items dominate: PG-2 (silent missing-openapi fallback), PG-4 (committed `dist/`), PG-5 (no auth on the shell) remain open; PG-1 is fixed and verified held.
- Fix first: only PLAY-1 matters, and it is a two-line escape (or a corrected doc) — everything else is tracked exposure posture for staging environments.

## Findings

### PLAY-1 · LOW · docs/security-hygiene · self-verified
**The shell's escape docs promise a breakout-proof inline script, but `json_escape` is bare `serde_json::to_string` — which escapes neither `<` nor `/` — so an `app_name` (or registered spec URL) containing `</script>` terminates the script element and injects markup into the shell.**

- **Where**: `plugins/umbral-playground/src/routes.rs:66-69` and `:148-156`; `src/shell.html:14-23`; `tests/rust_integration.rs:343-366`.
- **Evidence**:
  ```rust
  // routes.rs:66-69 — the render doc's claim
  /// per-app scope. The app name is HTML-attribute-escaped (quotes +
  /// `<>` + `&`) so an exotic project slug can't break out of the
  /// meta-tag attribute or the inline script.
  ```
  ```rust
  // routes.rs:148-156 — the json_escape doc claims `/` escaping; the body does not do it
  /// JSON-string escape — quotes the value inline so the inline
  /// `<script>` tag carries `window.__UMBRAL_PLAYGROUND_APP__ =
  /// "<value>"` with backslash-escaping for the dangerous chars
  /// (`"`, `\`, `/`, control chars). Backed by `serde_json::to_string`
  /// for correctness; the result includes the surrounding double
  /// quotes.
  fn json_escape(s: &str) -> String {
      serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
  }
  ```
  `serde_json` escapes `"`, `\` and control characters — not `/` and not `<`. The value lands raw inside `<script>` (`shell.html:15`: `window.__UMBRAL_PLAYGROUND_APP__ = __APP_NAME_JSON__;`), and the HTML tokenizer ends a script element at any `</script` byte sequence regardless of JS string state. The crate's own test pins the behaviour: `tests/rust_integration.rs:362-365` asserts the rendered line is `window.__UMBRAL_PLAYGROUND_APP__ = "my\"shop & <test>";` — `<` survives literally.
- **Impact**: `PlaygroundPlugin::new("</script><script>fetch('/api/user/',{'method':'DELETE'})</script>")` — or the same sequence inside the OpenApiPlugin spec URL, which `render_shell` JSON-injects via the same helper (`routes.rs:85-86`) — renders a shell where attacker-positioned markup executes in the app origin, with the visitor's cookies, for every person who opens the playground. The input is developer-set source configuration (or a misconfigured option), so no hostile visitor can trigger it: this is a doc-vs-code defect and a defense-in-depth gap, not a live XSS. That calibration is why it is LOW, not CRITICAL.
- **Fix**: make the claim true — script-context escaping in `json_escape` (`<` → `<`, `/` → `\/`, or uppercase `<` escape after `serde_json`) — or narrow both doc comments. The hardening review's PG-3 already asked for a `</script>` regression test; add it alongside.

### PLAY-2 · INFO · correctness
**`json_escape`'s error fallback is unreachable dead code that would silently blank the storage scope if it ever fired.**
- **Where**: `plugins/umbral-playground/src/routes.rs:154-156`.
- **Evidence**: `serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())` — serializing a `&str` cannot fail, so the `unwrap_or_else` arm is dead; and its chosen fallback (`""`) is the silent-degradation shape: were it ever reached, `window.__UMBRAL_PLAYGROUND_APP__` would quietly become the empty string and gap #71's per-app storage namespacing would collapse with no signal. Observation only — no action required, but if the helper is ever touched, `expect("serializing a str cannot fail")` is the honest form.

## Strengths

- **Prod gate with a warning**: in `Environment::Prod` without `allow_in_prod`, `routes()` returns an empty router and logs (`lib.rs:166-177`) — the crate warns rather than relying on the caller's memory, and `allow_in_prod`'s doc (`lib.rs:115-125`) states exactly what the opt-in re-exposes.
- **`single_pass_replace`** (`routes.rs:105-130`): a hand-rolled left-to-right scan whose emitted value is never rescanned, closing the recursive-replace hazard hardening PG-1 found in the old chained `.replace` — with the invariant stated in the docstring and the safety argument spelled out at the UTF-8 boundary.
- **`html_escape_attr`** (`routes.rs:135-146`) escapes all five attribute-dangerous characters (`& < > " '`), and the test asserts the exact escaped `content="..."` output.
- **Deterministic rendering**: `asset_prefix` is snapshotted from `settings.static_url` at route-build time into `PlaygroundState` (`routes.rs:30-38`), so a CDN override flows through and `render_shell` performs no ambient lookups (gaps2 #53) — with a test (`shell_follows_configured_static_url`).
- **Graceful degradation**: `build.rs` emits a `cargo:warning` and a placeholder page when npm is unavailable, so the Rust build never fails on a missing Node toolchain; `m2_build.rs` asserts the generated constants exist.
- **No server-side proxy**: the shell injects a spec URL and nothing else — REST calls are made by the browser, same-origin, with the visitor's own cookies. There is no credential-injection point and no SSRF surface on the server (the design choice the focus areas probe for).
- **Tests exercise the real router and assert exact response bodies** (`oneshot` + `to_bytes` over `Plugin::routes()`), including the escape test that made PLAY-1 verifiable.

## Clean areas checked

- CSRF: the router has exactly one route — `GET {base}/` (`routes.rs:184-186`) — and the shell handler (`routes.rs:159-167`) only renders; no state-changing endpoint exists to protect.
- Authentication: the plugin itself carries no auth logic and no credentials; exposure is controlled by the prod gate, with the staging-environment gap tracked as PG-5.
- Injection: single-pass substitution kills the token-recursion vector; attribute context is fully escaped; the JSON/script context is PLAY-1's caveat, not a separate issue.
- Panics: the only `unwrap()` is `Response::builder()...body(...).unwrap()` (`routes.rs:166`) over a constant header set — unreachable failure.
- No `unsafe`, no DB, no raw SQL (the crate has no database surface at all); deps are `umbral` facade + axum + static-pipeline types only.
- `allow_in_prod` doc, degraded-mode placeholder, and `at()` mount are all documented in the crate; the missing behavioural test for the prod gate and `at()` remains a known test gap (see Known).

## Known issues already tracked elsewhere (not counted in the totals)

- `hardening/plugins-review/umbral-playground.md` PG-1 — naive chained `.replace` into HTML `[fixed/verified held]` — the chain is now `single_pass_replace` (`routes.rs:87-96`), emitted values never rescanned.
- `hardening/plugins-review/umbral-playground.md` PG-2 — missing-openapi fallback is silent `[open]` — `registered_openapi_spec_url().unwrap_or("/openapi/openapi.json")` (`routes.rs:85`) still serves a shell pointing at a non-existent spec with no boot warning; the suggested `on_ready` warn is not implemented.
- `hardening/plugins-review/umbral-playground.md` PG-3 — escaping assessment `[partially held — see PLAY-1]` — the test-side conclusion about `<test>` is right, but its closing "cannot break out of the script block" is false for a `</script>` sequence, which is exactly what the doc comments promise is handled (PLAY-1).
- `hardening/plugins-review/umbral-playground.md` PG-4 — `dist/` is committed to git `[open]` — verified still present at `plugins/umbral-playground/dist/` (`assets/`, `favicon.svg`, `index.html`); each Vite build mints new hashed filenames, so history accumulates bundles.
- `hardening/plugins-review/umbral-playground.md` PG-5 — no auth/rate-limit on the shell; `allow_in_prod` is the only guard `[open]` — the shell route still carries no `require_staff`-style gate.
- `hardening/plugins-review/umbral-playground.md` test gaps — no behavioural tests for the `allow_in_prod` mount, `at()`/trailing-slash normalisation, or a token-in-`app_name` (incl. `</script>`) `[open]`.

## Out-of-scope notes

- PG-2's proper fix (distinguishing "OpenApiPlugin not installed" from "registry not populated yet" at boot) touches core's registry — `umbral::routes::registered_openapi_spec_url()` in `crates/umbral-core/src/routes.rs`; the warn-at-boot half lands in playground's `on_ready`, the registry contract half belongs to the core reviewer.
