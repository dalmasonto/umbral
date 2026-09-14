//! A framework-level request/response middleware contract (feature #68).
//!
//! axum/tower already give you `Layer` + `Service`, but writing one
//! correctly means understanding poll-readiness, `BoxFuture`, and the
//! `Service` trait's ownership rules. Most application middleware only
//! wants two things: *look at the request before the handler*, and
//! *look at the response after*. The [`Middleware`] trait is that
//! narrow, ergonomic surface: a request-side hook and a
//! response-side hook, typed for Rust.
//!
//! Plugins contribute middleware via `Plugin::middleware`; an app adds
//! its own via `AppBuilder::middleware`. `App::build` collects them all
//! into one [`MiddlewareStack`] and installs it as a single axum layer.
//!
//! ## Composition (the onion)
//!
//! `before_request` hooks run in registration order; `after_response`
//! hooks run in the *reverse* order, so each middleware wraps the ones
//! registered after it — the standard onion model that makes
//! composition predictable. A `before_request` may short-circuit by
//! returning `Err(response)`: the handler and every later middleware are
//! skipped, and only the `after_response` hooks of the middleware that
//! already ran (in reverse) get to see the short-circuit response.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

/// A typed request/response middleware. Implement either hook (or both);
/// the defaults pass through untouched.
///
/// ```ignore
/// use umbral::prelude::*;
/// use axum::extract::Request;
/// use axum::response::Response;
///
/// struct RequestId;
///
/// #[umbral::async_trait]
/// impl Middleware for RequestId {
///     async fn before_request(&self, mut req: Request) -> Result<Request, Response> {
///         req.headers_mut().insert("x-request-id", new_id().parse().unwrap());
///         Ok(req)
///     }
/// }
/// ```
#[async_trait]
pub trait Middleware: Send + Sync + 'static {
    /// A short label for diagnostics. Defaults to the type name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Declarative position in the chain. **Lower values are OUTER** — the
    /// middleware's `before_request` runs earlier and its `after_response`
    /// runs later (onion order). Middleware with equal `order` keep their
    /// registration order (app-level before plugin-level; plugins in
    /// dependency order). `MiddlewareStack::apply` stable-sorts by this
    /// before installing, so a middleware can place itself relative to
    /// others (e.g. a session loader at `-100`, an auth gate at `-50`)
    /// without depending on registration timing. Default `0`.
    fn order(&self) -> i32 {
        0
    }

    /// Inspect or modify the request before it reaches the handler.
    ///
    /// Return `Ok(req)` to continue (with the possibly-modified request),
    /// or `Err(response)` to short-circuit: the handler and all later
    /// middleware are skipped, and the response unwinds back out through
    /// the `after_response` hooks of the middleware that already ran.
    ///
    /// Default: pass the request through unchanged.
    async fn before_request(&self, req: Request) -> Result<Request, Response> {
        Ok(req)
    }

    /// Inspect or modify the response on the way out.
    ///
    /// Default: pass the response through unchanged.
    async fn after_response(&self, res: Response) -> Response {
        res
    }
}

/// An ordered set of [`Middleware`], collected from the app builder and
/// every plugin, installed as one axum layer by `App::build`.
#[derive(Clone, Default)]
pub struct MiddlewareStack {
    middleware: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareStack {
    /// An empty stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one middleware to the end of the stack. Its `before_request`
    /// runs after every middleware already in the stack; its
    /// `after_response` runs before them (onion order).
    pub fn push(&mut self, mw: Arc<dyn Middleware>) {
        self.middleware.push(mw);
    }

    /// Append every middleware from `other`, preserving order.
    pub fn extend(&mut self, other: impl IntoIterator<Item = Arc<dyn Middleware>>) {
        self.middleware.extend(other);
    }

    /// True when no middleware is registered — `App::build` skips
    /// installing the layer entirely in that case.
    pub fn is_empty(&self) -> bool {
        self.middleware.is_empty()
    }

    /// Number of registered middleware.
    pub fn len(&self) -> usize {
        self.middleware.len()
    }

    /// Wrap `router` with this stack as a single axum middleware layer.
    /// A no-op (returns the router unchanged) when the stack is empty.
    pub fn apply(mut self, router: axum::Router) -> axum::Router {
        if self.middleware.is_empty() {
            return router;
        }
        // Declarative ordering: stable-sort by `Middleware::order` (lower =
        // outer) so chain position is controllable independent of
        // registration timing. `sort_by_key` is stable, so equal-`order`
        // middleware keep their insertion order (app before plugins).
        self.middleware.sort_by_key(|mw| mw.order());
        let state = Arc::new(self.middleware);
        router.layer(axum::middleware::from_fn_with_state(state, run_stack))
    }
}

/// The axum middleware fn that drives one [`MiddlewareStack`] per request:
/// run the `before_request` hooks in order (short-circuiting on the first
/// `Err`), invoke the handler, then run the `after_response` hooks of the
/// middleware that ran, in reverse.
async fn run_stack(
    State(stack): State<Arc<Vec<Arc<dyn Middleware>>>>,
    req: Request,
    next: Next,
) -> Response {
    // `Option` so the request can be moved into each `before_request` and
    // handed back, without the borrow checker tripping on the short-
    // circuit (`Err`) path where it isn't returned.
    let mut req_opt = Some(req);
    let mut ran = 0usize;
    let mut short_circuit: Option<Response> = None;

    for mw in stack.iter() {
        let req = req_opt
            .take()
            .expect("request present for each before hook");
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
    }

    let mut res = match short_circuit {
        Some(resp) => resp,
        None => {
            next.run(
                req_opt
                    .take()
                    .expect("request present when not short-circuited"),
            )
            .await
        }
    };

    // Only the middleware whose `before_request` ran get an
    // `after_response`, in reverse (onion unwind).
    for mw in stack.iter().take(ran).rev() {
        res = mw.after_response(res).await;
    }
    res
}

// =========================================================================
// Middleware registry (feature #68 introspection; task #323).
//
// A snapshot of the *typed* middleware active in the app, grouped by the
// plugin that contributed it, published once at `App::build()` time. It is
// the middleware analog of `crate::routes::RouteRegistry`: the same
// `OnceLock` + `init` / `get` lifecycle, the same per-plugin `BTreeMap`
// grouping, and the same "declared snapshot, not a live table" drift caveat.
//
// ## What it can and cannot see
//
// Two things contribute request/response behaviour to the router, and only
// one of them is enumerable:
//
//   1. Typed `Middleware` impls — from `AppBuilder::middleware` (recorded
//      under the implicit `"app"` plugin) and `Plugin::middleware` (recorded
//      under the plugin's name). These are ENUMERABLE: the registry knows
//      each one's `name()` and `order()` and can replay the effective chain
//      via `MiddlewareRegistry::effective_order`.
//
//   2. Raw `Plugin::wrap_router` layers. These are OPAQUE. `wrap_router`
//      hands the plugin an `axum::Router` and takes one back; the framework
//      cannot tell whether the plugin added zero layers or ten, nor name
//      what it added — axum exposes no route-table / layer introspection
//      API. This is the exact analog of `Routes::with_router` in the route
//      registry: a genuine contribution the snapshot cannot enumerate.
//
// The registry therefore records ONLY typed middleware. A plugin that
// contributes solely `wrap_router` layers still appears (with an empty
// per-plugin list — "present, but no *enumerable* middleware"); the empty
// list does NOT prove the plugin adds no behaviour.
// =========================================================================

/// Where a recorded middleware entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum MiddlewareSource {
    /// A typed [`Middleware`] impl — fully enumerable (name + order known).
    Typed,
    /// A raw [`Plugin::wrap_router`](crate::plugin::Plugin::wrap_router)
    /// layer contribution. OPAQUE: the framework can model THAT a plugin
    /// contributes such layers but cannot enumerate or name them (there is
    /// no axum route-table introspection API). Mirrors `Routes::with_router`
    /// in the route registry.
    ///
    /// `App::build` never emits this variant automatically, because it
    /// cannot reliably detect a `wrap_router` override: the default impl
    /// returns the router unchanged, and a trait object gives no way to
    /// compare a plugin's method against that default. The variant exists
    /// so callers that DO know a contribution is opaque can record it by
    /// hand (via [`MiddlewareSpec::wrap_router`]) without pretending it is
    /// an enumerable typed middleware.
    WrapRouter,
}

/// One recorded middleware entry: a short name, its declarative `order`
/// (lower = outer — see [`Middleware::order`]), and where it came from.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MiddlewareSpec {
    pub name: String,
    pub order: i32,
    pub source: MiddlewareSource,
}

impl MiddlewareSpec {
    /// A spec for a typed [`Middleware`] — the enumerable case. `name` is
    /// typically the middleware's [`Middleware::name`] and `order` its
    /// [`Middleware::order`].
    pub fn typed(name: impl Into<String>, order: i32) -> Self {
        Self {
            name: name.into(),
            order,
            source: MiddlewareSource::Typed,
        }
    }

    /// A marker spec for an opaque `wrap_router` contribution. `order` is
    /// meaningless for these (raw tower layers order by application, not by
    /// [`Middleware::order`]) so it is recorded as `0`. See
    /// [`MiddlewareSource::WrapRouter`] for why `App::build` does not emit
    /// these automatically.
    pub fn wrap_router(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            order: 0,
            source: MiddlewareSource::WrapRouter,
        }
    }
}

/// Snapshot of the typed middleware active in the app, keyed by plugin name.
/// The implicit `"app"` plugin holds `AppBuilder::middleware` registrations;
/// each registered plugin holds its `Plugin::middleware` contributions.
/// Iteration order is alphabetical by plugin name (`BTreeMap`), matching
/// [`crate::routes::RouteRegistry`].
///
/// An entry with an empty list means the plugin contributes no *typed*
/// middleware; it may still add opaque `wrap_router` layers (see the module
/// docs). A plugin that was never registered is simply absent.
#[derive(Debug, Clone, Default)]
pub struct MiddlewareRegistry {
    pub by_plugin: BTreeMap<String, Vec<MiddlewareSpec>>,
    /// Typed middleware in REGISTRATION order (app-level first, then plugins
    /// in topological dependency order, each plugin's `middleware()` in its
    /// returned order) — the exact sequence [`MiddlewareStack`] receives
    /// before it stable-sorts by `order`. Kept private so [`Self::effective_order`]
    /// can reproduce the real chain, including how equal-`order` entries tie
    /// (by registration, not alphabetically by plugin name).
    registration_order: Vec<MiddlewareSpec>,
}

impl MiddlewareRegistry {
    /// Total number of recorded (typed) middleware across every plugin.
    pub fn total(&self) -> usize {
        self.by_plugin.values().map(|v| v.len()).sum()
    }

    /// Record a plugin's typed-middleware contribution. Called once per
    /// plugin at build time (app-level first, then each plugin in topological
    /// order) so registration order is preserved for [`Self::effective_order`].
    /// An empty `specs` still creates the per-plugin entry, mirroring the
    /// route registry's "present but empty" convention.
    pub fn record_plugin<I>(&mut self, plugin: &str, specs: I)
    where
        I: IntoIterator<Item = MiddlewareSpec>,
    {
        let specs: Vec<MiddlewareSpec> = specs.into_iter().collect();
        self.registration_order.extend(specs.iter().cloned());
        self.by_plugin
            .entry(plugin.to_string())
            .or_default()
            .extend(specs);
    }

    /// The typed middleware in EFFECTIVE chain order — stable-sorted by
    /// `order` (lower = outer, runs `before_request` first), exactly as
    /// [`MiddlewareStack::apply`] installs them. Equal-`order` entries keep
    /// registration order (app-level before plugins, plugins in dependency
    /// order). This is the order the pipeline actually runs, which the
    /// alphabetical `by_plugin` grouping alone does not reveal.
    ///
    /// Only typed middleware appear here; opaque `wrap_router` layers cannot
    /// be placed in this sequence (see the module docs).
    pub fn effective_order(&self) -> Vec<&MiddlewareSpec> {
        let mut ordered: Vec<&MiddlewareSpec> = self.registration_order.iter().collect();
        ordered.sort_by_key(|spec| spec.order);
        ordered
    }
}

static REGISTRY: OnceLock<MiddlewareRegistry> = OnceLock::new();

/// Publish the registry. Called from `App::build()` after the middleware
/// stack has been assembled. Safe to call exactly once; subsequent calls are
/// no-ops (mirrors [`crate::routes::init`]).
pub fn init(registry: MiddlewareRegistry) {
    let _ = REGISTRY.set(registry);
}

/// Read the registry. Returns `None` if `init` hasn't been called (binaries
/// that bypass `App::build()`, tests that short-circuit the build flow).
/// Callers should treat `None` as "no middleware to surface" rather than as
/// an error (mirrors [`crate::routes::get`]).
pub fn get() -> Option<&'static MiddlewareRegistry> {
    REGISTRY.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_counts_typed_middleware_and_keeps_empty_entries() {
        let mut reg = MiddlewareRegistry::default();
        reg.record_plugin(
            "app",
            [
                MiddlewareSpec::typed("a-outer", -100),
                MiddlewareSpec::typed("a-inner", 50),
            ],
        );
        reg.record_plugin("plug", [MiddlewareSpec::typed("plug-mw", 0)]);
        // A plugin present in the registry but contributing no typed
        // middleware (e.g. wrap_router-only) keeps an empty entry.
        reg.record_plugin("silent", std::iter::empty());

        assert_eq!(reg.total(), 3);
        assert!(reg.by_plugin.contains_key("silent"));
        assert!(reg.by_plugin["silent"].is_empty());
    }

    #[test]
    fn effective_order_sorts_by_order_stable_on_ties() {
        let mut reg = MiddlewareRegistry::default();
        // Registration order: a-outer(-100), a-inner(50) [app], then
        // plug-mw(0) [plugin]. Effective order sorts by `order`: -100, 0, 50.
        reg.record_plugin(
            "app",
            [
                MiddlewareSpec::typed("a-outer", -100),
                MiddlewareSpec::typed("a-inner", 50),
            ],
        );
        reg.record_plugin("plug", [MiddlewareSpec::typed("plug-mw", 0)]);

        let effective: Vec<&str> = reg
            .effective_order()
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(effective, vec!["a-outer", "plug-mw", "a-inner"]);
    }

    #[test]
    fn effective_order_ties_keep_registration_order() {
        let mut reg = MiddlewareRegistry::default();
        reg.record_plugin(
            "app",
            [
                MiddlewareSpec::typed("first", 0),
                MiddlewareSpec::typed("second", 0),
            ],
        );
        reg.record_plugin("plug", [MiddlewareSpec::typed("third", 0)]);

        let effective: Vec<&str> = reg
            .effective_order()
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(effective, vec!["first", "second", "third"]);
    }
}
