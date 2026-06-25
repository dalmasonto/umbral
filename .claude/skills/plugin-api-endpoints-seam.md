---
name: plugin-api-endpoints-seam
description: Use when a plugin needs to advertise callable HTTP endpoints to another plugin (e.g. a REST API root, a service-discovery JSON) without the two crates depending on each other. Covers the Plugin::api_endpoints() → core global → consumer wiring.
---

# The `Plugin::api_endpoints()` discovery seam

## Context

A plugin (say `umbral-oauth`) mounts routes like `/oauth/google/login`. Another plugin (say `umbral-rest`) wants to *list* those routes in an API-root index, or a SPA wants to fetch them. The hard constraint: **the consumer must not depend on the producer's crate** — `umbral-rest` can't `use umbral_oauth::…`, because Cargo's no-circular-dep ban is what proves "REST is optional / plugins are independent." So you need a way for the producer to *declare* endpoints and the consumer to *discover* them generically.

This seam is how. It mirrors the existing `registered_models()` / `model_aliases` registries.

## Approach

Three moving parts:

1. **The core type + trait method** (`crates/umbral-core/src/plugin.rs`):
   - `ApiEndpoint { group, name, method, path, label }` — origin-agnostic, **relative `path` only**, re-exported from the facade (`umbral::plugin::ApiEndpoint`).
   - `fn api_endpoints(&self) -> Vec<ApiEndpoint> { Vec::new() }` — a default trait method, so plugins opt in by overriding and everyone else contributes nothing.

2. **The collection at build** (`crates/umbral-core/src/app.rs`, right after `init_plugin_order`): `App::build()` already walks `sorted_plugins`; it extends a `Vec` with each `plugin.api_endpoints()` and publishes it via `migrate::init_api_endpoints(...)` into a `OnceLock<Vec<ApiEndpoint>>`.

3. **The read** (`crates/umbral-core/src/migrate.rs`): `pub fn registered_api_endpoints() -> Vec<ApiEndpoint>` returns the global (empty before build). The consumer plugin reads *this*, never the producer crate. `umbral-rest`'s `GET /api/` root calls it and joins the incoming request's origin onto each relative `path` to produce an absolute `url`.

Producer side (the plugin advertising): override `api_endpoints()` to return rows. If the plugin *also* exposes the same data another way (umbral-oauth serves both `GET /oauth/providers` **and** `api_endpoints()`), drive both from **one shared descriptor builder** (`OAuthPlugin::provider_links()`) so they can't drift.

## Why

`registered_plugins()` returns `Vec<String>` — plugin **names**, not `Box<dyn Plugin>`. `App::build()` consumes the sorted plugin list during build; there's no retained registry of live trait objects to call `.api_endpoints()` on at request time. So a global populated at build is the only seam available — and it's the same pattern every other cross-plugin lookup here uses (`REGISTRY`, `MODEL_ALIASES`, `PLUGIN_ORDER`). Don't try to thread plugin objects into the consumer; publish to a global.

Relative `path` in the core type (no origin) keeps `ApiEndpoint` honest: core can't know the public host. The consumer decides the origin — `umbral-rest` reads the request `Host` (+ `X-Forwarded-Proto`); `umbral-oauth`'s own `/oauth/providers` uses its configured `redirect_base`. Either is fine; baking an origin into the core type is not.

## Pitfalls

- **Don't add a dep from the consumer to the producer to "just list its routes."** That's the exact coupling the seam exists to avoid. If you're typing `umbral-oauth` in `umbral-rest/Cargo.toml`, stop.
- **Test the seam with a local stand-in plugin, not the real producer.** `plugins/umbral-rest/tests/api_root.rs` defines a tiny `DiscoveryPlugin` implementing `api_endpoints()` and asserts the root surfaces it — proving aggregation works with zero `oauth↔rest` dependency. A test that imported `umbral-oauth` would silently reintroduce the coupling.
- **`registered_api_endpoints()` is empty until `App::build()` runs.** Anything reading it at request time is fine; anything reading it during build setup may see nothing.
- **One descriptor builder when a producer has two discovery surfaces**, or they drift the first time someone adds a provider/route.
- **Guard the consumer's index route against an empty base.** `umbral-rest` skips mounting `GET /api/` when its base path is `""` (mounted at root), else `/` collides with the app's home route.

## See also

- Spec: `docs/superpowers/specs/2026-06-13-oauth-spa-token-and-endpoint-discovery-design.md`
- Producer: `plugins/umbral-oauth/src/lib.rs` (`provider_links`, `api_endpoints`), `routes.rs` (`/oauth/providers`).
- Consumer: `plugins/umbral-rest/src/lib.rs` (`api_root`, `request_origin`).
- Core: `crates/umbral-core/src/{plugin.rs, app.rs, migrate.rs}`.
- The dependency-inversion rule this upholds: `CLAUDE.md` → "Dependency inversion is the whole game."
