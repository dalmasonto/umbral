# Plugin Route Metadata Can Drift From Actual Routes

Category: Correctness, Simplicity
Severity: Low

## Finding

The plugin contract allows `routes()` and `route_paths()` to diverge. The current source comments say this only causes stale 404 debugging metadata, but route metadata is also the kind of surface that tends to feed OpenAPI, audits, admin views, docs, and permission review over time.

## Evidence

- `crates/umbral-core/src/routes.rs:22-31` documents that `route_paths` can drift from actual `routes()`.
- Core route collision and inspection features depend on route metadata staying accurate.

## Risk

If route metadata becomes an audit or policy source, stale paths can hide exposed routes or report routes that are no longer mounted.

## Recommendation

Reduce duplication between mounted routes and route metadata:

- Register routes through a builder that records paths as routes are attached.
- Add plugin tests that compare declared route paths to mounted route specs.
- Treat missing route metadata as a warning for plugins that expose HTTP routes.

## Suggested Tests

- A plugin that mounts a route but omits it from `route_paths()` fails a route metadata check.
- Route registry output matches mounted routes for all built-in plugins.

