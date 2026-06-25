# Review: umbral-livereload

Read-only audit, 2026-06-16. Scope: `plugins/umbral-livereload/src/lib.rs` (the only source file). Cross-referenced against `planning/hardening/backlog.md`, `reviews/security.md`, and `reviews/performance-scalability.md`.

NET-NEW items only. No previously-filed entry covers livereload specifically.

---

## Verdict

**Complete and well-implemented for its stated scope.** The plugin does exactly what it says: file-watcher → SSE → browser reload, CSS hot-swap, boot-id reconnect detection, auto-injected client script, hard-gated to `Environment::Dev`. No `todo!()`s, no stubs, no no-ops. Implementation quality is high — the debounce, the denylist classifier, the `no-store` cache header, and the `beforeunload` clean-close are all present.

**Worst finding:** The `inject_client` middleware buffers the entire response body into memory before injecting the snippet (`lib.rs:273`). For a dev server this is unlikely to matter, but it is a blanket `usize::MAX` cap on body size with no guard.

**Pre-existing fmt note (as flagged in the prompt):** There is no non-canonical line visible in `lib.rs` as-read. The file is canonically formatted. If `cargo fmt` flagged something, it may have been in a prior version already resolved or in a different crate — the current file shows nothing.

---

## Completeness

| Area | Status |
|---|---|
| Dev-only gating | Complete. `Plugin::routes`, `Plugin::wrap_router`, and `Plugin::on_ready` all guard on `is_dev()` / `Environment::Dev`. |
| File watching | Complete. `notify::recommended_watcher` with recursive/non-recursive mode per path; graceful skip for nonexistent paths. |
| Debounce | Complete. 90ms window; CSS-only vs reload distinction preserved across the burst. |
| CSS hot-swap | Complete. Client `bustCss()` replaces stylesheet `href` without reload. |
| Client script injection | Complete. `Plugin::wrap_router` mounts `inject_client` middleware; injects before `</body>` with append fallback. |
| Boot-id restart detection | Complete. `BOOT_ID` (nanosecond timestamp) sent as first SSE frame; client reloads on mismatch. |
| Prod gating | Complete. No route, no watcher, no injection outside `Dev`. |
| `.rs` file handling | Complete. Denylist ignores `.rs`/`.lock`/`.rlib`/`.rmeta`/`.d` — handled by `umbral dev` rebuild path. |
| Keep-alive | Complete. 15s SSE keep-alive on the stream. |
| Stubs / todo | None found. |

---

## Findings

### LR-1 — `inject_client` buffers the full response body with `usize::MAX` cap (NEW)

**Severity: Important**

`lib.rs:273`: `axum::body::to_bytes(body, usize::MAX).await` collects the entire response body before injecting the `<script>` snippet. This means:
- A template that renders a large page (or an accidentally large dev payload) is fully buffered in memory before any bytes reach the browser. In dev this rarely matters but it is an unconstrained allocation.
- A streaming response (SSE from another handler, chunked file downloads) would be fully consumed here, losing the streaming property.

The error path at `lib.rs:277` is correctly handled (returns an empty body rather than panicking), but the success path has no size guard.

**Fix:** Add a reasonable cap, e.g. `axum::body::to_bytes(body, 32 * 1024 * 1024).await` (32 MiB). If the body exceeds the cap, return it unmodified (no injection). This is safe because `inject_client` only fires in Dev; an oversized response is an unusual dev-time edge case, and skipping injection on it is acceptable.

**Gap:** NEW.

---

### LR-2 — `WATCHER` static holds the watcher but `BUS` TX is cloned into the debounce task (NEW)

**Severity: Nit**

`lib.rs:409`: the `notify::RecommendedWatcher` is stored in `WATCHER` (a `OnceLock<Mutex<...>>`) to keep it alive. However the `Mutex` is never locked after being set — the watcher just needs to be alive, and `Mutex` carries unnecessary overhead (and a potential panic on poisoning if the watcher thread panics while the lock is held). A plain `OnceLock<notify::RecommendedWatcher>` with the value moved in would be cleaner, but `RecommendedWatcher` may not be `Sync`, hence the `Mutex` wrapper. No correctness issue; just unnecessary ceremony.

**Gap:** None.

---

### LR-3 — `is_dev()` called at route/middleware registration time, not at request time (NEW)

**Severity: Nit**

`lib.rs:116-119` and `lib.rs:122-125`: `is_dev()` is called inside `Plugin::routes()` and `Plugin::wrap_router()`, both of which run during `App::build`. If settings are not yet installed at the time `routes()` is called (e.g. when a test constructs the plugin directly and calls `.routes()` without an `App::build`), `get_opt()` returns `None` and the function defaults to `false` — no route is registered and no middleware is mounted.

This is the intended defensive behaviour (`lib.rs:155-159`), and it correctly handles the case. But the comment says "defensive — `App::build` sets them before plugin hooks run", meaning the only real risk is a test that calls `plugin.routes()` outside `App::build`. The existing tests do not exercise the dev-path routes directly, so there is currently no test coverage of the `is_dev() == true` route-registration path.

**Gap:** None (FYI).

---

### LR-4 — Injected client script has no XSS surface, but `data` attribute carries user-controlled path (FYI)

**Severity: FYI**

The injected `CLIENT_SNIPPET` (`lib.rs:200-241`) is a fixed static string with no user input interpolated into it. The `change` event's `data` (`{"type":"css"}` or `{"type":"reload"}`) is a hardcoded JSON literal assembled in the debounce task — no user-controlled file path reaches the SSE frame body. The `classify` function (`lib.rs:326-356`) returns only the boolean; the path itself is never sent to the client. No injection surface found.

---

### LR-5 — `spawn_watcher` silently exits if the debounce channel is dropped (FYI)

**Severity: FYI**

The debounce task (`lib.rs:414-437`) exits when `evt_rx.recv()` returns `None` — i.e., when the notify callback's `evt_tx` is dropped. The callback's `evt_tx` lives inside the closure captured by the watcher, which in turn lives in `WATCHER`. As long as `WATCHER` is set, the channel is live. If `WATCHER` is not set (because all `dirs` were skipped at `lib.rs:405-408`), `evt_tx` is dropped when the watcher variable goes out of scope at `lib.rs:381`, the task exits immediately, and the bus produces no messages. This is correct behaviour (and `is_dev()` logging already warns), but it is a silent exit rather than an explicit early-return. No correctness issue.

---

## Plugin-contract

- **Facade-only imports:** Clean. `lib.rs:7-10` imports `umbral::plugin::{AppContext, Plugin, PluginError}` and `umbral::web::{Router, get}` — all through the `umbral` facade. No `umbral-core` internal or sibling plugin import.
- **Migrations:** None. No persisted schema. Correct.
- **`Plugin` impl:** Complete. `name()`, `routes()`, `wrap_router()`, and `on_ready()` all present and correctly gated.
- **`axum` import:** `lib.rs:243` uses `axum::middleware::from_fn` and `axum::...` directly — consistent with other plugins that list `axum` as a direct dep.

---

## Tests

| Test | File | Covers |
|---|---|---|
| `injects_before_closing_body` | `lib.rs:444-454` | Snippet placement before `</body>`, content preservation |
| `appends_when_no_body_tag` | `lib.rs:456-461` | Fallback append path |
| `classify_routes_css_vs_reload_vs_ignore` | `lib.rs:463-481` | All extension/path categories including edge cases |

**Gaps:**
- No integration test for the SSE endpoint (`/__umbral/livereload`).
- No test for `inject_client` middleware end-to-end (body buffering + injection on a real HTTP response).
- No test for `spawn_watcher` / debounce (file-watcher → bus → SSE frame). Would require either a real filesystem or a mock notify; currently untested.
- No test that `Plugin::routes()` returns an empty router in non-Dev environments.
- The dev-path `on_ready` (watcher init, BUS/BOOT_ID set) is untested — it requires `Environment::Dev` in settings.
