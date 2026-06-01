//! Per-table REST customization bundles.
//!
//! [`ResourceConfig`] groups every customization for ONE table
//! (`hide` / `transform` / `computed`) into a single value that any
//! module — a plugin crate, a free function, `main.rs` itself —
//! can build and hand to [`crate::RestPlugin::resource`].
//!
//! Why this matters: without this type the only way to customize REST
//! responses was the per-call builder chain on `RestPlugin` itself —
//! `.hide("user", "password_hash").transform("user", ...)`. Every
//! customization landed in `main.rs` because that's where
//! `RestPlugin` was constructed. With `ResourceConfig` the user
//! plugin (or whatever module owns the `User` model) can define its
//! REST shape next to the model:
//!
//! ```ignore
//! // plugins/users/src/lib.rs
//! pub fn rest_resource() -> umbra_rest::ResourceConfig {
//!     umbra_rest::ResourceConfig::new("user")
//!         .hide("password_hash")
//!         .transform("email", mask_email)
//!         .computed("display_name", display_name)
//! }
//!
//! // main.rs
//! RestPlugin::default()
//!     .resource(users::rest_resource())
//!     .resource(posts::rest_resource())
//! ```
//!
//! ## Composition with the per-call builders
//!
//! `ResourceConfig` doesn't *replace* the existing
//! `RestPlugin::hide` / `.transform` / `.computed` builders — it
//! complements them. Calls land in the same vecs internally, so
//! mixing the two is fine:
//!
//! ```ignore
//! RestPlugin::default()
//!     .resource(users::rest_resource())     // bundled user customization
//!     .hide("audit_log", "user_id")          // one-off case in main.rs
//! ```
//!
//! The DRF analog: each Django app has a `serializers.py` next to its
//! `models.py`. `ResourceConfig` plays the same role — the per-model
//! REST shape lives next to the model, not in the project's wiring
//! layer.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::permission::{Action, Permission};
use crate::{ComputedFn, TransformFn};

/// Bundled REST customization for one table. Build via
/// [`Self::new`] + chainable methods; register with
/// [`crate::RestPlugin::resource`].
///
/// Fields are public-ish via the constructor + builder methods
/// only — the closure-bearing vecs are kept private because the
/// `ComputedFn` / `TransformFn` types are internal implementation
/// detail.
pub struct ResourceConfig {
    pub(crate) table: String,
    pub(crate) hidden: Vec<String>,
    pub(crate) transforms: Vec<(String, TransformFn)>,
    pub(crate) computed: Vec<(String, ComputedFn)>,
    /// Permission class for this resource. `None` defaults to
    /// [`crate::permission::AllowAny`] at merge time.
    pub(crate) permission: Option<Arc<dyn Permission>>,
    /// Opt-in view scope. `None` means "all actions exposed" — the
    /// backward-compatible default. `Some(set)` restricts the
    /// resource to exactly that set; everything else 404s.
    pub(crate) view_scope: Option<HashSet<Action>>,
}

impl std::fmt::Debug for ResourceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceConfig")
            .field("table", &self.table)
            .field("hidden", &self.hidden)
            .field("transforms_count", &self.transforms.len())
            .field("computed_count", &self.computed.len())
            .finish()
    }
}

impl ResourceConfig {
    /// Start a new resource config for the given table.
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            hidden: Vec::new(),
            transforms: Vec::new(),
            computed: Vec::new(),
            permission: None,
            view_scope: None,
        }
    }

    /// Attach a permission class to this resource. Every request to
    /// any action on this table will be authorised through the
    /// permission's `check(action, identity)` before the actual
    /// handler runs.
    ///
    /// Override examples:
    ///
    /// ```ignore
    /// // Only authenticated callers can do anything.
    /// ResourceConfig::new("post").permission(IsAuthenticated)
    ///
    /// // Public-read, staff-write (and only staff CRUD).
    /// ResourceConfig::new("post").permission(OrPermission::new(vec![
    ///     Box::new(ReadOnly),
    ///     Box::new(IsStaff),
    /// ]))
    /// ```
    pub fn permission<P: Permission>(mut self, perm: P) -> Self {
        self.permission = Some(Arc::new(perm));
        self
    }

    /// Restrict this resource to a specific set of REST actions —
    /// the opt-in alternative to having every model expose all five
    /// (`List` / `Retrieve` / `Create` / `Update` / `Delete`). Any
    /// action not in the set returns 404 from the handler.
    ///
    /// Default (no call) is "every action exposed" so existing
    /// resources don't change shape on upgrade.
    ///
    /// ```ignore
    /// // Read-only public catalogue: no create/update/delete endpoints
    /// // even mount.
    /// ResourceConfig::new("product").views([Action::List, Action::Retrieve])
    /// ```
    pub fn views<I: IntoIterator<Item = Action>>(mut self, actions: I) -> Self {
        self.view_scope = Some(actions.into_iter().collect());
        self
    }

    /// The table this config is for. Used by [`crate::RestPlugin::
    /// resource`] when folding into the plugin's per-table vecs.
    pub fn table(&self) -> &str {
        &self.table
    }

    /// Strip a field from every REST response for this table.
    /// Equivalent to [`crate::RestPlugin::hide`] but with the table
    /// implicit. The column stays writable and ORM-readable; only
    /// the outbound JSON shape changes.
    pub fn hide(mut self, field: &str) -> Self {
        self.hidden.push(field.to_string());
        self
    }

    /// Replace a field's value in every REST response for this table.
    /// Equivalent to [`crate::RestPlugin::transform`] with the table
    /// implicit.
    pub fn transform<F>(mut self, field: &str, f: F) -> Self
    where
        F: Fn(&Value) -> Value + Send + Sync + 'static,
    {
        self.transforms
            .push((field.to_string(), std::sync::Arc::new(f)));
        self
    }

    /// Add a derived field to every REST response for this table.
    /// Equivalent to [`crate::RestPlugin::computed`] with the table
    /// implicit.
    pub fn computed<F>(mut self, name: &str, f: F) -> Self
    where
        F: Fn(&Map<String, Value>) -> Value + Send + Sync + 'static,
    {
        self.computed
            .push((name.to_string(), std::sync::Arc::new(f)));
        self
    }
}
