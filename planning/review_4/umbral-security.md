# review_4 · umbral-security

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 2108 LOC (`plugins/umbral-security`) |
| **Reviewer** | review_4 slice agent (static-only) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 1 medium · 1 low · 0 info — 2 new, 0 known |

## Summary

- `SecurityPlugin` ships the framework's CSRF protection (HMAC-SHA256 signed double-submit cookie + session-cookie binding) and a per-response hardening-header bundle, layered onto the app router via `Plugin::wrap_router` (`src/lib.rs:384-460`).
- The CSRF machinery itself is well built: constant-time compares, signed-mode rotation for pre-upgrade cookies, secret resolution deferred to request time, Prod warnings for missing HSTS/CSP, and a `production_hardened()` preset. No raw SQL, no `unsafe`, no attacker-reachable panics found.
- Biggest gap: `csrf_exempt` prefixes are never validated, and `is_exempt` normalises a `"/"`/`""` prefix into "exempt every path" — a one-entry config mistake silently disables CSRF app-wide (SEC-1).
- Second gap: the plugin's layers are applied in core Phase 5.5, before the 404 fallback is installed (Phase 5.6), so 404 responses carry none of the hardening headers despite the module doc promising "every response" (SEC-2). The real fix is ordering in core (`app.rs`).
- The routed lead on the `/graphql` exemption prefix (`/graphql` also exempts `/graphql/sse` + `/graphql/ws`) is the documented segment-prefix semantic; verified held, see Clean areas — the graphql review owns the exploit angle.

## Findings

### SEC-1 · MEDIUM · security/ergonomics (unvalidated exempt prefixes) · self-verified
**An exempt prefix of `"/"`, `""`, or `"//"` collapses to "exempt every path" — CSRF silently disabled app-wide — and a prefix without a leading slash is silently inert; nothing validates or warns.**

- **Where**: `plugins/umbral-security/src/lib.rs:558-564` (`is_exempt`), `355-377` (`csrf_exempt` builder), `160`/`237` (`SecurityConfig::csrf_exempt_paths`), `462-494` (`on_ready` — no validation).
- **Evidence**:
  ```rust
  fn is_exempt(&self, path: &str) -> bool {
      self.exempt_paths.iter().any(|prefix| {
          let prefix = prefix.trim_end_matches('/');
          path == prefix || path.starts_with(&format!("{prefix}/"))
      })
  }
  ```
  With `prefix = "/"` (or `""`, or `"//"`), `trim_end_matches('/')` yields `""`, so the predicate becomes `path == "" || path.starts_with("/")` — true for every path. The middleware then short-circuits before the safe-method branch:
  ```rust
  if state.is_exempt(req.uri().path()) {
      return Ok(next.run(req).await);
  }
  ```
  (`src/lib.rs:748-750`). The builder appends with no validation:
  ```rust
  self.config.csrf_exempt_paths.extend(paths.into_iter().map(Into::into));
  ```
  (372-374), and `on_ready` (462-494) checks the secret and warns about HSTS/CSP only. The unit tests cover well-formed prefixes only (`1076-1127`: `exempt_path_matching_is_prefix_based`, `csrf_exempt_boundary_stops_at_path_segment`).
- **Impact** (traced): an operator writes `csrf_exempt(["/"])` — or splices an env-provided value that comes back empty — and every request on every route skips CSRF. Trace: `POST /contact` with a session cookie but no csrf token → `is_exempt("/contact")` → prefix trim → `""` → `starts_with("/")` → `true` → `next.run(req)` → the handler executes the state-changing write. Cookie/session-authenticated writes (login flows, admin actions, any form POST) become CSRF-forgeable, and no request fails, logs, or warns. The inverse mistake is quieter but real: a prefix without a leading slash (`"api"`) matches nothing (`/api` != `api`, and `/api/` does not start with `api/`), so the exemption silently does nothing and API writes 4xx until the developer removes it — a nudge toward the dangerous direction. MEDIUM: a security-control configuration trap, but not attacker-triggerable without the misconfiguration.
- **Fix**: validate/normalise at config time — in `csrf_exempt` and, for direct `with_config` users, in `on_ready`: reject (or warn loudly on) an empty/root prefix, and prepend a missing leading `/` (or warn) so a legitimate prefix can't be inert. A boot check is where the operator sees it; a debug assertion is not enough.

### SEC-2 · LOW · drift/ordering (contract) · self-verified
**404 responses miss every security layer: `wrap_router` layers are applied (core Phase 5.5) before the 404 fallback is installed (Phase 5.6), and an axum layer only wraps the endpoints that exist when `layer()` is called.**

- **Where**: `plugins/umbral-security/src/lib.rs:384-459` (all layers) + module doc `4-6`; `crates/umbral-core/src/app.rs:1714-1716` vs `1744-1748`; axum 0.8.9 vendored `src/routing/mod.rs:303-317` (`layer`) and `345-355` (`fallback`).
- **Evidence**: the module doc claims:
  ```
  //! request must carry a matching CSRF token; every response gets the hardening
  //! headers you've enabled.
  ```
  but core applies plugin layers first and installs the fallback afterwards:
  ```rust
  // Phase 5.5 — apply each plugin's middleware in topological order…
  for plugin in &sorted_plugins {
      router = plugin.wrap_router(router);
  }
  ```
  ```rust
  if need_not_found_fallback || slash_redirect != Off {
      let fallback = crate::errors::not_found_fallback(self.not_found_template.clone());
      router = router.fallback(fallback);
  }
  ```
  Axum 0.8.9: `Router::layer` maps the layer over `path_router`, `fallback_router` and `catch_all_fallback` **as they exist at call time** (`routing/mod.rs:303-317`); `Router::fallback` *adds a new endpoint* afterwards (`345-355`), so it is never wrapped. Core is deliberately ordered this way for the Phase 5.65 middleware stack ("Placed AFTER the 404 fallback so middleware sees misses too", `app.rs:1750-1756`) — the plugin layers simply predate that reasoning.
- **Impact** (static trace; axum semantics source-verified, not runtime-executed): a request that matches no route is served by the fallback with none of the plugin's layers — no `X-Content-Type-Options`, `X-Frame-Options`, `Referrer-Policy`, `X-XSS-Protection`, `Permissions-Policy`, COOP/CORP/COEP, no HSTS/CSP when enabled, and the `Server` header un-stripped. LOW: the miss path carries a fixed error body and no state change, so this is a hardening/claim gap, not a bypass — but "every response" is false for a whole response class, and the fallback is the one response an unauthenticated prober sees most.
- **Fix**: in `app.rs`, install the fallback before the Phase 5.5 loop, or re-apply the header/`Server`-strip layers after Phase 5.6. If core keeps the ordering, the module doc must stop promising "every response". (Routing note to the core reviewer in Out-of-scope.)

## Strengths

- The CSRF design is the good kind of boring: HMAC-SHA256-signed double-submit with constant-time compare (`subtle`), optional session-cookie binding, token scoped into `umbral::templates::CURRENT_CSRF` *before* the handler runs so a first render already carries it, and rotation of pre-upgrade cookie tokens when signed mode is enabled (`src/lib.rs:766-790`).
- Secret handling is layered defensively: `resolve_secret()` re-resolves ambient settings at request time (`548-556`) instead of freezing a possibly-absent secret at build time, and `on_ready` hard-fails an empty `SECRET_KEY` (`check_secret_key`, `491`).
- Prod posture nudges are present and loud: HSTS/CSP warnings in `Environment::Prod` (`475-489`) plus explicit `SecurityConfig::production_hardened()` / `SecurityPlugin::production_hardened()` presets (`288`, `334-335`).
- Exempt-path matching is segment-boundary correct with negative tests (`/api-internal`, `/apixyz`, `/api2`) — the audit_2 S4 fix held; only the unvalidated prefix *shapes* (SEC-1) slipped past.
- Behavioural test suite for the crate's size: `tests/csrf_flow.rs`, `private_cache.rs`, `production_hardened.rs`, `empty_secret_key.rs`, `integration.rs`.

## Clean areas checked

- **Percent-encoding / trailing-slash exemption bypass**: both the exemption check (`req.uri().path()`) and axum route matching operate on the raw, undecoded path — an encoded spelling like `/%61pi/...` matches neither the guarded route nor the exempt prefix, so no encoding trick reaches a CSRF-guarded handler while matching an exemption. Trailing slashes on prefixes are normalised by `trim_end_matches('/')` (the same normalisation whose degenerate case is SEC-1 for `"/"`).
- **`/graphql` prefix breadth** (routed lead): `/graphql` exempting `/graphql/sse` and `/graphql/ws` is the documented prefix semantic (builder doc `364-366` warns an exemption is only safe where nothing under it relies on cookie/session auth), and the exempt check runs before the method branch, so it applies to writes under the prefix — by design. Verified held as a security-plugin matter; the exploitable-route angle belongs to the graphql review (Out-of-scope).
- **No raw SQL, no `unsafe`**: `grep sqlx::query plugins/umbral-security/src/` is empty; the single `unsafe` textual hit is a doc-comment mention of CSP `'unsafe-inline'` (`src/lib.rs:286`) — zero real unsafe blocks.
- **Panics**: the only `expect`s are `getrandom::getrandom(...).expect("getrandom failed")` (603) and `Hmac::<Sha256>::new_from_slice(...).expect(...)` (623, infallible — HMAC accepts any key length); neither is reachable from request data. No `unwrap` on request paths.
- **Header application mechanics**: layers are additive; `SetResponseHeaderLayer::if_not_present` lets a handler opt out per response while defaults hold (`499+`); `SensitiveHeadersLayer` covers `AUTHORIZATION`/`COOKIE`/`SET_COOKIE`; `Server` override/strip both honored (`438-445`).

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps2 #81` — CSRF path-segment boundary `[fixed/verified held]` — SEC-1 is a distinct hole in the same area: the shipped boundary handling only covers well-formed prefixes; prefix *shape* is never validated.
- `audit_2 S1` — prod CSP/HSTS opt-in `[fixed/verified held]` — prod warning + `production_hardened()` preset ship (`lib.rs:288`, `334`).
- `audit_2 S2` — SecurityPlugin not mounted by default `[fixed/verified held]` — the generated app mounts it (`crates/umbral-cli/src/scaffold.rs:662`, `743`).
- `audit_2 S3` — signed-mode secret captured too early `[fixed/verified held]` — `resolve_secret()` re-resolves ambient settings per request (`lib.rs:548-556`).
- `audit_2 S4` — exemption prefix = documented tradeoff `[fixed/verified held]` — segment boundary kept; cookie-auth warning documented at `364-366`.
- `audit_2 S5` — `Server: umbral` default `[fixed/verified held]` — accepted tradeoff; `.server_header` opt-out exists.

## Out-of-scope notes

- `crates/umbral-core/src/app.rs:1714-1748` — fallback installed after the Phase 5.5 plugin-layer loop; SEC-2's real fix is ordering here (for the `umbral-core` reviewer).
- `plugins/umbral-graphql` — `/graphql` CSRF exemption breadth + graphql routes' own write-CSRF posture (cross-ref `review_4/umbral-graphql.md`); SEC-1 here is the config-validation side only.
- `crates/umbral-cli/src/scaffold.rs` — the generated app's `.csrf_exempt(["/api"])` is the well-formed case; no action, noted so audit_2 S2's verification site is on record.
