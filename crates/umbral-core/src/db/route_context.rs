//! The request-scoped routing context: a `tokio::task_local!` value the
//! `DatabaseRouter` reads to make per-request (per-tenant) decisions. The
//! per-request twin of umbral's ambient-`OnceLock` pool pattern.

use std::future::Future;
use std::sync::Arc;

/// An opaque tenant identifier. Apps that don't do multitenancy never set it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TenantKey(String);

impl TenantKey {
    pub fn new(s: impl Into<String>) -> Self {
        TenantKey(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The request-scoped routing context. Carries the common-case tenant plus
/// an extensible typed store so any app/plugin can stash its own routing key.
#[derive(Clone, Default)]
pub struct RouteContext {
    tenant: Option<TenantKey>,
    /// Postgres session variables (GUCs) to set on the connection this request
    /// uses — e.g. `("app.user_id", "42")` for an RLS policy that reads
    /// `current_setting('app.user_id')`. The PG pool's `after_acquire` hook
    /// runs `set_config(name, value, false)` for each; `after_release` resets
    /// them, so a value can't leak to the next request on the same connection.
    session_vars: Vec<(String, String)>,
    extensions: http::Extensions,
}

impl RouteContext {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_tenant(mut self, tenant: TenantKey) -> Self {
        self.tenant = Some(tenant);
        self
    }
    pub fn tenant(&self) -> Option<&TenantKey> {
        self.tenant.as_ref()
    }
    /// Add a Postgres session variable (GUC) to apply on this request's DB
    /// connection. Builder form; see [`Self::session_vars`].
    pub fn with_session_var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.session_vars.push((name.into(), value.into()));
        self
    }
    /// Mutating form of [`Self::with_session_var`] (used by middleware that
    /// augments an already-scoped context).
    pub fn add_session_var(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.session_vars.push((name.into(), value.into()));
    }
    /// The Postgres session variables to set for this request.
    pub fn session_vars(&self) -> &[(String, String)] {
        &self.session_vars
    }
    /// Stash a typed routing value for a custom router to read back.
    pub fn insert<T: Clone + Send + Sync + 'static>(&mut self, value: T) {
        self.extensions.insert(value);
    }
    /// Read a typed routing value previously stashed via [`Self::insert`].
    pub fn get<T: Clone + Send + Sync + 'static>(&self) -> Option<&T> {
        self.extensions.get::<T>()
    }
}

tokio::task_local! {
    static ROUTE_CONTEXT: Arc<RouteContext>;
}

/// The current request's routing context. Returns a **default** context when
/// none is set — background `umbral-tasks` jobs, boot, CLI, and tests. The
/// router then falls back to the default DB / `public` schema; it never
/// silently inherits or guesses a tenant.
pub fn current() -> Arc<RouteContext> {
    ROUTE_CONTEXT
        .try_with(|c| c.clone())
        .unwrap_or_else(|_| Arc::new(RouteContext::default()))
}

/// Run `fut` with `ctx` as the ambient routing context. The explicit opt-in a
/// background job uses to run as a tenant.
pub async fn scope<F: Future>(ctx: RouteContext, fut: F) -> F::Output {
    ROUTE_CONTEXT.scope(Arc::new(ctx), fut).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn current_is_default_when_unset() {
        // No scope established: spawned-task / background fallback.
        assert!(current().tenant().is_none());
    }

    #[tokio::test]
    async fn scope_sets_and_restores_context() {
        let ctx = RouteContext::new().with_tenant(TenantKey::new("acme"));
        scope(ctx, async {
            assert_eq!(current().tenant().unwrap().as_str(), "acme");
        })
        .await;
        // Outside the scope, back to default.
        assert!(current().tenant().is_none());
    }

    #[tokio::test]
    async fn spawned_task_does_not_inherit_context() {
        let ctx = RouteContext::new().with_tenant(TenantKey::new("acme"));
        scope(ctx, async {
            // A freshly spawned task has NO ambient context (task-locals
            // don't cross spawn). This is the hard safety rule: no silent
            // tenant inheritance into background work.
            let handle = tokio::spawn(async { current().tenant().cloned() });
            assert!(handle.await.unwrap().is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn session_vars_round_trip_through_scope() {
        // audit_2 C2/R2: GUCs set on the context are visible inside the scope
        // (the PG pool's before_acquire hook reads them) and gone outside it.
        let ctx = RouteContext::new()
            .with_session_var("app.user_id", "42")
            .with_session_var("app.tenant_id", "acme");
        scope(ctx, async {
            let vars = current().session_vars().to_vec();
            assert_eq!(
                vars,
                vec![
                    ("app.user_id".to_string(), "42".to_string()),
                    ("app.tenant_id".to_string(), "acme".to_string()),
                ]
            );
        })
        .await;
        // Outside the scope: no session vars (nothing leaks to background work).
        assert!(current().session_vars().is_empty());
    }

    #[tokio::test]
    async fn extensions_store_typed_values() {
        #[derive(Clone, PartialEq, Debug)]
        struct Region(&'static str);
        let mut ctx = RouteContext::new();
        ctx.insert(Region("eu"));
        scope(ctx, async {
            assert_eq!(current().get::<Region>(), Some(&Region("eu")));
        })
        .await;
    }
}
