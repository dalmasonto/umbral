//! Task #323 — the middleware introspection registry. Builds a REAL `App`
//! wiring two app-level typed middleware (distinct `order`s), a plugin that
//! contributes one typed middleware, and a plugin that only overrides
//! `wrap_router` (an opaque layer contribution), then proves
//! `umbral::middleware::get()` reflects them:
//!   * app-level middleware recorded under the implicit "app" plugin,
//!   * plugin-contributed middleware recorded under the plugin's name,
//!   * `effective_order()` replays the chain in sorted-by-`order` order
//!     (equal-`order` ties keep registration order),
//!   * a `wrap_router`-only plugin appears with an EMPTY typed-middleware
//!     list — the documented drift caveat: raw layers aren't enumerable.
//!
//! Settings init is one-shot per test binary, so there is exactly ONE
//! `App::build` here and every assertion reads the single published registry.

use std::sync::Arc;

use axum::extract::Request;
use umbral::async_trait;
use umbral::middleware::{Middleware, MiddlewareSource};
use umbral::plugin::Plugin;
use umbral::web::Router;

/// A pass-through middleware with an explicit name and order, so the registry
/// entry it produces is easy to assert on.
struct Named {
    label: &'static str,
    order: i32,
}

#[async_trait]
impl Middleware for Named {
    fn name(&self) -> &'static str {
        self.label
    }
    fn order(&self) -> i32 {
        self.order
    }
    // Both hooks default to pass-through; this test cares about registration,
    // not request/response behaviour (that lives in middleware_pipeline.rs).
}

/// Contributes one typed middleware and no routes.
struct MwPlugin;

impl Plugin for MwPlugin {
    fn name(&self) -> &'static str {
        "mwplug"
    }
    fn middleware(&self) -> Vec<Arc<dyn Middleware>> {
        vec![Arc::new(Named {
            label: "plug-mw",
            order: 0,
        })]
    }
}

/// Contributes NO typed middleware, only a raw `wrap_router` layer — the
/// opaque case. The registry can record THAT this plugin exists but cannot
/// enumerate what its layer does.
struct WrapPlugin;

impl Plugin for WrapPlugin {
    fn name(&self) -> &'static str {
        "wrapplug"
    }
    fn wrap_router(&self, router: Router) -> Router {
        // A genuine (if trivial) tower layer contribution: stamps a header on
        // every response. Invisible to the middleware registry by design.
        router.layer(axum::middleware::from_fn(
            |req: Request, next: axum::middleware::Next| async move {
                let mut res = next.run(req).await;
                res.headers_mut().insert("x-wrapplug", "1".parse().unwrap());
                res
            },
        ))
    }
}

#[tokio::test]
async fn registry_reflects_typed_middleware_by_plugin_and_effective_order() {
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    let _app = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        // App-level middleware, registration order: app-outer(-100) then
        // app-inner(40). Recorded under the implicit "app" plugin.
        .middleware(Named {
            label: "app-outer",
            order: -100,
        })
        .middleware(Named {
            label: "app-inner",
            order: 40,
        })
        .plugin(MwPlugin)
        .plugin(WrapPlugin)
        .build()
        .expect("App::build");

    let reg = umbral::middleware::get().expect("registry published by App::build");

    // --- per-plugin grouping ---
    let app_mw = reg.by_plugin.get("app").expect("app-level entry present");
    let app_names: Vec<&str> = app_mw.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        app_names,
        vec!["app-outer", "app-inner"],
        "app-level middleware recorded under the implicit app plugin, in registration order"
    );
    assert_eq!(app_mw[0].order, -100);
    assert_eq!(app_mw[1].order, 40);
    assert!(app_mw.iter().all(|s| s.source == MiddlewareSource::Typed));

    let plug_mw = reg
        .by_plugin
        .get("mwplug")
        .expect("plugin middleware entry present");
    assert_eq!(plug_mw.len(), 1);
    assert_eq!(plug_mw[0].name, "plug-mw");
    assert_eq!(plug_mw[0].order, 0);
    assert_eq!(plug_mw[0].source, MiddlewareSource::Typed);

    // --- opacity of wrap_router (the drift caveat) ---
    let wrap_mw = reg
        .by_plugin
        .get("wrapplug")
        .expect("wrap_router-only plugin still present in the registry");
    assert!(
        wrap_mw.is_empty(),
        "a wrap_router-only plugin contributes NO enumerable typed middleware; \
         its raw layer is opaque to the registry"
    );

    // --- totals ---
    assert_eq!(
        reg.total(),
        3,
        "three typed middleware (two app-level + one plugin); wrap_router layers don't count"
    );

    // --- effective (sorted-by-order) chain ---
    // Registration order was app-outer(-100), app-inner(40), plug-mw(0);
    // sorting by order (lower = outer) yields -100, 0, 40.
    let effective: Vec<&str> = reg
        .effective_order()
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        effective,
        vec!["app-outer", "plug-mw", "app-inner"],
        "effective_order replays the chain sorted by order(), like MiddlewareStack::apply"
    );
}
