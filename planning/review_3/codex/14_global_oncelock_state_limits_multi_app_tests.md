# Global OnceLock State Limits Multi-App And Test Isolation

Category: Correctness, Simplicity, Testability
Severity: Medium

## Finding

Several core and plugin systems use process-global `OnceLock` registries or configs. This makes the first initialized app/config win for the entire process. It is simple for one production app, but brittle for tests, examples, embedded apps, and dynamic app construction.

## Evidence

- `crates/umbral-core/src/db.rs` uses global pool state.
- `crates/umbral-core/src/routes.rs` uses a global route registry.
- `crates/umbral-core/src/static_files.rs` uses global static route state.
- `plugins/umbral-openapi/src/lib.rs:196` stores plugin config in a global `OnceLock`.
- `plugins/umbral-openapi/src/lib.rs:242` ignores `CONFIG.set(self.clone())` failures.

## Risk

Tests can leak app configuration across cases. Multiple apps in the same process can read stale plugin or routing metadata. Ignored `OnceLock::set` failures hide the moment configuration becomes stale.

## Recommendation

Move runtime registries into `AppContext` where feasible. For unavoidable globals:

- Return an error when setting different config after initialization.
- Provide test-only reset handles.
- Document the one-app-per-process assumption explicitly.

## Suggested Tests

- Build two apps with different OpenAPI/route/static config in the same process and assert no stale data leaks.
- Repeated tests with isolated temp apps should pass in any order.

