# review_4 · umbral-livereload

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 628 LOC (`plugins/umbral-livereload`) |
| **Reviewer** | Claude (rev4 wave) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 2 medium · 1 low · 2 info — 5 new, 2 known |

## Summary

- Dev-only live reload: a `notify` OS file watcher pushes `{"type":"css"|"reload"}` over an SSE endpoint, and a middleware injects a static client script into every `text/html` response; stylesheets hot-swap via a `?v=` cache-bust, everything else reloads. 628 LOC, all in one file (unit tests inline; no `tests/` directory).
- Prod posture is correct: `routes()`, `wrap_router()` and `on_ready()` each return early outside `Environment::Dev` (`:132-147`, `:149-152`), so the documented "leave the plugin line in `main.rs` unconditionally" holds.
- Both medium findings are in the *client/injection* half of the dev loop: injecting into a response that a plugin inside this one already compressed corrupts the page (LIVE-1), and the CSS cache-bust strips required query strings — the admin's Google Fonts link gets rewritten to a 400 on every CSS save (LIVE-2).
- The watcher/client design is strong: SharedWorker single-connection transport, boot-id reconnect protocol, denylist classification that deliberately ignores `.rs`, 90 ms debounce, and a static (non-interpolated) injected script with no XSS surface.
- Nothing here can affect production; the fixes are small and dev-scoped. Remaining test gaps are already tracked in the hardening review (Known).

## Findings

### LIVE-1 · MEDIUM · correctness · self-verified
**`inject_client` appends the script to any `text/html` response without checking `Content-Encoding`; when a compression layer sits inside it, the gzip body is corrupted and `Content-Encoding: gzip` is left in place — the dev page fails to decode.**
- **Where**: `plugins/umbral-livereload/src/lib.rs:361-384`; trigger `plugins/umbral-cache/src/lib.rs:1109-1113`; ordering `crates/umbral-core/src/app.rs:1714-1715`
- **Evidence**:
  ```rust
  let is_html = res
      .headers()
      .get(CONTENT_TYPE)
      .and_then(|v| v.to_str().ok())
      .map(|c| c.contains("text/html"))
      .unwrap_or(false);
  ```
  ```rust
  // Body length changed; drop the stale Content-Length so axum recomputes.
  parts.headers.remove(CONTENT_LENGTH);
  axum::response::Response::from_parts(parts, axum::body::Body::from(injected))
  ```
  ```rust
  // Compression (outermost so the body is already compressed before any
  // header-setter above runs on the response on the way out).
  if h.compression {
      router = router.layer(CompressionLayer::new());
  }
  ```
- **Impact**: Trace (static; needs runtime confirmation): a Dev app registers `CachePlugin::with_compression()` **before** `LiveReloadPlugin` (plugin `wrap_router` layers compose later-applied = outermost, per `app.rs:1714-1715` "Later plugins wrap earlier ones"). On the response path the cache layer's `CompressionLayer` runs first (inner), gzip-compressing the HTML; livereload's `inject_client` runs after (outer), sees `Content-Type: text/html`, buffers the gzip bytes, lossy-decodes them, appends the script, removes only `Content-Length` — and keeps `Content-Encoding: gzip`. The browser tries to gunzip script-appended text and the page breaks. Note the seam is order-dependent: core's `AppBuilder::compression()` is applied after the plugin loop (`app.rs:1878-1880`), so it is outermost and safe — only a compression layer added *inside* the plugin chain (the cache plugin's) triggers this.
- **Fix**: In `inject_client`, bail out of body rewriting when `Content-Encoding` is present (the `no-store` header can still be set); a decompress-modify-recompress path is not worth it for a dev tool. A regression test with a compressing inner layer would pin it.

### LIVE-2 · MEDIUM · correctness · self-verified
**`bustCss()` strips existing query strings when cache-busting every stylesheet, rewriting links whose query is load-bearing — the admin's Google Fonts link becomes a 400 URL on any CSS save.**
- **Where**: `plugins/umbral-livereload/src/lib.rs:289-294`; victim `plugins/umbral-admin/templates/wrapper.html:58`
- **Evidence**:
  ```javascript
  function bustCss() {
    document.querySelectorAll('link[rel="stylesheet"]').forEach(function (l) {
      var base = (l.href || "").split("?")[0];
      if (base) l.href = base + "?v=" + Date.now();
    });
  }
  ```
  ```html
  <link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&display=swap" rel="stylesheet"/>
  ```
- **Impact**: Trace (static): in Dev, saving any `.css` publishes `{"type":"css"}`; the client runs `bustCss()` over every stylesheet. For the admin font link, `split("?")[0]` yields `https://fonts.googleapis.com/css2`, and the rewrite produces `…/css2?v=<ts>` — Google returns 400 (the `family` parameter is required), so Inter silently falls back to system fonts until a full reload. Any querystring-versioned asset (`app.css?v=3`) degrades the same way. The failure is silent in the UI (no console error) and hits every admin page in the default dev loop.
- **Fix**: Preserve the query — `l.href = l.href.split('#')[0] + (l.href.indexOf('?') >= 0 ? '&' : '?') + 'v=' + Date.now();` — so existing parameters survive the cache-bust.

### LIVE-3 · LOW · error-handling polish
**The body-read failure arm silently returns an empty body with no log — a dev page can go blank with zero trace.**
- **Where**: `plugins/umbral-livereload/src/lib.rs:372-377`
- **Evidence**:
  ```rust
  Err(_) => return axum::response::Response::from_parts(parts, axum::body::Body::empty()),
  ```
- **Impact**: If `to_bytes` fails (mid-stream body error), the response is replaced by an empty one and the error is discarded — the developer sees a blank page and has nothing to grep for. The other half of this arm's design, buffering with `usize::MAX`, is already tracked (hardening LR-1, Known); the silent `Err(_)` is the untracked part. Rare path, dev-only — LOW, not higher.
- **Fix**: `tracing::warn!` the error in the arm (dev-only crate, zero cost) and/or fall back to passing the original response through untouched when the body cannot be buffered.

### LIVE-4 · INFO · drift/observation
**Two independent dev gates for one fact.**
- **Where**: `plugins/umbral-livereload/src/lib.rs:132-147` + `:174-178` (`is_dev()` via ambient settings) vs `:149-152` (`ctx.settings.environment` in `on_ready`)
- **Observation**: `routes()`/`wrap_router()` decide via `umbral::settings::get_opt()` and `on_ready` decides via its `ctx`. They agree today (settings are initialized at `app.rs:1404`, before the plugin loop at `:1714`), but the same fact now has two sources — a future change to one gate half-mounts the plugin (e.g. injection without a watcher, or vice versa). Consolidating on `ctx` where available and one helper would remove the hazard.

### LIVE-5 · INFO · hygiene
**Unused dependency `http = "1"`.**
- **Where**: `plugins/umbral-livereload/Cargo.toml:19`
- **Observation**: Every HTTP type in the crate is referenced through `axum::http::…`; the direct `http` dependency has no users. Drop it (or import through it) so the dependency list matches reality.

## Strengths

- **The dev gate is honored everywhere.** All three request-surface-visible behaviors (SSE routes, injection, watcher) early-return outside `Environment::Dev`; the README/MDX claim that a prod build "mounts none of it" is structurally true (dependency present, behavior absent).
- **No injection/XSS surface.** `CLIENT_SNIPPET` is a static `const` string with zero interpolation; the changed file path never reaches the client (the bus carries only the two fixed JSON frames plus the boot id), which matches the hardening review's LR-4 reading.
- **Client transport design.** SharedWorker holding one `EventSource` across tabs (with a per-tab fallback) solves the per-host connection-limit failure mode; the boot-id reconnect protocol cleanly covers the rebuild→restart case; both invariants are pinned by inline tests (`worker_holds_a_single_eventsource_and_fans_out`).
- **Watcher discipline.** Denylist `classify()` (documented as intentionally not an allowlist) ignores `.rs`/lockfiles/target/VCS/editor noise precisely so a source edit waits for `umbral dev`'s rebuild instead of reloading against a dying process; 90 ms debounce collapses fs-event bursts into one message; missing watch paths and watcher-init failure degrade with warnings rather than breaking boot.
- **Injection mechanics are careful where they apply.** `inject_into_html` prefers the last `</body>` and appends otherwise (tested both ways); the stale `Content-Length` is dropped so axum recomputes; the dev-only `no-store` header has its rationale documented and is only mounted in Dev.
- **Honest docs.** `live-reload.mdx` describes the denylist, the `.rs`-is-ignored split, the CSS hot-swap limits and the prod posture exactly as implemented.

## Clean areas checked

- Command execution / data exfil via the watcher — none; only fixed JSON frames are sent, files are watched but never read.
- XSS via injection — static const script, no user data; path never echoed (hardening LR-4 confirmed).
- Prod mount-by-accident — three independent early returns; no route string is registered outside Dev.
- Panics on the request path — no `unwrap`/`expect` in `sse_handler`, `worker_handler`, `inject_client`; `bus.send` errors are ignored with a stated reason (no subscribers yet).
- SSE endpoint auth — unauthenticated by design and acceptable: Dev-only, read-only, carries only boot id + reload signal; the audit_2 gating stamp lists livereload as correctly Dev-gated.
- Unbounded channels — the `evt_tx` unbounded channel is fed by the OS watcher thread and drained by the debounce loop; worst-case growth is bounded by real fs-event rate, and the hardening review's LR-1 covers the related buffering question.
- Raw SQL — not applicable (no DB); `grep sqlx::query`: zero hits.

## Known issues already tracked elsewhere (not counted in the totals)

- `planning/hardening/plugins-review/umbra-livereload.md` LR-1 — `to_bytes(body, usize::MAX)` buffers the whole HTML body with no cap — `[open]` (LIVE-3 covers the untracked silent-error half of the same arm).
- `planning/hardening/plugins-review/umbra-livereload.md` (Tests) — no SSE-route test, no `inject_client` e2e test, no watcher/debounce test, no non-Dev empty-router test — `[open]` (the inline unit tests cover injection purity, worker invariants and `classify` only).
- `planning/hardening/plugins-review/umbra-livereload.md` LR-3 — `is_dev()` evaluated at registration time — `[fixed/verified held]` (verified: `settings::init` at `app.rs:1404` precedes the plugin `routes`/`wrap_router` loop at `:1714`, so the gate sees the real environment; the residual two-gate drift is LIVE-4).
- `gaps2 #96` — SharedWorker one-connection design (realtime, transport lineage) — `[fixed/verified held]` (client + worker tests pin the behavior).
- `gaps3 #28` (SC-5) — `notify 6 → 8` supply-chain bump for the watcher, plus the Dev-only note — `[fixed/verified held]` (Cargo.toml has `notify = "8"`).

## Out-of-scope notes

- `plugins/umbral-cache/src/lib.rs:1109-1113` — its `CompressionLayer` placement is individually correct (compress before outer header-setters); LIVE-1 is filed against livereload's injection contract and cites the cache site only as the trigger — routed to the cache reviewer for cross-check.
- `crates/umbral-core/src/app.rs:1878-1880` — core `AppBuilder::compression()` applies after the plugin wrap loop, so it is outermost and safe with livereload; no action, recorded so the ordering analysis is not redone.
- `umbra_website` dogfoods `.plugin(LiveReloadPlugin::new().watch("plugins"))` (per `gaps2-done.md` #62) — the multi-root/`plugins/*/templates` watch path has no test; covered by the tracked Tests entry above.
