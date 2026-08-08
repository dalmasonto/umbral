# OpenAPI: handwritten-route coverage and a breaking-change checker

Status: draft (design). Covers gaps5 #35 (tf#248) and gaps5 #36 (tf#249).

## Problem

`umbral-openapi` today documents exactly one thing: the CRUD surface `umbral-rest` auto-generates from models, plus each `@action` and each plugin's `Plugin::openapi_paths()` contribution. The plugin's own module docs say so (`plugins/umbral-openapi/src/lib.rs:16-19`):

> v1 only describes umbral-rest's auto-generated endpoints. Hand-written routes the user added on the builder are not in scope.

Two consequences follow, and they are the two items here:

1. **#35 coverage gap.** A route a user mounts by hand (through the `Routes` builder or a plugin's `routes()`), a login form POST, a webhook receiver, a custom JSON endpoint, is invisible to the spec, to Swagger UI, and to the generated client. The user has no in-framework way to say "this handwritten route accepts X and returns Y." OpenAPI documentation is a REST-plugin side effect rather than a property of the normal handler path.

2. **#36 no contract guard.** The spec is generated but never compared across versions. Nothing tells CI that renaming a field, removing a path, or tightening a type just broke every downstream client. `STABILITY.md` promises "we will not knowingly break the Stable tier in a PATCH" and that "every breaking change to the Stable tier ships with a migration note." For the generated API that promise is currently unenforced: a breaking change to a model silently rewrites the spec with no gate.

Both are marked HIGH in gaps5.

## The seams that already exist (read this before proposing anything)

The design below reuses live plumbing rather than inventing a parallel path. The real seams:

### Route registration

- `crates/umbral-core/src/routes.rs` defines `RouteSpec { path, methods, permission }` (routes.rs:57) and the `Routes` builder (routes.rs:201). Every `Routes::get / post / put / patch / delete / route / route_gated` call pushes a `RouteSpec` AND mounts the axum handler in one call, so the declared list cannot drift from what is served. `into_parts()` (routes.rs:384) hands back `(Router, Vec<RouteSpec>)`.
- The user binary registers its routes with `AppBuilder::routes(Routes::new()...)`. Plugins get the same drift-free path through `Plugin::routes_builder() -> Option<Routes>` (plugin.rs:223); the legacy pair is `Plugin::routes()` + `Plugin::route_paths()` (plugin.rs:185, plugin.rs:249).
- `RouteSpec.permission` already exists as an example of enriching a spec with metadata the framework surfaces elsewhere (the ungated-route boot audit). Its doc comment (routes.rs:66) literally names "future OpenAPI security annotations" as the intended second consumer. That is the extension point #35 lands on.

### OpenAPI path collection

`umbral-openapi`'s `build_spec` (plugins/umbral-openapi/src/lib.rs:310) assembles `paths` from three sources, in order:

1. Auto-CRUD, by walking `umbral::migrate::registered_plugins()` and `models_for_plugin`, filtered by `umbral_rest::is_exposed(table)` and mounted at `umbral_rest::registered_base_path()` (lib.rs:332-403).
2. **Plugin contributions**, merged from `umbral::routes::registered_openapi_paths()` (lib.rs:410-414). That registry is populated once at `App::build()` via `umbral::routes::init_openapi(entries)` (routes.rs:446), fed by every plugin's `Plugin::openapi_paths() -> Vec<(String, serde_json::Value)>` (plugin.rs:274).
3. Custom actions, from `umbral_rest::registered_action_schemas() -> Vec<ActionSchema>` (umbral-rest/src/lib.rs:2064), each lowered by `action_path_item` (lib.rs:471).

`ActionSchema` (umbral-rest/src/lib.rs:2045) is the closest existing precedent for "a non-CRUD endpoint declares its own request/response schema": it carries `input_schema: Option<Value>` and `output_schema: Option<Value>`, both raw OpenAPI schema objects, and a schemaless action still appears with a generic 200.

The load-bearing observation: **umbral-openapi already merges `registered_openapi_paths()` blindly.** Anything that lands in that core registry at build time shows up in the spec, Swagger UI, and the generated client with zero further changes to the openapi plugin. #35 is therefore a matter of feeding handwritten-route operations into that one registry.

### CLI subcommands

`umbral-cli`'s `dispatch` (crates/umbral-cli/src/lib.rs:288) runs plugin commands first, then a fixed built-in set. A plugin ships subcommands via `Plugin::commands() -> Vec<Box<dyn PluginCommand>>` (plugin.rs:511). `umbral-openapi` already does this: it ships `gen-client` (plugins/umbral-openapi/src/lib.rs:223, 1348) as a `PluginCommand` whose `needs_ready()` returns `false` (lib.rs:1362) because it reads the registry offline, with no DB and no live server, and supports a `--check` CI-gate flag (lib.rs:1384). That command is the template `openapi diff` copies.

## #35 - Handwritten routes carry their own OpenAPI operation

### Shape

Make the OpenAPI operation a property of the route spec, declared where the route is mounted, then fold documented specs into the existing `init_openapi` registry at build time. Three additive pieces:

**1. `RouteSpec` gains an optional operation.** Add one field:

```rust
pub struct RouteSpec {
    pub path: String,
    pub methods: Vec<&'static str>,
    pub permission: Option<String>,
    /// The OpenAPI Operation Object(s) for this route, keyed by lowercased
    /// HTTP method (`"get"`, `"post"`). `None` means undocumented: the route
    /// is served and appears in the dev 404 registry, but contributes nothing
    /// to the spec (today's behaviour for every handwritten route).
    pub operations: Option<BTreeMap<String, serde_json::Value>>,
}
```

The value is an OpenAPI 3.0 Operation Object serialized as `serde_json::Value`, the same currency `openapi_paths()` and `ActionSchema` already trade in. `None` keeps every existing route exactly as it is (undocumented), so the field is purely additive and no caller is forced to change.

**2. `Routes` builder gets documented variants.** The bare `.get(path, handler)` stays undocumented. A documented route is declared by attaching an operation:

```rust
Routes::new()
    .get("/health", health)                       // undocumented, as today
    .get_documented("/report/{id}", report, Op::new()
        .path_param("id", "string", "Report id")
        .response::<ReportView>(200, "The rendered report")
        .response_status(404, "Not found"))
    .post_documented("/webhooks/stripe", stripe_hook, Op::new()
        .request::<StripeEvent>("Stripe event payload")
        .response_status(202, "Accepted"))
```

`Op` is a small builder in `umbral-core` that lowers to an Operation Object `Value`. It offers the ergonomic 80 percent (path/query params, request body, keyed responses, tags, summary, operationId, and a `security` hook that reuses `RouteSpec.permission`) plus a raw escape hatch `Op::from_value(v)` for anything it does not model. Under the hood `get_documented` calls the same `with_method` path as `get`, then sets `operations`, so the axum handler and the spec come from one call and cannot drift.

`Op::request::<T>()` / `response::<T>()` need `T`'s JSON schema. Two supported ways to get it, mirroring how actions already work:

- **Typed.** `T: ApiSchema`, a trait that yields a `#/components/schemas/...` entry, satisfiable by `#[derive(ApiSchema)]` (the natural sibling of the existing `#[derive(Dto)]` from gaps3 #29). The derive walks the struct fields the same way `model_schema` walks columns and emits the component once, referenced by `$ref`.
- **Raw.** `Op::request_schema(value)` / `response_schema(status, value)` take a `serde_json::Value` directly, exactly as `ResourceConfig::action_input_schema` / `action_output_schema` do today. This is the guaranteed floor: no derive required, and it is what `ActionSchema` already proves works end to end.

**3. Plugins document routes the same way.** A plugin that already implements `routes_builder()` gets documentation for free by switching its `.get(...)` calls to `.get_documented(...)`. Nothing new to implement: `routes_builder()` already returns a `Routes`, and the operations ride along in the specs it yields.

### Wiring: one new fold at `App::build()`, zero change to umbral-openapi

`App::build()` already collects `RouteSpec`s from the app `Routes` and from every plugin (via `routes_builder()` / `route_paths()`) to populate the `RouteRegistry`. Add a second pass over that same collected list: for every spec whose `operations` is `Some`, emit `(path, path_item_value)` and hand the merged vec to the existing `umbral::routes::init_openapi(...)` alongside the plugin `openapi_paths()` contributions.

Because `build_spec` already reads `registered_openapi_paths()` and merges it last-write-wins (lib.rs:410-414), **umbral-openapi needs no change at all**: handwritten operations arrive through the identical registry the plugin already consumes. Documented app routes get grouped under a default tag (for example `app`) so Swagger UI buckets them separately from the model resources. `#[derive(ApiSchema)]` components are registered into the same `components.schemas` map the spec already builds.

This is the core-principle payoff: OpenAPI stops being a REST-plugin afterthought. A route documents itself at the point it is mounted, through the normal `Routes` builder, and any spec consumer (Swagger UI, `gen-client`, the diff tool in #36) picks it up. A REST-free app that mounts only handwritten routes can still emit a complete spec, because the coverage no longer depends on `umbral-rest` walking the model registry.

### Why not a separate annotation registry or a macro on the handler

An attribute macro on the handler function (`#[openapi(...)] async fn report(...)`) was the obvious alternative. Rejected: axum exposes no route-table introspection, so a handler-side macro still cannot know the path or method the route is mounted at, which is the drift `routes.rs` warns about repeatedly (routes.rs:22-39). Declaring the operation at the mount site, next to the path and method, is the only place all three facts are known together. This is the same reason `RouteSpec` records the permission at the mount site rather than on the handler.

## #36 - `umbral openapi diff --breaking`

### Shape

A new offline subcommand shipped by `OpenApiPlugin::commands()` next to `gen-client`, so it inherits the same "no DB, no live server, `needs_ready() == false`" posture. Because the prompt spells it `umbral openapi diff`, the `PluginCommand` returns a parent `clap::Command::new("openapi")` with a `diff` (and `snapshot`) subcommand, rather than a flat `openapi-diff`.

```
umbral openapi snapshot --out openapi/0.0.11.json
    # build the current spec (reuse build_spec) and write it, the committed baseline

umbral openapi diff --base openapi/0.0.11.json [--current <file>]
    # compare a prior spec to the current one (current defaults to the freshly
    # built spec from the live registry, same source gen-client reads)
    --breaking          # exit non-zero if any BREAKING change is found (CI gate)
    --changelog <file>  # write a grouped markdown changelog (default: stdout)
    --format text|json  # machine-readable output for other tooling
```

`snapshot` reuses `build_spec` (the function behind `spec_handler`) so the baseline is byte-for-byte what the server would serve. `diff` builds the current spec the same way when `--current` is omitted, so the common CI flow is "compare the committed `openapi/<version>.json` against the spec the current code would emit."

### Classification (semver-aware)

The diff walks both specs structurally (`paths`, then per-path operations, then parameters / request body / responses / `components.schemas`) and classifies every difference into one of three buckets. The rules follow the standard OpenAPI-diff (oasdiff-style) contract:

**Breaking** (a conforming client can stop working):

- A path or an operation (method) present in base is removed.
- A required request parameter is added, or an existing parameter flips optional to required.
- A parameter or a request/response field type is narrowed (`integer` to `string`, wider to narrower `format`, a removed `enum` member on a request-input position, an added `enum` restriction where none existed).
- A new required field is added to a request body schema, or a required field is added where the caller must now supply it.
- A field is removed from a response schema, or a response `enum` gains a value the client did not know (breaking for consumers that switch exhaustively), or a documented success response status is removed.
- `maxLength` / `minimum` / `maximum` / `pattern` tightened on an input.

**Additive** (safe, non-breaking):

- A new path, operation, optional parameter, optional request field, or new response field.
- A new success response status.
- A widened type or a relaxed constraint on an input.

**Neutral** (no client impact): description / summary / example / tag / `operationId` text changes, reordering.

Foreign-key and M2M vendor extensions (`x-umbral-fk-ref`, `x-umbral-m2m-*`) are compared structurally too, so a changed relation target is caught. Removal of a `readOnly` / `writeOnly` / `x-umbral-*` marker is classified by whether it loosens or tightens the input contract.

### CI gate and changelog

`--breaking` makes the command exit non-zero when the Breaking bucket is non-empty, exactly like `gen-client --check` exits non-zero on drift (lib.rs:1409-1423). A CI job runs `umbral openapi diff --base openapi/$(last_released_version).json --breaking` and fails the build on any breaking change, which operationalizes the STABILITY.md clause below.

`--changelog` emits keep-a-changelog-shaped markdown grouped `Breaking` / `Added` / `Changed`, each entry naming the path, method, and field, so the human-facing "migration note in the changelog" that STABILITY.md requires is generated rather than hand-written:

```
## API changes 0.0.11 -> 0.0.12

### Breaking
- Removed `DELETE /api/article/{id}`
- `POST /api/order`: field `currency` is now required
- `GET /api/user/{id}`: response field `email` removed

### Added
- `GET /api/article/{id}/related/` (new action)
- `POST /api/order`: optional field `coupon`
```

### Tie to STABILITY.md

`STABILITY.md` names the `umbral` CLI subcommands and the auto-generated REST surface as part of the **Stable** tier, and commits that breaking changes to that tier land only in a MINOR bump with a changelog migration note, never silently in a PATCH. Nothing enforced that for the generated API until now. `umbral openapi diff --breaking` is the enforcement:

- In CI on a PATCH branch, a non-empty Breaking bucket fails the build, catching a would-be silent break before release.
- On a MINOR bump, the `--changelog` output is the required migration note, pasted into the release changelog.
- The tool's Breaking / Additive classification is the concrete, checkable definition of "knowingly break," turning a prose promise into a gate. A short "Generated API surface" clause is added to STABILITY.md pointing at this command as the mechanism, and to the committed `openapi/<version>.json` baselines as the versioned record of the contract.

Deprecation windows apply unchanged: a field slated for removal is first marked (a `deprecated: true` on the schema property, which OpenAPI supports natively), which the diff reports as Additive/Neutral, then removed a minor later, which the diff reports as Breaking. The tool sees the whole cycle.

## Scope and sequencing

- #35 first: it is the prerequisite that makes handwritten routes visible; the diff tool is only as complete as the spec it compares. The floor for #35 is the raw-`Value` path (`Op::request_schema` / `response_schema`), which needs no new derive and reuses the exact mechanism `ActionSchema` already ships. `#[derive(ApiSchema)]` is the ergonomic layer on top and can follow.
- #36 ships as a `PluginCommand` on `OpenApiPlugin`, offline, `needs_ready() == false`, modeled on `gen-client`. Baselines live in a committed `openapi/` directory.
- User-facing docs: one page under `documentation/docs/v0.0.1/openapi/` for each (`documenting-routes.mdx`, `openapi-diff.mdx`) per the ship-a-feature-ship-its-doc rule.

## Files this touches

- `crates/umbral-core/src/routes.rs`: `RouteSpec.operations` field, `Op` builder, `Routes::*_documented` methods.
- `crates/umbral-core/src/app.rs`: fold documented specs into `routes::init_openapi(...)` at build time.
- `crates/umbral-macros`: `#[derive(ApiSchema)]` (optional, ergonomic layer).
- `plugins/umbral-openapi/src/lib.rs`: register `ApiSchema` components; new `openapi diff` / `snapshot` `PluginCommand`. No change to `build_spec`'s path-merge, which already consumes `registered_openapi_paths()`.
- `STABILITY.md`: a "Generated API surface" clause pointing at `umbral openapi diff --breaking`.

## See also

- `plugins/umbral-openapi/src/lib.rs` (`build_spec`, `action_path_item`, `GenClientCommand`).
- `crates/umbral-core/src/routes.rs` (`RouteSpec`, `Routes`, `init_openapi`, `registered_openapi_paths`).
- `crates/umbral-core/src/plugin.rs` (`openapi_paths`, `routes_builder`, `route_paths`, `commands`).
- `STABILITY.md` (API tiers, deprecation windows, the Stable-tier PATCH promise).
- gaps5 #35 (tf#248), gaps5 #36 (tf#249).
