# review_4 · umbral-core (web layer slice)

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 12,566 LOC — the 15 slice files under `crates/umbral-core/src/` (`app.rs`, `web.rs`, `web/multipart.rs`, `web/streaming.rs`, `middleware.rs`, `errors.rs`, `api_error.rs`, `templates.rs`, `forms.rs`, `static_files.rs`, `cors.rs`, `hosts.rs`, `routes.rs`, `slash.rs`, `pagination.rs`) + ~780 LOC of crate tests skimmed |
| **Reviewer** | core-4-web slice (assembler merges this into `umbral-core.md` and renumbers to `CORE-n`) |
| **Verdict** | healthy |
| **Findings** | 0 critical · 0 high · 1 medium · 8 low · 0 info — 9 new, 7 known |

Slice file: IDs are slice-local (`WEBL-n`); the assembler renumbers when merging.

## Summary

- This slice is umbral's request path: `App::builder().build()` assembly (plugin registration, Phase 1–6 boot order, middleware/routing/DB wiring), the `Middleware` onion, error-page rendering, and the template / form / static-file / CORS / host / slash-redirect surfaces.
- Overall state: healthy. Every security-critical fix from review_3 / audit_2 that this slice owns is present and correct in this tree — CORS segment-boundary scoping, the prod host guard (incl. the HTTP/2 authority fallback), prod error-body blanking, JSON-500 pass-through, template autoescaping + the `safe_url` scheme allowlist, slash-redirect `//` refusal, multipart caps, and the slash probe's snapshot taken *after* the middleware stack.
- Biggest issue (WEBL-1, MEDIUM): two models that claim the same `TABLE` name are accepted silently at boot — `table_alias` and `meta_by_table` are last-wins HashMaps and the `model.table.unique` check the docs promise was never implemented, so ORM reads can be routed to the wrong model/database with no warning.
- Next two traps (WEBL-2, WEBL-3): an error body over 64 KiB is silently blanked before reaching the 500 hook / dev page / log, and `StreamingResponse::into_response` `expect()`s on a caller-supplied `content_type` — the one panic reachable from this slice's public API.
- The slash-redirect fix has no integration test through `App::builder()` (WEBL-9): only hand-built routers exercise the layer, so the snapshot-after-middleware property (the actual review_3-class fix) can regress with the suite green.

## Findings

### WEBL-1 · MEDIUM · correctness/arch · self-verified

**Two models that claim the same `TABLE` name are accepted silently at boot: alias/meta lookups are last-wins `HashMap`s and the promised `model.table.unique` system check does not exist.**

- **Where**: `crates/umbral-core/src/app.rs:1276-1281` and `:1284-1289` (the two `insert` sites), `crates/umbral-core/src/app.rs:1354-1355` (`meta_by_table`); check catalogue `crates/umbral-core/src/check.rs:8-15, 190-197`; model registry `crates/umbral-core/src/migrate.rs:64-72`
- **Evidence**:
  ```rust
  // app.rs:1276-1281 (per-plugin loop; the app-model loop at 1284-1289 is the same shape)
              let alias = model
                  .database
                  .clone()
                  .or_else(|| plugin_default.map(|s| s.to_string()))
                  .unwrap_or_else(|| "default".to_string());
              table_alias.insert(model.table.clone(), alias);
  ```
  ```rust
  // app.rs:1354-1355
  let meta_by_table: HashMap<&str, &ModelMeta> =
      all_models.iter().map(|m| (m.table.as_str(), m)).collect();
  ```
  ```rust
  // check.rs:11-13 — the promised check, still "landing later" (M5/M7 came and went):
  //! isn't left at the insecure dev default). More checks (`field.
  //! backend`, `model.pk.present`, `model.table.unique`, `route.
  //! collision`, `plugin.dependency.*`) land alongside the registries
  ```
  ```rust
  // check.rs:194-197 — first entries of the real catalogue; it contains
  // model.soft_delete_cascade / model.auto_user / model.materialized_view /
  // settings.* / backend.* / field.* / plugin.security_missing and NO
  // `model.table.unique`, `model.pk.present`, or `route.collision`.
  pub fn framework_checks() -> Vec<SystemCheck> {
      vec![
  ```
  ```rust
  // migrate.rs:65-70 — the registry flatten: no uniqueness rejection
  let mut flat: Vec<(String, ModelMeta)> = Vec::new();
  ...
          for m in per_plugin.get(&plugin).cloned().unwrap_or_default() {
              flat.push((plugin.clone(), m));
          }
  ```
  (Catalogue entry specified in `docs/specs/05-backends-and-system-check.md:164`: "`model.table.unique` | Two models don't claim the same `TABLE` name.")
- **Impact**: A plugin (or the app) declares a model whose `TABLE` collides with another model's — e.g. a third-party plugin shipping a `session` table alongside `umbral-sessions`. Nothing at boot reports it: `table_alias` keeps the last model seen, so on a multi-DB app the *first* model's queries can be routed to the *second* model's database alias (wrong-database reads via a silently-wrong `Alias`); `meta_by_table` binds FK resolution and the dynamic/admin path to one of the two models arbitrarily; and `init_plugins` hands the migration engine both models for one physical table, deferring the symptom to a confusing migrate-time DDL error. The failure is silent exactly where it is detectable (boot) and loud where it is not (runtime query / migration) — the posture CLAUDE.md's "backend mismatches caught at boot, not in prod" rule exists to prevent.
- **Fix**: Implement `model.table.unique` in `check.rs::framework_checks()` walking `umbral::migrate::models()` (the registry now exists), and/or make the two `app.rs` insert sites reject a duplicate instead of overwriting. Once the check guarantees uniqueness the maps can legitimately `expect` it. The spec catalogue (`docs/specs/05-backends-and-system-check.md:164`) should move the entry from "planned" to shipped in the same change. The check-id half belongs to the runtime/contracts slice — routed below.

### WEBL-2 · LOW · correctness

**An error body larger than 64 KiB is silently swapped for an empty string before the error message reaches the log, the `server_error` hook, and the dev-mode page.**

- **Where**: `crates/umbral-core/src/errors.rs:544-550` (`render_500_middleware`), `crates/umbral-core/src/errors.rs:630-634` (`render_error_middleware`)
- **Evidence**:
  ```rust
  // Capture the body to extract the error message for the hook + dev
  // context. 64KB cap: error messages don't need more, and we don't
  // want a malicious upstream to OOM us.
  let (_parts, body) = resp.into_parts();
  let bytes = axum::body::to_bytes(body, 64 * 1024)
      .await
      .unwrap_or_default();
  ```
  (the 4xx sibling at `:630-634` is identical: "Capture the body (the handler's message). 64KB cap, same as the 500 path.")
- **Impact**: `to_bytes` returns `Err(LengthLimitError)` when the body exceeds the cap, and `unwrap_or_default()` turns that into `[]`, so `humanize_error_body("")` produces an empty message. A handler whose 500 / `(status, message)` body exceeds 64 KiB — a rendered error page, a validation-errors JSON with many fields, a long error chain — therefore logs a blank line, fires `fire_server_error_hook` with an empty message, and (dev mode) renders the error page with no message. The served body is still correct (it is re-rendered from the template, not the captured bytes), so the loss is invisible until someone needs the diagnostic. The comment says the cap exists because "error messages don't need more" — the intended degradation is truncation; today it is amnesia.
- **Fix**: On capture failure keep the leading bytes (read through a limit that truncates rather than errors, or re-read with the cap removed after warning), or at minimum `tracing::warn!` when the capture fails so the blank message has an explanation.

### WEBL-3 · LOW · api

**`StreamingResponse` panics in `into_response` for a caller-supplied `content_type` that is not a valid header value — the public setter accepts any `String`.**

- **Where**: `crates/umbral-core/src/web/streaming.rs:78-83` (setter), `:121-122` (the `expect`)
- **Evidence**:
  ```rust
  /// Set the `Content-Type`. Defaults to `application/octet-stream`.
  pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
      self.content_type = content_type.into();
      self
  }
  ```
  ```rust
  builder
      .body(self.body)
      .expect("content-type / content-disposition are always valid header values")
  ```
- **Impact**: `http::response::Builder::header` defers `HeaderValue` validation to `.body()`, so a `content_type` containing a control character — e.g. `.content_type(format!("{stored_ct}; charset=utf-8"))` where `stored_ct` comes from a DB column or upload metadata, the natural file-serving pattern — makes `into_response` panic inside the request path: a 500 via `CatchPanicLayer`, or a dropped connection when that layer is disabled. The `expect` message asserts an invariant the API does not enforce ("always valid" is true only for values the framework itself writes; the setter is public and accepts arbitrary strings). Latent today — no in-tree caller passes a dynamic value (`grep '\.content_type('` finds only `crates/umbral-core/tests/streaming_response.rs:20`) — but it is the one panic this slice exposes to user code. `Content-Disposition` is already sanitized (`sanitize_filename`, `:128-132`); `Content-Type` is the unsanitized sibling — the drift *is* the finding.
- **Fix**: Validate at the setter boundary — fall back to `application/octet-stream` for an invalid value (or accept `HeaderValue`), and in `into_response` return a 500 `Response` instead of `expect`, since `IntoResponse` can return a fresh response and there is no reason to panic here.

### WEBL-4 · LOW · docs

**Five `ignore`d doc examples call `.build().await?`, but `AppBuilder::build()` is synchronous.**

- **Where**: `crates/umbral-core/src/app.rs:916-917`, `:943-944`, `:1019`, `:1041`; `crates/umbral-core/src/cors.rs:33-34`; signature at `crates/umbral-core/src/app.rs:1122`
- **Evidence**:
  ```rust
  ///     .build()
  ///     .await?
  ```
  (`app.rs:916-917`; `app.rs:1019` and `:1041` are `///     .build().await?;`; `cors.rs:33-34` is the `//!` twin)
  ```rust
  pub fn build(self) -> Result<App, BuildError> {
  ```
- **Impact**: Copy-pasting any of these examples does not compile — `Result` is not awaitable. They are `ignore` blocks, which is why rustdoc/CI never caught the drift, so the reader has to debug the framework's own example. Five occurrences across two files make this a missed find-replace after `build()` became sync, not five independent typos, and they are not covered by the stale-comment batch in `gaps6 #21`.
- **Fix**: Drop `.await` in all five sites; consider un-`ignore`ing one so the compiler pins the signature permanently.

### WEBL-5 · LOW · docs

**`AppBuilder::cors()`'s doc claims it "becomes the outermost wrapper" — contradicted by the layering the same function installs.**

- **Where**: `crates/umbral-core/src/app.rs:920-921` (claim) vs `:1887-1888` (global CORS applied here) and `:1898-1901` (host guard applied later, i.e. outside)
- **Evidence**:
  ```rust
  /// The layer is applied LAST in the middleware chain so it
  /// becomes the outermost wrapper — preflight `OPTIONS` is
  ```
  ```rust
  if let Some(cors) = self.cors.take() {
      router = router.layer(cors.into_layer());
  }
  ...
  // Phase 5.95 — Host-header validation (allowed-hosts allowlist). Applied
  // outermost so a forged `Host` is rejected with a 400 before any
  // handler, plugin, or CORS logic runs.
  ```
- **Impact**: Every `router.layer()` wraps what exists, so the layers applied *after* CORS — host guard, body-limit, timeout, default security headers, `TraceLayer` — all sit outside it; the Phase 5.95 comment in the same file even states the host rejection happens "before any ... CORS logic runs". Concretely: a host-rejected 400 (and a 413/408/trace-wrapped response) carries no CORS headers, so a browser reports an opaque CORS failure instead of showing the real status. The behaviour is defensible; the doc sentence invites misdiagnosis, and the two comments in one file contradict each other.
- **Fix**: Reword the `cors()` doc: the layer wraps the app's routes and middleware but sits *inside* the host / hardening / trace layers — describe the order it actually gets.

### WEBL-6 · LOW · api

**A middleware that short-circuits from `before_request` never gets its own `after_response` call, and neither the docs nor the pinning test say so.**

- **Where**: `crates/umbral-core/src/middleware.rs:163-170` (`ran` incremented only on `Ok`), `:186-189` (unwind); docs `:20-23` and `:74-75`; test `crates/umbral-core/tests/middleware_pipeline.rs:57-60, 149-161`
- **Evidence**:
  ```rust
  match mw.before_request(req).await {
      Ok(modified) => {
          req_opt = Some(modified);
          ran += 1;
      }
      Err(resp) => {
          short_circuit = Some(resp);
          break;
      }
  }
  ```
  ```rust
  // Only the middleware whose `before_request` ran get an
  // `after_response`, in reverse (onion unwind).
  for mw in stack.iter().take(ran).rev() {
      res = mw.after_response(res).await;
  }
  ```
- **Impact**: The doc sentence "only the `after_response` hooks of the middleware that already ran … get to see the short-circuit response" (module doc `:18-23`, trait doc `:72-75`) is ambiguous about the middleware whose `before_request` *produced* the short circuit: it ran, but `ran` counts only `Ok`, so it is excluded. A middleware that splits the work — reject in `before_request`, decorate the rejection in `after_response` (e.g. adding `Retry-After` to its own 429) — silently loses the decoration, with the only workaround being to set headers on the `Err` response directly. The test that pins the behaviour cannot catch a change: its `Gate` (line 57) implements `before_request` only, so "the short-circuiter's `after_response` must not run" and "it is never called" are indistinguishable; the assertion message at `:161` ("only already-run middleware unwind on short-circuit") encodes the ambiguity rather than resolving it.
- **Fix**: Decide and document explicitly: either run `after_response` on the short-circuiting middleware too (arguably the correct onion — it did run), or state in both doc sites that the short-circuiter is excluded; then give the test's `Gate` a recording `after_response` and assert whichever behaviour is chosen.

### WEBL-7 · LOW · correctness

**A `Host` header in fully-qualified form (`example.com.`, trailing dot) fails the prod host guard.**

- **Where**: `crates/umbral-core/src/hosts.rs:66-72` (`hostname_of`), `:53-62` (`match_host`), `:39-44` (`HostPolicy::new` lowercases only)
- **Evidence**:
  ```rust
  fn hostname_of(raw: &str) -> &str {
      let raw = raw.trim();
      if let Some(rest) = raw.strip_prefix('[') {
          return rest.split(']').next().unwrap_or(rest);
      }
      raw.split(':').next().unwrap_or(raw)
  }
  ```
  ```rust
  if let Some(domain) = pattern.strip_prefix('.') {
      // ".example.com" matches "example.com" and any subdomain of it.
      return host == domain || host.ends_with(pattern);
  }
  host == pattern
  ```
- **Impact**: `Host: example.com.` is normalized only by `trim()` + port-strip, so the trailing dot survives into `match_host`, where plain patterns compare with `==` — and a leading-dot pattern `.example.com` misses it too (`"example.com.".ends_with(".example.com")` is false). No match → `HostPolicy` (enforced only in `Environment::Prod`) answers 400. The trailing-dot FQDN is legal (RFC 3986 §3.2.2 / RFC 1035) and is emitted by some HTTP clients, health checkers, and string-built proxies; the guard is off in dev, so the app works locally and starts 400ing those callers in production only.
- **Fix**: Strip one trailing `.` in `hostname_of` (and normalize allowlist entries the same way in `HostPolicy::new`); the existing equality/suffix logic is then correct.

### WEBL-8 · LOW · correctness/api

**`Field::select` / `Field::fk_select` with `nullable = false` mark the field required but install zero validators, so an empty submission validates clean.**

- **Where**: `crates/umbral-core/src/forms.rs:576-588` (`select`) and `:596-610` (`fk_select`), `:670-679` (`validate`)
- **Evidence**:
  ```rust
  pub fn select(name: impl Into<String>, options: Vec<(String, String)>, nullable: bool) -> Self {
  ...
          required: !nullable,
          validators: Vec::new(),
  ```
  ```rust
  pub fn validate(&self, value: &str, errors: &mut ValidationErrors) {
      if !self.required && value.is_empty() {
          return;
      }
      for v in &self.validators {
  ```
- **Impact**: A required select has no way to fail server-side: with `required: true`, `validate("")` skips the early return, then iterates an empty validator list and reports nothing — so `POST field=` is accepted. The docstring at `:573-575` ("`nullable` … drops `Required`") implies a `Required` validator exists to drop; it does not, so `required` means only the HTML attribute (client-side only). The derive path is safe — `umbral-macros` attaches `validate_choice_member` as a validator (`crates/umbral-macros/src/lib.rs:5847, 5870-5878`) — so the trap is limited to hand-built forms via the public `forms::Field` API (the only in-tree `Field::select` caller is the macro).
- **Fix**: In both constructors, when `!nullable` push a `Required` validator (or special-case `self.required && value.is_empty()` in `validate`); reserve `validators: Vec::new()` for the nullable case.

### WEBL-9 · LOW · tests

**No test drives the slash-redirect layer through `App::builder()` — the snapshot-after-middleware-stack property (the review_3-class fix) has no regression cover.**

- **Where**: wiring `crates/umbral-core/src/app.rs:1794-1805` and the builder `:710-711`; layer tests `crates/umbral-core/tests/slash_redirect.rs:22-27`
- **Evidence**:
  ```rust
  // Slash-redirect layer (see the Phase 5.6 note above). Installed
  // after the middleware stack so the snapshot it probes runs the
  // alternate-form request through the global middleware rather than
  // around it, and before the panic / compression / CORS / host layers
  // so a probe never needs those outermost wrappers to decide a
  // redirect. ...
  if self.slash_redirect != crate::slash::SlashRedirect::Off {
      let snapshot = router.clone();
  ```
  ```rust
  fn router_with_fallback(router: Router, policy: SlashRedirect) -> Router {
      if policy == SlashRedirect::Off {
          return router;
      }
      router.layer(axum::middleware::from_fn(slash_redirect_probe(
  ```
- **Impact**: `grep -rn "\.slash_redirect(" crates/umbral-core/tests plugins/*/tests` returns nothing, and `SlashRedirect::default()` is `Off` (`slash.rs:85-88`) — so no test sets a non-`Off` policy through `App::builder()`. `tests/slash_redirect.rs` exercises `alternate_path` and the probe on hand-built routers, and the auth plugin's `both_slash_forms_of_login_resolve` passes with the layer off; the property that motivated the current wiring — the probe must run *through* the global middleware stack so it cannot bypass rate-limiting/auth — is asserted nowhere. Reverting the fix to a pre-stack snapshot would leave every test green.
- **Fix**: One integration test in the style of `tests/middleware_pipeline.rs`: boot with `App::builder().slash_redirect(SlashRedirect::Append)` plus a counting (or denying) `Middleware`, request the non-canonical form, and assert both the 308 and that the middleware observed the probe (counter incremented, or middleware-denied → no redirect).

## Strengths

- **The security-critical history holds, verified in-tree this sweep.** CORS scoping is segment-boundary-correct (`cors.rs:369-374` `path_under_prefix` requires the next char to be `/`, with the rationale comment at `:366-368` and a dedicated test at `:383`); the host guard rejects before every downstream layer and supports the HTTP/2 authority form (`hosts.rs:53-62, 100-107`); 5xx bodies are blanked outside dev (`errors.rs:645-647`, "WEB-5 safe-by-default"); JSON 500s pass through un-rendered (`errors.rs:536-540`); slash redirects refuse protocol-relative paths (`slash.rs:279` test); multipart carries a 32 MiB default cap (`web/multipart.rs:34-40`).
- **The onion is small and legible.** `middleware.rs` is one `run_stack` function with a `ran` counter and a comment per branch; `Middleware` is dyn-compatible with defaulted pass-through hooks, and the typed `MiddlewareRegistry` (feature #68) coexists with opaque `wrap_router` layers — the right partition of what can and cannot be introspected.
- **Boot phases are documented as decisions.** The Phase 5.6/5.65/5.9/5.95/5.99 comments in `app.rs` (e.g. `:1794-1800`, `:1898-1901`, `:1913-1920`) each state *why* the layer sits where it does and which earlier bug motivated it. This is what made reviewing the ordering tractable; it is also what the next maintainer will need before touching it.
- **Error rendering is centralized with the right guards.** `render_500_middleware` skips already-HTML and JSON responses before touching anything (`errors.rs:522-540`), and `render_error_middleware` keeps `Accept: application/json` bodies raw — the "one surface, two formats, don't clobber either" split is handled once, not per-handler.
- **`api_error.rs` never leaks internals by accident.** `Database`/`Internal` log the cause and serialize an opaque `"internal server error"`, while `Unauthorized`/`Forbidden`/`TooManyRequests` pass developer-authored text — safe by default, with the escape hatch explicit and four tests.
- **Static publishing is careful.** `collect_into_with` writes a hashed manifest as a `BTreeMap`, `copy_tree` refuses symlinks that escape the source root, collisions are errors, and the suite covers traversal, dev-vs-prod serving, and idempotent re-publish (`static_files.rs`, read in full).
- **The slash probe is defensively designed.** Header-less GET, pre-layer snapshot, `//` refusal, 308 to preserve method/body, and a CRLF canary — the sharp edges were thought about, which is why WEBL-9 (missing regression test) is worth closing rather than the code being wrong.

## Clean areas checked

- **Template escaping**: autoescape on for `.html`/`.htm`, `render_str` as the explicit raw path, `safe_url` allowlists schemes against `javascript:`-style hrefs (`templates.rs:443-453`), the `img` filter escapes URL/alt into a `from_safe_string` wrapper (`:345-379`), attribute-context escaping present — no unescaped-interpolation hole found; `safe_string` misuse sites grepped.
- **CSRF (core side)**: the task-local `CURRENT_CSRF` + escaped `csrf_input` helper are correct; the middleware itself lives in `plugins/umbral-security` (routed below).
- **Host-guard semantics**: prod-only enforcement, `"*"` disable, leading-dot subdomain patterns, IPv6 brackets and ports stripped, HTML-escaped 400 page — all as documented (the trailing-dot case is WEBL-7).
- **Slash-redirect protocol safety**: `alternate_path` refuses protocol-relative paths; backslash cannot reach a `Location` header because `http::Uri` rejects it (`http-1.4.1` `URI_CHARS[0x5C] == 0`, and hyper parses request targets via `Uri::from_maybe_shared`), so the "`\` acts as `/`" open-redirect hypothesis is refuted, not merely unfound; 308 preserves method/body.
- **Body/size limits**: `RequestBodyLimitLayer` + `TimeoutLayer` wrap every request including host-guard rejections (`app.rs:1913-1927`); multipart enforces per-part and total caps; `pagination.rs` clamps page size (no unbounded list default).
- **Static file serving**: traversal and escaping-symlink refusal, content-type gating, hashed-vs-dev split, collision errors — adversarial cases are in the suite.
- **`web.rs` / `api_error.rs` / `routes.rs`**: thin plumbing (axum re-exports, `API_BASE` first-write-wins `OnceLock`, error shapes, route registration); no logic to get wrong, no findings.
- **`app.rs::ready()`**: hooks run exactly once, `on_ready` failures are typed (`BuildError::PluginOnReady`) and propagated; the pre-hook `ready_fired` flag is the known boot-lifecycle item below, not a new finding.
- **Panics on the request path**: audited `unwrap`/`expect` in the slice — everything else reachable is either guarded (`expect("request present …")` on an invariant maintained two lines up) or funneled to `CatchPanic`; WEBL-3 is the one exception.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps6 #24` — `request_timeout` is global-only; one slow route forces the whole app's timeout up (open).
- `gaps6 #21` — stale doc comments (a)-(e), incl. `app.rs:522-524` (open; WEBL-4/5 are not among them).
- `gaps4 #50` — slash-redirect as a layer over every 404 (fixed; `app.rs:1794-1800` is that fix's own comment).
- `gaps4 #44` — builder-order documentation/behaviour (fixed).
- `gaps5 #102` / TaskFlow #323 — middleware ordering/registry contract (resolved by `MiddlewareRegistry`, feature #68; wired at `app.rs:1791`, tested by `tests/middleware_pipeline.rs` + `tests/middleware_registry.rs`).
- `planning/gaps5.md:410` (review_3 triage closeout) — fixed: CORS segment-boundary matching, JSON-500 passthrough, prod error-body blanking, `safe_url` scheme allowlist; deferred there and since landed in the tree: slash-redirect probe snapshot taken after the middleware stack (`app.rs:1794-1800`); still deferred: pure code-dedup incl. the `html_escape` ×3 duplication in `templates.rs`, plus assorted doc-drift.
- `planning/audit_2/findings/core-app-config.md:21` (entry 7, "deferred: boot-lifecycle redesign") — a failed `build()` has already published settings/pools/backend into panicking `OnceLock`s. Adjacent and folded here rather than numbered: `ready()` sets `ready_fired` before the `on_ready` loop (`app.rs:103` vs `:110-117`), so a failed hook leaves the process reporting ready — same boot-lifecycle redesign.

## Out-of-scope notes

1. `plugins/umbral-security` — the CSRF middleware implementation lives there (core side verified clean); route CSRF questions to that slice.
2. `crates/umbral-core/src/check.rs` (runtime/contracts slice) — `model.table.unique`, `model.pk.present`, `route.collision`, `plugin.dependency.*` are documented as "lands alongside the registries" (`check.rs:8-15, 190-192`) and are absent from `framework_checks()` (`:194-257`); WEBL-1 depends on this absence, and the fix for WEBL-1 is the check implementation.
3. `crates/umbral-core/src/app.rs:1513-1517` — comment "BUG-20: … multiple plugins contributing the same path produce duplicate entries; umbral-openapi's merge step picks the first" — for the openapi/plugin slices to cross-check.
4. `plugins/umbral-storage` — stored/sniffed `content_type` values exist there (`media.rs` `sniff_content_type`, `neutralised_upload`); if any consumer feeds a stored value into `StreamingResponse::content_type`, WEBL-3 moves from latent to live — worth one check in that slice.
