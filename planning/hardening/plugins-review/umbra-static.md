# umbral-static — Holistic Review

> Date: 2026-06-16 · Read-only audit

## Verdict

`umbral-static` is a lean, correct plugin that delivers the two most-important static-file shapes: filesystem serving via `tower_http::ServeDir` and compile-time-embedded assets via `include_dir`. The plugin contract is clean (facade-only imports, correct `Plugin` impl, no circular deps), the `collectstatic` command is properly contributed as a plugin command rather than a framework built-in, and the unified-pipeline integration via `defers_to_pipeline()` + `static_root_dirs()` correctly avoids the axum double-nest-catch-all panic. Security posture is acceptable for a framework that documents "put a reverse proxy in front for prod" - path traversal is blocked in depth by `resolve_under_root` (lexical reject + canonicalize + containment), and the embedded path is structurally safe via in-memory tree lookup. The worst real finding is that `EmbeddedDirService` never emits `ETag` or `Last-Modified`, so every request to an embedded asset returns a full 200 even for clients holding a fresh copy - wasted bandwidth on every page load in browser. A content-hashed cache-busting manifest and on-the-fly compression (GZip, Brotli) are absent, which is acceptable for v0 but sets a ceiling on prod fitness for the filesystem path.

## Completeness (vs a full static-files subsystem)

| Static-files capability | umbral-static status | Notes |
|---|---|---|
| `collectstatic` command | **Complete** | Contributed via `Plugin::commands()`. Supports `--clear`. Handles namespaced plugin dirs + site root dirs. Warns on missing source, aborts on namespace collision before any copy. Idempotent re-runs. |
| Dev serving (runserver auto-mounts) | **Complete** | `StaticPlugin::new(mount, dir)` mounts `ServeDir`; `StaticPlugin::embedded(mount, dir)` mounts `EmbeddedDirService`. Both available without `App::build`. |
| Prod serving (filesystem) | **Partial** | Wraps `ServeDir` correctly; no precompressed variant support (no `.gz`/`.br` sidecar serving). No `X-Content-Type-Options: nosniff` header. |
| Unified pipeline (`static_url` as single mount) | **Complete** | `defers_to_pipeline()` + `static_root_dirs()` prevents double-catch-all panic; live source in dev, collected root in prod. |
| Template `static()` global | **Complete (in core, not this plugin)** | `umbral-core/src/templates.rs:274` registers `static()` as a minijinja global that resolves against `static_url`. Already filed as docs gap in `helpers.mdx` — **already #66**. |
| Finders (filesystem finder, app-dirs finder, custom finders) | **MISSING** | a pluggable static-file-finders abstraction (pluggable discovery strategies) has no equivalent. The two shapes (filesystem + embedded) are enough for v0. Not stubbed. |
| File hashing / content-hashed manifest | **MISSING** | No content hash in URLs, no manifest JSON, no `static('foo.css')` → `/static/foo.abc123.css` rewrite. `collectstatic` copies files as-is. Cache-busting relies entirely on `max_age` settings - a deploy without `--clear` + long max-age will serve stale assets. Not stubbed. |
| On-the-fly compression (GZip, Brotli) | **MISSING** | `ServeDir::precompressed_gzip()` / `precompressed_brotli()` are not wired in. The `tower-http` feature `fs` is enabled but only `ServeDir::new(dir)` is called. No `.gz` / `.br` sidecars produced by `collectstatic`. |
| `static_url` / `static_root` settings | **Complete** | `settings.static_url` (default `/static/`) and `settings.static_root` (default `staticfiles/`) cover the standard static-URL and static-root settings. `static_url` normalisation (leading slash, trailing slash) is implemented and tested. |
| ETag / conditional requests (filesystem) | **Complete (via ServeDir)** | `tower_http::ServeDir` handles `If-Modified-Since`, `If-None-Match`, `Last-Modified`, and emits `304 Not Modified`. Range requests (`Range` → `206 Partial Content`) also delegated to ServeDir. |
| ETag / conditional requests (embedded) | **MISSING** | `EmbeddedDirService` returns `200 + body` on every GET regardless of `If-None-Match` / `If-Modified-Since`. No `ETag`, no `Last-Modified`, no `304` ever emitted. |
| `X-Content-Type-Options: nosniff` | **MISSING** | Neither the filesystem path nor the embedded path sets `nosniff`. `tower_http::ServeDir` does not add it. `umbral-media` adds it explicitly; umbral-static does not. |
| Media files (`MEDIA_URL`, `MEDIA_ROOT`, upload serving) | **Out of scope** | Owned by `umbral-media`. Correct split. |

## Findings

**[NEW] [Required] `EmbeddedDirService` never emits ETag/304 — every request pays full bandwidth** — `plugins/umbral-static/src/lib.rs:441–472`

`EmbeddedDirService::call()` returns `200 + full body` unconditionally. It never sets `ETag`, `Last-Modified`, or reads `If-None-Match` / `If-Modified-Since`. A browser holding the asset in cache can't revalidate; it always re-downloads the full payload. For embedded assets (CSS, JS, fonts baked into the binary) the bytes never change between deploys, so a stable `ETag` (e.g. a hash of the bytes computed at compile time, or even the bytes' address as a `&'static` pointer) would eliminate re-downloads entirely. **Fix shape:** compute `ETag: "<hex-of-sha1-or-stable-id>"` at construct time (embedded bytes are `'static`, so the content is known at startup); in `call()` check `If-None-Match` against it and return `304 Not Modified` with empty body + `ETag` header when it matches. The `Last-Modified` approach is possible (binary mtime), though `ETag` is simpler and more reliable for embedded content. Note: axum's top-level router DOES set `Content-Length` from the body's `size_hint` and strips the body on HEAD (verified in `axum-0.8.9/src/routing/route.rs:166-171`), so HEAD is technically correct — only ETag/304 is missing.

**[NEW] [Required] `collectstatic` copies symlinks as files but does not detect symlink loops** — `crates/umbral-core/src/static_files.rs:651–689`

`copy_tree()` uses `file_type().is_dir()` to decide whether to recurse. A directory symlink (`is_dir()` returns `true` on a symlink-to-dir) causes infinite recursion if the symlink points back to an ancestor directory in the tree (a loop). `std::fs::read_dir` follows directory symlinks; there is no loop detection (`visited` set, `depth` cap, or follow-symlinks flag). In most real deployments this won't trigger, but it's a foot-gun for any user who has symlinks in their static source dirs. **Fix shape:** pass `follow_links: false` / use `symlink_metadata` to detect symlink-to-dir and copy them as files (or just skip with a warning), OR maintain a `HashSet<PathBuf>` of visited canonical dirs and abort with an error if a candidate is already in the set.

**[NEW] [Important] No `X-Content-Type-Options: nosniff` header on static responses** — `plugins/umbral-static/src/lib.rs:274–295`, `crates/umbral-core/src/static_files.rs:292–318`

Neither the filesystem `ServeDir` path nor the embedded `EmbeddedDirService` path nor the unified `serve_file()` path adds `X-Content-Type-Options: nosniff`. Older browsers (IE/Edge legacy, some Safari versions) will MIME-sniff a response when the content-type doesn't match what they expect, which can turn a misidentified file into active content. `umbral-media` explicitly sets `nosniff` on its serve route (`plugins/umbral-media/src/lib.rs:481-485`). Static files are intentionally authored (not user-uploaded), so the XSS risk is low compared to media, but parity with `umbral-media`'s security posture is the right baseline. **Fix shape:** add `SetResponseHeaderLayer::if_not_present(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"))` to the `cache_layer` construction in `routes()`, and add the same to `serve_file()` in `umbral-core`.

**[NEW] [Important] `ServeDir` serves dot-files (`.env`, `.git`, `.htpasswd`) without restriction** — `plugins/umbral-static/src/lib.rs:275–283`

`tower_http::ServeDir` only blocks `..` path traversal and absolute paths. It does **not** block access to files whose names start with `.`. If a user mounts `StaticPlugin::new("/static", ".")` (the project root) or any directory that happens to contain `.env`, `.git/config`, or `.htpasswd`, those files are served verbatim. The embedded path is safe (the `include_dir!` macro is explicit about which directory is embedded at compile time; the developer controls the baked tree). The filesystem path has no equivalent protection. **Fix shape:** add a `filter` or wrap `ServeDir` with a response-intercepting layer that checks whether any path component starts with `.` and returns 404 before opening the file. A lexical check before calling `ServeDir` (reject any component starting with `.`) is sufficient and can be done as a thin `tower::Service` wrapper. Document the limitation clearly if not fixed now.

**[NEW] [Important] dev-mode `max_age` override is documented and coded but has zero test coverage** — `plugins/umbral-static/src/lib.rs:197–211`, `plugins/umbral-static/tests/cache_headers.rs`

`effective_max_age()` has a branch that forces `max_age=0` when `settings.environment == Environment::Dev`. The `cache_headers.rs` test-file comment (lines 8–11) says "Tests that exercise it set the settings OnceLock up-front" — but inspecting every test in the file, zero tests actually initialize the settings `OnceLock` with `Environment::Dev`. All four tests exercise the "settings not initialised → use configured value as-is" branch. The dev-mode override is therefore completely untested. **Fix shape:** add a test that calls `umbral::settings::init_test(Settings { environment: Environment::Dev, .. })` (or equivalent), constructs `StaticPlugin` with a non-zero `max_age`, builds the router, makes a request, and asserts `Cache-Control: public, max-age=0`.

**[NEW] [Optional] Filesystem mode: no opt-in to precompressed sidecars (`gzip`, `brotli`)** — `plugins/umbral-static/src/lib.rs:275–282`, `Cargo.toml:14`

`tower_http::ServeDir` has `precompressed_gzip()` and `precompressed_brotli()` methods; `collectstatic` could produce `.gz`/`.br` sidecars alongside the originals. Neither is wired. For a production single-binary deployment (the documented use case), serving uncompressed 50 KB CSS / JS when the client signals `Accept-Encoding: br, gzip` is a meaningful overhead. **Fix shape:** add `.precompressed_gzip()` / `.precompressed_br()` as optional builder methods (`StaticPlugin::precompressed_gzip(self) -> Self`), and document that `collectstatic` does not yet produce sidecars (that's a separate task).

**[NEW] [Optional] `defers_to_pipeline()` is evaluated twice per `App::build`** — `plugins/umbral-static/src/lib.rs:228–237`

`routes()` calls `self.defers_to_pipeline()` (line 250) and `static_root_dirs()` calls it again (line 306). Each call does two ambient `get_opt()` reads plus a string normalization. At build time this is negligible, but the logic is duplicated and the two calls can theoretically observe different states if settings mutate between calls (pathological, but possible in tests that race). **Fix shape:** pre-compute `defers` in `routes()` and cache the result in the struct (set during `Plugin::routes` call, read by `static_root_dirs`), or accept it as a cosmetic nit that doesn't matter in practice.

**[NEW] [Nit] `EmbeddedDirService` does not set `Accept-Ranges: none`** — `plugins/umbral-static/src/lib.rs:466–470`

`ServeDir` sets `Accept-Ranges: bytes` (line 234 of tower-http's `future.rs`). `EmbeddedDirService` sets no `Accept-Ranges` header. While a missing header means clients won't attempt range requests, explicitly setting `Accept-Ranges: none` is the spec-correct way to signal that range requests are not supported. Low priority but worth aligning for HTTP/1.1 compliance.

**[NEW] [Nit] `cache_headers.rs` describes a dev-mode test scenario that doesn't exist** — `plugins/umbral-static/tests/cache_headers.rs:6–11`

The file-level doc comment says "Tests that exercise it [dev-mode] set the settings OnceLock up-front." No such test exists (see the Important finding above). The comment is misleading — a future reader will search for the dev-mode test, not find it, and assume it was deleted rather than never written. **Fix shape:** either write the test (fixing the Important finding above) or remove the misleading comment.

already #66 — `helpers.mdx` omits `static()`/`media_url()`/`highlight_styles()` (docs gap, not a code bug).

## Tests

**What's covered:**

- `integration.rs` — filesystem round-trip (serve file, 404 on miss, nested subdirectory, plugin name, router builds for missing dir). All behavioral. Good.
- `cache_headers.rs` — no max_age → no header; max_age configured → header present with correct values; zero max_age → `max-age=0`. All filesystem mode. Good.
- `embedded.rs` — CSS/JS MIME correct; nested file served; 404 on miss; path traversal attempt → 404; `dir()` returns `None` for embedded. Good fixture-based tests.
- `collectstatic_command.rs` — command name is `collectstatic`; `--clear` parses correctly. Thin but adequate for command registration.
- `pipeline_integration.rs` — the only full `App::build` test; catches the boot panic regression for double-nest-catch-all; asserts site file, namespaced plugin asset, and 404 for unknown path. Excellent scope.
- `umbral-core/tests/static_publish.rs` + `static_files.rs` inline tests — registry collision/dedup, `resolve_under_root` (path traversal + symlink escape on Unix), `collect_static` idempotency/clear/missing-source/root-dirs, `static_handler` dev/prod algorithm. Comprehensive.

**Gaps:**

1. **Dev-mode cache override** — `effective_max_age()` dev branch is untested (see [NEW] Important finding above).
2. **Embedded mode cache headers** — `cache_headers.rs` only tests the filesystem path. Embedded mode `max_age` builder + `SetResponseHeaderLayer` apply the same code path but there's no embedded-mode cache-header test.
3. **Embedded mode 405** — no test that POST/PUT/DELETE to an embedded mount returns 405 with `Allow: GET, HEAD`.
4. **Embedded mode ETag / 304 absent** — no test asserting the current (wrong) behavior, which means the gap is undetected by the suite. A test that sends `If-None-Match: *` and asserts 200 (not 304) would document the current behavior; a fix test would assert 304.
5. **Dot-file exposure** — no test that requesting `/.env` or `/.git/config` from a filesystem mount returns a 404 (it currently returns a 200 if the file exists).
6. **`collectstatic` symlink-to-dir loop** — no test for the infinite-recursion scenario. Would need a Unix-only `#[cfg(unix)]` test with `std::os::unix::fs::symlink(dir, child_of_dir)`.
7. **Filesystem `ServeDir` 304 behavior** — no test that a second request with `If-None-Match` matching the ETag returns 304. (`ServeDir` does this correctly, but it's undocumented by tests here.) Low priority since `tower_http` tests it internally.
