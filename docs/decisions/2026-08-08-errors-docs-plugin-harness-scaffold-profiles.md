# Errors taxonomy, docs-drift process, plugin compliance harness, and scaffold profiles (design)

Status: draft. Covers planning/gaps5.md #79 (tf#292), #80 (tf#293), #81 (tf#294), #82 (tf#295).
Date: 2026-08-08

Four DX/quality line items that share one theme: making umbral's *contracts* legible and enforceable to the people on the outside of the framework - API clients (#79), doc readers (#80), third-party plugin authors (#81), and operators standing up production (#82). Each section is self-contained; they are drafted together because they cross-reference (the harness tests the manifest, the scaffold profiles wire the EnterprisePreset, and all four are quality gates rather than new runtime features).

This is a design draft. Nothing here ships in this pass; the point is to fix the shapes so the implementation PRs are mechanical.

---

## #79 (tf#292): a unified error taxonomy

### The problem, concretely

There is a good handler-facing error type already - `ApiError` in `crates/umbral-core/src/api_error.rs`. It gets the two things that matter right:

- **Safe-by-default split.** `Database(sqlx::Error)` and `Internal(String)` log the real cause server-side (`tracing::error!`) and hand the client an opaque `"internal server error"`; table names, SQL fragments, and constraint text never reach the wire. The client-visible variants (`NotFound`, `BadRequest`, `Unauthorized`, `Forbidden`, `TooManyRequests`, `Validation`) carry only text a developer wrote or a structured field-error map.
- **`?`-flow.** `From<sqlx::Error>`, `From<WriteError>`, `From<DynError>`, and `From<TemplateError>` mean a handler returns `Result<T, ApiError>` and a bare `?` does the right thing (validation -> 400, infra -> 500).

What is NOT unified is everything *around* it:

1. **Parallel error enums per plugin.** `umbral-rest` ships its own `ActionError` (`plugins/umbral-rest/src/resource.rs`) with variants `BadInput / NotFound / Unauthenticated / Forbidden / Internal` - a second copy of the same taxonomy, with a different name set and its own `IntoResponse`. `umbral-auth` renders 401/403 responses from several files (`login_required.rs`, `extractors.rs`, `form_routes.rs`, `session_user.rs`). The admin and tasks surfaces render their own error pages/logs. Nothing forces these to agree on a code string, a JSON shape, or the user-safe/internal split.
2. **Two JSON envelopes, no stable machine codes.** `ApiError::into_response` emits `{"error": "...", "code": "..."}` for single-message errors and `{"code, field_errors, non_field_errors}` for validation. The `code` strings (`not_found`, `bad_request`, `database_error`, `internal_error`, ...) are ad-hoc, undocumented, and not shared with `ActionError`, which builds its own envelope. A client cannot switch on a stable code across surfaces.
3. **No RFC7807.** Nothing emits `application/problem+json`, the interop standard clients and gateways expect.

### The design

Three pieces, all building on the real `ApiError`. The goal is one taxonomy and one wire contract that REST, GraphQL, auth, admin, and tasks all route through - without breaking the ergonomic `Result<T, ApiError>` handler surface that already exists.

#### 1. A stable code taxonomy: `UmbralErrorCode`

A closed enum of stable, documented machine codes in `umbral-core`, re-exported from the facade (`umbral::web::UmbralErrorCode`, not in the prelude - this is contract surface, not everyday handler code). Each variant maps to exactly one HTTP status and one `kind` (user-safe vs internal). The strings are the public contract; they never change once shipped.

```rust
// crates/umbral-core/src/api_error.rs (or a sibling errors.rs)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UmbralErrorCode {
    // 4xx - user-safe: the message is developer/framework text, safe to send.
    NotFound,            // "not_found"            404
    BadRequest,          // "bad_request"          400
    ValidationFailed,    // "validation_failed"    400  (carries field errors)
    Unauthorized,        // "unauthorized"         401
    Forbidden,           // "forbidden"            403
    Conflict,            // "conflict"             409  (unique/constraint the client can resolve)
    TooManyRequests,     // "too_many_requests"    429
    UnsupportedMedia,    // "unsupported_media"    415
    // 5xx - internal: cause is logged, never sent.
    Internal,            // "internal_error"       500
    DatabaseError,       // "database_error"       500 (a distinct code, same opaque body)
    ServiceUnavailable,  // "service_unavailable"  503
}
```

`UmbralErrorCode` owns `fn as_str(&self) -> &'static str`, `fn status(&self) -> StatusCode`, and `fn is_user_safe(&self) -> bool` (true for the 4xx family). This is the single source of truth: `ApiError` variants map onto codes, and every other surface maps its own error onto the same enum rather than inventing strings. The existing ad-hoc strings (`not_found`, `bad_request`, `database_error`, `internal_error`) are preserved as the `as_str()` values, so the change is additive - current clients keep seeing the codes they see today; the enum just makes them a contract instead of a literal.

#### 2. An RFC7807 problem+json response contract

RFC7807 defines a `application/problem+json` body with members `type`, `title`, `status`, `detail`, `instance`, plus arbitrary extensions. umbral's contract:

```json
{
  "type": "https://umbral.dev/errors/validation_failed",
  "title": "Validation failed",
  "status": 400,
  "code": "validation_failed",
  "detail": "One or more fields are invalid.",
  "instance": "/api/post/",
  "field_errors": { "title": ["This field is required."] },
  "non_field_errors": []
}
```

Rules, and how they encode the user-safe/internal split:

- `type` is a stable URI built from the code (`https://umbral.dev/errors/<code>`); it does not have to resolve, but the docs site will host a page per code (ties to #80). `code` is the machine key (`UmbralErrorCode::as_str`), carried as an extension member so a client can switch on it without URI parsing. Keeping both `type` and a bare `code` means old `{"error","code"}` consumers and new problem+json consumers both find their field.
- `title` is a fixed, non-sensitive phrase per code (`"Not found"`, `"Internal server error"`). `detail` is the *user-safe* message: for 4xx it is the developer/framework text; **for 5xx it is a fixed generic string**, never `err.to_string()`. The real cause is logged server-side exactly as `ApiError` does today.
- `field_errors` / `non_field_errors` are extension members present only for `ValidationFailed`, sourced from `WriteError::field_errors()` / `non_field_errors()` (already wired in `ApiError::Validation`).
- `instance` is the request path (best-effort; omitted if unavailable). No query string, no PII beyond the path.

Content type is `application/problem+json` per the RFC. Because problem+json is a superset of the current `{"error","code"}` shape (it *adds* members), the transition is a widening, not a break; a legacy client reading `.error` can be kept working by also emitting `"error": <detail>` during a deprecation window if we choose (open question 1).

#### 3. One renderer, every surface routes through it

The unification is: **there is exactly one function that turns a `(UmbralErrorCode, user-safe detail, optional field errors)` into a `Response`.** `ApiError::into_response` becomes a thin call into it. Every other error type converts *into `ApiError`* (or into the code triple) rather than rendering its own envelope:

- **REST.** `ActionError` collapses into `ApiError` via a `From<ActionError>` impl (`BadInput -> BadRequest`, `Unauthenticated -> Unauthorized`, `Forbidden -> Forbidden`, `NotFound -> NotFound`, `Internal -> Internal`). `ActionError` stays as the *authoring* convenience (a handler can still return it) but its `IntoResponse` delegates to the shared renderer, so REST stops shipping a second envelope. The built-in list/detail/create handlers already produce 400s with field errors; those move to the shared renderer too.
- **GraphQL.** GraphQL has its own error array shape (`errors: [{ message, extensions }]`), so it does not emit problem+json at the transport level. Instead it puts the SAME taxonomy into `extensions.code` (= `UmbralErrorCode::as_str`) and applies the identical user-safe/internal split for `message` (opaque for internal). One taxonomy, two transports.
- **Auth.** The 401/403 renders in `umbral-auth` route through `ApiError::unauthorized` / `ApiError::forbidden` (HTML-redirect variants for browser flows keep their redirect, but the JSON/API path uses the shared renderer). This kills the scattered hand-built responses.
- **Admin.** Admin is HTML; it keeps rendering error *pages*, but the status + logged-vs-shown split is driven by `UmbralErrorCode` so a 500 page never prints the cause. Its JSON data endpoints use the shared renderer.
- **Tasks.** Task failures are not HTTP responses; the taxonomy still applies to how a failure is *recorded* (a stable `code` on the failed-task row + the internal cause in the log, never in a user-visible field), so an admin surfacing task failures shows the safe code, not the raw error.

#### Why build on `ApiError` rather than replace it

`ApiError` already encodes the hard-won safe-by-default posture and the `?`-conversions every handler depends on (its module doc calls the leaky `(StatusCode, String)` + `err.to_string()` pattern out by name). Replacing it would churn every handler. Adding `UmbralErrorCode` + the problem+json renderer *under* it, and making the plugins converge onto it, gets one taxonomy with zero handler churn. The test that this is right: deleting `ActionError`'s bespoke `IntoResponse` and pointing it at the shared renderer must change no status code and no field-error output - only the envelope, additively.

### Rollout

1. Land `UmbralErrorCode` + `problem_response(...)` renderer in `umbral-core`; make `ApiError::into_response` call it. Emit problem+json. (Optionally keep `"error"` alongside for one release.)
2. `From<ActionError> for ApiError` + delegate `ActionError`'s `IntoResponse`. Grep `plugins/umbral-rest` for hand-built error bodies; route them through.
3. Auth JSON paths -> `ApiError`. GraphQL `extensions.code`. Admin JSON endpoints + page status. Tasks failure-record code.
4. Docs page per code (feeds #80's docs-tests: the `type` URIs become link-checked).

### Open questions

1. Keep a legacy `"error": <detail>` member in problem+json for one deprecation window, or cut straight to RFC7807? (Recommend: keep it one release, behind a note, then drop.)
2. Is `Conflict` (409) worth splitting out of the current `WriteError` -> 400 mapping, or does a unique-violation stay a 400 validation error? (Recommend: 409 only when the ORM can positively identify a unique/constraint violation; otherwise 400.)

---

## #80 (tf#293): documentation drift sweep + docs tests

### The problem, concretely

gaps5 #28 (tf#241) already proved docs lag shipped behavior: `docs/specs/06-migration-engine.md:21-25` claimed index operations and `RunSql` were "deferred" long after they shipped. That specific drift has been fixed. #80 is the *process* that stops the next one - turning "someone noticed the spec was wrong" into a CI gate.

There is no mechanism today that fails when a doc example stops compiling, a spec's cross-link 404s, or a spec describes behavior the code no longer has. The #28 fix was manual and reactive; this makes drift a build failure.

### The design

Four mechanisms, smallest-first.

#### 1. Docs tests: doc examples must compile (and run where cheap)

The user-facing docs live in `documentation/docs/v0.0.1/**/*.mdx` and carry fenced ```rust code blocks. Internal specs carry ```rust too. Today those are dead text. The mechanism:

- **Extract-and-compile.** A test harness (a `tests/docs_examples.rs` in a small `umbral-docs-tests` dev crate, or an `xtask`) walks the MDX/spec tree, pulls every ```rust fence not tagged `ignore`/`no_run`/`text`, wraps each in a `fn main`-or-`#[test]` shell against the real facade, and compiles them. A fence that no longer compiles against the current API is the drift signal - the same failure `cargo test --doc` gives for `///` doctests, extended to prose docs the compiler never sees.
- **Fence tags mirror rustdoc.** ` ```rust ` = must compile; ` ```rust,ignore ` = shown but not compiled (for illustrative fragments); ` ```rust,no_run ` = compile but do not execute (anything needing a DB/server). This reuses conventions authors already know from rustdoc, so no new dialect.
- **Prefer real doctests where the code is library code.** For anything that lives on a public item, a `///` doctest is better than an MDX fence because `cargo test --doc` already runs it. The MDX harness is for the standalone tutorial/guide prose that has no home on an item.

This is the direct generalization of the #28 fix: #28 was a human reading a spec and noticing a lie; docs tests make the compiler read it.

#### 2. Link checker

A CI job that resolves every link in `documentation/` and `docs/`:

- **Internal links** (relative paths, `docs/...` refs, `#28`-style cross-refs to specs, the `https://umbral.dev/errors/<code>` URIs #79 introduces): must resolve to a real file/anchor. A dead cross-link is a hard failure.
- **External links**: checked in a *non-blocking* nightly job (external 404s/timeouts are noisy and not our bug); a broken external link opens an issue, it does not fail PR CI.

A small offline crawler (the tree is local Markdown/MDX) is enough; no live site needed for internal links.

#### 3. Stale-spec marker convention

Not every drift is catchable by a compiler. A spec can describe an intent that shifted. The convention: a spec may carry a machine-readable staleness banner, and CI enforces its terms.

```markdown
<!-- umbral:spec status=stable owner=@dalmas reviewed=2026-08-08 covers=crates/umbral-core/src/api_error.rs -->
```

- `status`: `stable` | `draft` | `stale` | `superseded=<path>`. A doc marked `stale` renders a visible banner ("This spec may lag the code; see …") on the docs site and is excluded from the "trustworthy" set.
- `owner`: the spec-owner (see #4). `reviewed`: last human review date.
- `covers`: the source paths the spec describes. CI can warn (not fail) when a `covers` path changed substantially since `reviewed` (git-diff heuristic) - a *review nudge*, not a gate, because "the code changed" is not proof "the spec is wrong."

The banner is an HTML comment so it is inert in rendered Markdown and MDX; a small linter parses it.

#### 4. Spec-owner reviews + the CI job

- **Ownership.** A `CODEOWNERS`-style map (`docs/OWNERS` or the `owner=` marker) assigns each spec/area a human owner. A PR that touches a `covers` source path pings the owning spec's owner as a suggested reviewer - the human half of drift-prevention.
- **The CI job** (`docs-check`) runs on every PR: (1) compile docs examples (#1), (2) internal link check (#2), (3) staleness-marker lint (#3: every spec under `docs/specs/` must carry a marker; a missing one fails). External-link and review-nudge checks run nightly/non-blocking. This is one job so "docs are green" is a single signal next to `cargo test`.

### Why this shape

The #28 fix showed the failure mode is *silent*: nothing broke, the spec just quietly lied for weeks. Every mechanism here converts a silent lie into a loud failure at the earliest cheap point - the compiler for examples, a resolver for links, a required marker for intent. It deliberately does NOT try to auto-detect semantic drift (that needs a human); it makes the human review *cheap and routed* (owner + marker) instead of heroic.

### Open questions

1. MDX-fence harness as a workspace dev-crate vs an `xtask`. (Recommend `xtask` so it does not add a workspace member that `cargo test` compiles on every run; run it in `docs-check` only.)
2. Do internal `#N`-style gap cross-refs get link-checked against `planning/`? (Recommend: warn-only; gap numbers are stable identifiers but the trackers move entries to archives, so a resolver would need to search both.)

---

## #81 (tf#294): plugin author test/certification harness

### The problem, concretely

The `Plugin` trait (`crates/umbral-core/src/plugin.rs`) is a strong runtime contract, and `docs/specs/plugin-manifest-and-registry.md` adds a *descriptive* manifest (what a plugin declares). What is missing is the *verification*: a third-party plugin author has no way to prove their plugin actually satisfies the contract - that its migrations apply, its declared routes exist, its OpenAPI validates, its system checks pass, its settings/admin/auth wire up, and it stays semver-compatible. Today they find out by booting a real app and hoping.

The manifest spec calls this out explicitly (§6): "Not the plugin compliance/certification harness (gaps5 #81). That harness *tests* a plugin; this manifest *describes* one. They are complementary: the harness can assert that a plugin's declared `capabilities` match what its tests exercise."

### The design

A published test crate - `umbral-compliance` (dev-dependency) - that a plugin author adds and drives from their own `tests/`. It takes the plugin under test as a `Box<dyn Plugin>` (plus a throwaway SQLite pool) and runs a suite of assertions. It reuses the framework's real machinery (`App::build`, the migration engine, the system-check phase, the OpenAPI generator) rather than re-implementing checks, so "passes compliance" means "passes what the framework itself would do at boot."

```rust
// In a third-party plugin's tests/compliance.rs
use umbral_compliance::ComplianceSuite;

#[tokio::test]
async fn billing_plugin_is_compliant() {
    ComplianceSuite::for_plugin(BillingPlugin::default())
        .with_dependency(AuthPlugin::default())   // satisfies cross-plugin FKs
        .run()
        .await
        .assert_pass();
}
```

`ComplianceSuite::run` returns a structured `ComplianceReport { checks: Vec<CheckOutcome> }` so the author can inspect individual results or gate CI on `assert_pass()`. Each check is independently addressable (a plugin with no routes is not penalized for the routes check - it is `Skipped`, keyed off the declared `capabilities`).

### The checks (one per contract the framework relies on)

| Check | What it asserts | How |
|---|---|---|
| **Migrations apply** | Every migration the plugin owns applies cleanly to a fresh SQLite DB, and (best-effort) round-trips down/up if reversible. | Build an app with just this plugin (+ declared deps), run `migrate`, assert the tracking table records each and DDL succeeds. Existing rows are the test - the suite also runs migrations against a DB seeded with a row, per the CLAUDE.md "existing rows are the test" rule. |
| **Route specs exist** | If `capabilities` claims `Routes`/`RoutePaths`, `route_paths()` is non-empty and every declared `RouteSpec` path is actually reachable in the merged router (no drift between declared and mounted). | Build the app, diff `route_paths()` against the live router's routes. This is the `routes()` vs `route_paths()` drift check the manifest spec §2.1 already references, promoted to a test. |
| **OpenAPI validates** | `openapi_paths()` output is a structurally valid OpenAPI fragment. | Reuse `umbral-rest`'s existing schema validator (`validate_schema_node`, `plugins/umbral-rest/src/lib.rs`) / the `umbral-openapi` generator; assert the produced document validates. |
| **System checks pass** | The plugin's `system_checks()` all return non-`Error` findings under a clean config, and any `Error` finding is *intentional and documented* (e.g. a required setting genuinely missing). | Run the phase-4 check mechanism (`crates/umbral-core/src/check.rs`) over the app; assert no unexpected `Error`. |
| **Settings wire** | Every `required_settings` entry from the manifest is actually read by the plugin (declared-vs-used), and a `required` setting missing produces the documented boot error rather than a panic. | Boot once with the settings present (pass) and once absent (assert a clean `Error` finding, not a crash). Cross-checks manifest §2.3. |
| **Admin registration** | If the plugin registers admin models/views, they resolve against the admin registry without collision. | Build with `AdminPlugin`; assert the plugin's admin registrations mount. `Skipped` if the plugin declares no admin surface. |
| **Auth wiring** | If the plugin depends on auth (extractors, permission classes), those resolve against `AuthPlugin` and a request with/without identity behaves as declared. | Build with `AuthPlugin`; exercise one authed + one anon request through the plugin's routes. `Skipped` if no auth dependency. |
| **Manifest consistency** | The plugin's declared `capabilities` and `owns_migrations` match the live trait object. | Exactly the drift cross-check from manifest spec §3.2, run as a test instead of at boot. This is the concrete realization of "the harness can assert that a plugin's declared `capabilities` match what its tests exercise." |
| **Semver compat** | The plugin's `manifest().umbral_req` admits the umbral version the harness itself was built against. | Parse `umbral_req`, match against `env!("CARGO_PKG_VERSION")` of `umbral-compliance`. Fails fast if the author declares a range that excludes the umbral they test on. |

### Why a separate crate, and why it reuses framework machinery

- **Separate crate, not built into `umbral-testing`.** Compliance is opt-in verification a third party depends on; keeping it in its own `umbral-compliance` crate (dev-dep) means the framework's own test utilities do not grow a public certification API they must keep stable forever, and a plugin author pulls in only what they need.
- **Reuse `App::build` / the check phase / the migration engine.** The whole value is that "compliant" == "the framework accepts it at boot." If the harness re-implemented the checks, it could drift from the real boot path - the exact bug class #80 is about. So each check drives the real code path with a test-shaped assertion on top.
- **Ties to the manifest (spec §6), not a duplicate of it.** The manifest *describes*; the harness *verifies the description is true*. The manifest-consistency and semver checks are literally the manifest spec's §3.2 drift checks, moved from boot-warning to test-failure so an author catches them before publishing.

### "Certification"

"Certified" is just "the compliance suite passes on the umbral version in `umbral_req`, in the plugin's own CI." There is no central authority in scope here (that is the marketplace, Stage 3, gaps5 #90). A future catalog badge can render off a plugin publishing its compliance report, but that is additive; this pass delivers the runnable suite.

### Open questions

1. Do we ship a `#[umbral_compliance::suite]` attribute macro that generates the `#[tokio::test]` boilerplate, or keep the explicit builder? (Recommend explicit builder first; macro once the shape settles.)
2. Should the migrations check require a Postgres run too (postgres-first per CLAUDE.md), or is SQLite sufficient for the harness with a Postgres opt-in? (Recommend SQLite default + `.with_postgres(url)` opt-in, since a third-party CI may not have PG.)

---

## #82 (tf#295): production scaffold profiles

### The problem, concretely

`umbral startproject` (`crates/umbral-cli/src/scaffold.rs::scaffold_project`) generates one excellent thing: a batteries-on blog-style quickstart that exercises every surface. That is right for *learning* and *starting*. It is not right for an org standing up a specific kind of production service: an API-only backend does not want the admin's templates and Tailwind bundle; a SaaS wants auth + sessions + tenancy + the hardened posture; a back-office wants the admin front-and-center; a BaaS wants REST + OpenAPI + auth with no server-rendered pages at all. And none of the four wants to hand-assemble the production posture (`docs/decisions/2026-08-08-enterprise-preset-design.md`) or remember to wire CI/deploy/observability.

### The design

Add `umbral startproject --profile <profile>` and an orthogonal `--prod-hardening` flag. The profile selects *which plugins and layout* the generated `main.rs` + `Cargo.toml` carry; `--prod-hardening` layers the `EnterprisePreset` (gaps5 #3) and the production system checks on top. They compose: `--profile saas --prod-hardening` is the common production path.

#### The four profiles

| `--profile` | Shape | Plugins the generated `main.rs` wires | Layout delta vs quickstart |
|---|---|---|---|
| `api` | JSON API, no server-rendered HTML. | `AuthPlugin` (token mode), `RestPlugin`, `OpenApiPlugin`, `SecurityPlugin` (with `/` API-exempt), `StoragePlugin` (media only), `LogsPlugin`. No admin, no templates, no Tailwind. | Drops `templates/`, `styles/`, `static/css`, `widgets/`, the HTML views. `views/` holds JSON handlers returning `Json<T>`/`ApiError`. |
| `saas` | Multi-tenant product with UI + API + billing-shaped seams. | Quickstart set **plus** `TenantsPlugin` (schema routing), `TasksPlugin` (background work), `HealthPlugin`, `LogsPlugin`; auth with signup. | Keeps templates + admin; adds a `tenants`/onboarding seed step and a tenant-scoped example model. |
| `backoffice` | Internal admin tool; the admin IS the app. | `AuthPlugin` (staff/superuser gated), `SessionsPlugin`, `AdminPlugin` front-and-center, `PermissionsPlugin`, `SecurityPlugin`, `LogsPlugin`, `StoragePlugin`. Minimal public routes. | `main.rs` mounts admin at `/`; the public `views/` shrink to a login + a redirect to `/admin/`. Richer `widgets/` starter. |
| `baas` | Backend-as-a-service: data API + auth for external frontends. | `AuthPlugin` (token + OAuth-ready), `RestPlugin` (safe-by-default perms), `OpenApiPlugin`, `RealtimePlugin` (SSE/WS), `SecurityPlugin`, `StoragePlugin`, `LogsPlugin`. | Like `api` plus realtime + an `oauth` commented-in stanza; CORS config stub for a separate frontend origin. |

Every profile is *just a different set of `.plugin(...)` lines + a layout delta* over the existing generator - no new runtime privilege, consistent with the "every capability is a plugin" motto. The generator already emits a `main.rs` with a plugin list and a `Cargo.toml` with active-vs-commented plugin lines; a profile is a data table that toggles which lines are active and which template files are written. This reuses `scaffold_project`'s existing machinery (the `[(path, body)]` template loop, the commented-plugin convention in the generated `Cargo.toml`); it does not fork the generator.

The default (no `--profile`) stays exactly the current quickstart, so nothing changes for learners.

#### `--prod-hardening`

Orthogonal flag, composes with any profile (and with the default). It makes the generated `main.rs`:

1. **Call the EnterprisePreset.** `App::builder().preset(EnterprisePreset::default())` in place of hand-wiring `SecurityPlugin`/`SessionsPlugin`/etc. The preset design (`docs/decisions/2026-08-08-enterprise-preset-design.md`) already specifies this bundle: `SecurityPlugin` (HSTS, starter CSP, frame `DENY`), `SessionsPlugin` (secure cookies, SameSite, max-age), `AuthPlugin`, `HealthPlugin`, `LogsPlugin`, and (when present) metrics + distributed throttle, plus trusted-proxy handling and host validation. The scaffold flag is literally the second delivery surface that design names ("`umbral startproject --prod-hardening` (and/or `--profile ...`, ties to gaps5 #82) generates a `main.rs` that calls the preset, so the wiring is visible and editable rather than hidden").
2. **Add the `umbral-enterprise` dep** (the meta-crate the preset design recommends, keeping the facade plugin-free) to the generated `Cargo.toml`.
3. **Default `environment` toward Prod-readiness.** The generated `umbral.toml`/`.env` get a documented `environment = "Prod"` path and a note that the boot-time production system checks (secret_key not default, dev/debug off, secure cookies + HSTS under TLS, host validation, trusted-proxy list, Postgres-not-SQLite) will fail boot if unsatisfied - the checks the preset design §"Production system checks" specifies. The scaffold does not weaken them; it wires them and documents them.

`--prod-hardening` without a profile hardens the quickstart; with a profile it hardens that profile. The preset remaining equivalent to hand-wiring (the preset design's core promise) means the generated `main.rs` can always be expanded back to explicit `.plugin(...)` lines - the scaffold can even offer `--prod-hardening --explicit` to emit the un-bundled form for readers who want to see every layer.

#### CI / deploy / observability, wired per profile

Each profile writes, in addition to source:

- **CI** - a `.github/workflows/ci.yml` running `fmt` + `clippy` + `build` + `test` + `migrate` against a throwaway DB, plus (for `api`/`baas`) an OpenAPI-diff step. A `--prod-hardening` project additionally runs the production system checks in CI (boot under `environment = "Prod"` with a test secret) so an unsafe config fails CI, not prod.
- **Deploy** - a `Dockerfile` (multi-stage, distroless-ish final image) and a `.dockerignore`; for `saas`/`backoffice` a `compose.yaml` with a Postgres service; a documented `RUN migrate` step in the entrypoint. Deploy artifacts are opinionated defaults, editable like everything else.
- **Observability** - the quickstart already wires `umbral-logs` structured logging + optional OTLP export (`umbral_logs::observability::init`); profiles keep that and, under `--prod-hardening`, uncomment the `otel` feature and add the `OTEL_EXPORTER_OTLP_ENDPOINT`/`OTEL_SERVICE_NAME` env stubs to `.env.example`, plus (when the metrics crate exists, gaps5 #64) a `/metrics` note.

### Why profiles as data over the one generator

The generator is already the right shape: a table-of-contents `main.rs`, a plugin list, a Cargo file with active/commented plugin lines, and a `[(path, body)]` template loop. A profile is a *selection* over that, not a second generator. This keeps one code path to maintain (the CLAUDE.md "one generator to maintain" instinct that already folded `startapp` into `startplugin`), and it keeps every profile honestly equal to "the quickstart with a different plugin set" - which is the whole umbral thesis. The alternative (a bespoke generator per profile) would drift the four apart and duplicate the template-writing logic four ways.

### Interaction with the reserved-name / validation machinery

Profiles change *which files and deps* are written, not the name-validation or reserved-plugin logic; `scaffold_project` runs the same `validate_name` and reservation checks first. The profile branch sits after validation, selecting the plugin table and template set. No change to `RESERVED_PLUGIN_NAMES` or the command-name reservation.

### Open questions

1. Profile-specific example *models* (a tenant-scoped model for `saas`, none for `api`) vs a shared `Post` everywhere? (Recommend: a minimal profile-appropriate model - `api`/`baas` get a resource-shaped model with REST config, `backoffice` gets an admin-registered one, `saas` gets a tenant-scoped one - because the example is the profile's teaching surface.)
2. Does `--prod-hardening` pull `umbral-enterprise` unconditionally, or only when the security-relevant plugins are present? (Recommend: unconditionally under the flag; the meta-crate exists precisely so the flag is one dep + one `.preset(...)` line.)
3. Ship all four profiles at once, or land `api` + `--prod-hardening` first (the two with the clearest demand) and add `saas`/`backoffice`/`baas` as the plugin dependencies they need - tenants, realtime - stabilize? (Recommend: `api` + hardening first; it needs nothing not already shipped.)

---

## Cross-cutting notes

- **All four are quality gates, not runtime features.** #79 is a wire contract, #80 a CI process, #81 a test crate, #82 a generator selection. None adds a new privileged runtime path; each makes an existing contract legible/enforceable from outside.
- **They reinforce each other.** #79's per-code docs pages are link-checked by #80. #81's manifest-consistency check is #80's drift philosophy applied to plugins and reuses the manifest spec. #82's generated CI runs #80's `docs-check` and (under hardening) the production system checks; a `--prod-hardening` project can also run #81's compliance suite against any local plugins it scaffolds.
- **Dependencies.** #82 depends on the EnterprisePreset (#3) and the `umbral-enterprise` meta-crate landing; until metrics (#64) and distributed throttling (#67) exist the preset (and thus the hardened scaffold) warns they are recommended for multi-replica prod, per the preset design. #81 depends on the manifest (`docs/specs/plugin-manifest-and-registry.md`) for its consistency/semver checks. #79 and #80 stand alone.
