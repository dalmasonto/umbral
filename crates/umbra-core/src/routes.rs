//! Route registry — a snapshot of every URL path the framework knows
//! about, grouped by plugin.
//!
//! The registry is populated once at `App::build()` time from two
//! sources:
//!
//! 1. The implicit `"app"` plugin's path list, set via
//!    `AppBuilder::route_paths(&[...])`. Optional — apps that don't
//!    call it get an empty `"app"` entry.
//! 2. Each registered plugin's `Plugin::route_paths()` contribution,
//!    walked in topological dependency order.
//!
//! The registry is opt-in for surfacing. Currently the only consumer
//! is the dev-mode default 404 template, which renders the path list
//! so a developer who hits a typoed URL can see what's available
//! without grepping the router tree. The registry is read by
//! `crate::errors::render_not_found` only when `settings.environment
//! == Dev`, so production 404 responses stay minimal.
//!
//! ## What this is *not*
//!
//! The registry is a *declared* list, not a live introspection of
//! axum's route table. axum doesn't expose its internal `RouteTable`,
//! so plugins that contribute routes through `Plugin::routes()`
//! report them via this companion `Plugin::route_paths()` method. The
//! two can drift — if a plugin author adds a `.route("/foo", ...)` to
//! its `routes()` method but forgets to add `"/foo"` to
//! `route_paths()`, the registry won't mention it. The cost of that
//! drift is "404 page is slightly stale," not "framework is broken."
//!
//! For the user's hand-registered routes (`AppBuilder::router(...)`),
//! the same shape applies: call `AppBuilder::route_paths(&[...])`
//! alongside `.router(...)` to surface them. Skipping it is fine; the
//! registry just won't list those paths.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Snapshot of declared routes, keyed by plugin name. The implicit
/// `"app"` plugin holds the user's hand-registered paths; built-in
/// and third-party plugins hold their own contributions.
///
/// Iteration order is alphabetical by plugin name (BTreeMap), which
/// gives the 404 template a stable, human-friendly listing without
/// the framework picking an arbitrary plugin to show first.
#[derive(Debug, Clone, Default)]
pub struct RouteRegistry {
    pub by_plugin: BTreeMap<String, Vec<String>>,
}

impl RouteRegistry {
    /// Total number of declared paths across every plugin. Used by
    /// the 404 template's pluralisation and by tests asserting that
    /// at least *something* got registered.
    pub fn total(&self) -> usize {
        self.by_plugin.values().map(|v| v.len()).sum()
    }
}

static REGISTRY: OnceLock<RouteRegistry> = OnceLock::new();

/// Publish the registry. Called from `App::build()` after every
/// plugin's `route_paths()` has been collected. Safe to call exactly
/// once; subsequent calls are no-ops.
pub fn init(registry: RouteRegistry) {
    let _ = REGISTRY.set(registry);
}

/// Read the registry. Returns `None` if `init` hasn't been called
/// (production binaries that bypass `App::build()`, tests that
/// short-circuit the build flow). Callers should treat `None` as
/// "no routes to surface" rather than as an error.
pub fn get() -> Option<&'static RouteRegistry> {
    REGISTRY.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_sums_per_plugin_paths_and_handles_empty_groups() {
        let mut reg = RouteRegistry::default();
        reg.by_plugin
            .insert("app".to_string(), vec!["/".into(), "/articles".into()]);
        reg.by_plugin.insert(
            "admin".to_string(),
            vec!["/admin/".into(), "/admin/login".into(), "/admin/logout".into()],
        );
        reg.by_plugin.insert("sessions".to_string(), Vec::new());

        assert_eq!(reg.total(), 5);
    }
}
