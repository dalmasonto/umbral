//! Per-model admin customization bundles.
//!
//! [`AdminConfig`] is the admin's equivalent of `umbra_rest::ResourceConfig`.
//! One config per registered model. Build via [`AdminConfig::new`] + chainable
//! methods, then register with [`crate::AdminPlugin::register`]:
//!
//! ```ignore
//! use umbra_admin::{AdminPlugin, AdminConfig, Action};
//!
//! AdminPlugin::default()
//!     .register(
//!         AdminConfig::new("post")
//!             .list_display(&["title", "author", "published_at"])
//!             .list_filter(&["published", "author"])
//!             .search_fields(&["title", "body"])
//!             .ordering(&["-published_at", "title"])
//!             .readonly_fields(&["created_at", "id"])
//!             .actions(vec![Action::delete_selected()]),
//!     )
//! ```
//!
//! ## Fields that are not called out here
//!
//! - **`fieldsets`** — grouping fields into sections on the edit form. Low ROI,
//!   deferred. When it lands, add `.fieldsets(vec![...])` to `AdminConfig`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// =========================================================================
// Action
// =========================================================================

/// Context available to bulk action handlers.
#[derive(Debug, Clone)]
pub struct AdminContext {
    /// Username of the currently logged-in staff user.
    pub username: String,
    /// The SQL table the action was invoked on.
    pub table: String,
}

/// The boxed future every bulk action handler collapses to.
pub(crate) type ActionFuture =
    Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'static>>;

/// The stored handler closure. Arc'd so the config can be cloned cheaply.
pub(crate) type ActionHandlerFn =
    Arc<dyn Fn(Vec<i64>, AdminContext) -> ActionFuture + Send + Sync + 'static>;

/// A bulk admin action, e.g. "publish selected", "delete selected".
///
/// Each action has a URL-safe name displayed in the action dropdown and an
/// async handler that receives the selected primary keys (as `Vec<i64>`) plus
/// an [`AdminContext`] carrying the logged-in user and table name. The handler
/// returns a flash message on success or an error string on failure.
///
/// Use [`Action::new`] to define a custom action. Use
/// [`Action::delete_selected`] for the built-in bulk-delete.
#[derive(Clone)]
pub struct Action {
    pub(crate) name: String,
    pub(crate) label: String,
    pub(crate) handler: ActionHandlerFn,
}

impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Action")
            .field("name", &self.name)
            .field("label", &self.label)
            .finish()
    }
}

impl Action {
    /// Create a new bulk action.
    ///
    /// `name` must be ASCII lowercase/digits/underscores/hyphens (validated
    /// at construction; panics on an invalid name — it is always a bug).
    ///
    /// `handler` receives the selected PKs and context, returns a flash message
    /// string (`Ok`) or an error string (`Err`).
    ///
    /// ```ignore
    /// Action::new("publish", |ids, ctx| async move {
    ///     // ids: the selected primary keys
    ///     // ctx: AdminContext { username, table }
    ///     Ok(format!("Published {} rows.", ids.len()))
    /// })
    /// ```
    pub fn new<F, Fut>(name: impl Into<String>, label: impl Into<String>, f: F) -> Self
    where
        F: Fn(Vec<i64>, AdminContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        let name = name.into();
        let label = label.into();
        assert!(
            !name.is_empty() && name.chars().all(is_action_name_char),
            "Action::new: name {name:?} must be ASCII [a-z0-9_-]"
        );
        let handler: ActionHandlerFn = Arc::new(move |ids, ctx| Box::pin(f(ids, ctx)));
        Self {
            name,
            label,
            handler,
        }
    }

    /// The built-in bulk-delete action. Equivalent to Django's
    /// "Delete selected <model>" action that appears by default on every
    /// model's change list.
    ///
    /// Issues a single `DELETE FROM "<table>" WHERE "id" IN (...)` SQL
    /// statement for the selected PKs. Returns a flash message with the count.
    pub fn delete_selected() -> Self {
        Self::new(
            "delete_selected",
            "Delete selected",
            |ids, ctx| async move {
                if ids.is_empty() {
                    return Ok("No rows selected.".to_string());
                }
                let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                let sql = format!(
                    "DELETE FROM \"{}\" WHERE \"id\" IN ({placeholders})",
                    ctx.table.replace('"', "\"\"")
                );
                let pool = umbra::db::pool();
                let mut q = sqlx::query(&sql);
                for id in &ids {
                    q = q.bind(*id);
                }
                match q.execute(&pool).await {
                    Ok(r) => Ok(format!("Deleted {} row(s).", r.rows_affected())),
                    Err(e) => {
                        tracing::error!(error = %e, "admin: delete_selected failed");
                        Err("database error during delete".to_string())
                    }
                }
            },
        )
    }
}

fn is_action_name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
}

// =========================================================================
// AdminConfig
// =========================================================================

/// Per-model admin customization. Build via [`Self::new`] + chainable methods,
/// then register with [`crate::AdminPlugin::register`].
///
/// All methods are opt-in. An `AdminConfig` with only `.new("post")` called
/// behaves identically to the implicit default: all columns in the list,
/// no filters, no search, DB-default ordering.
#[derive(Clone, Debug)]
pub struct AdminConfig {
    /// SQL table name this config applies to.
    pub(crate) table: String,
    /// Columns to show in the list view (in order). Empty = all columns.
    pub(crate) list_display: Vec<String>,
    /// Columns that produce filter facets in the list sidebar. Empty = none.
    pub(crate) list_filter: Vec<String>,
    /// Columns that are searched by the search box via LIKE %term%. Empty = search disabled.
    pub(crate) search_fields: Vec<String>,
    /// Default sort columns. Leading `-` means descending. Empty = ORDER BY pk.
    pub(crate) ordering: Vec<String>,
    /// Bulk actions available on the list view.
    pub(crate) actions: Vec<Action>,
    /// Fields shown read-only on the edit/create form.
    pub(crate) readonly_fields: Vec<String>,
}

impl AdminConfig {
    /// Start a new `AdminConfig` for the given SQL table name.
    ///
    /// ```ignore
    /// AdminConfig::new("post")
    /// ```
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            list_display: Vec::new(),
            list_filter: Vec::new(),
            search_fields: Vec::new(),
            ordering: Vec::new(),
            actions: Vec::new(),
            readonly_fields: Vec::new(),
        }
    }

    /// Pick which columns appear in the list view, in this exact order.
    ///
    /// Default (no call): all columns in declaration order.
    ///
    /// ```ignore
    /// AdminConfig::new("post")
    ///     .list_display(&["title", "author", "published_at"])
    /// ```
    pub fn list_display(mut self, fields: &[&str]) -> Self {
        self.list_display = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Name the columns that become filter facets in the list sidebar.
    ///
    /// Each named field becomes a clickable facet whose values are the
    /// distinct non-null values in that column (booleans render as
    /// "true / false"; integers and text render their distinct values).
    ///
    /// ```ignore
    /// AdminConfig::new("post")
    ///     .list_filter(&["published", "author"])
    /// ```
    pub fn list_filter(mut self, fields: &[&str]) -> Self {
        self.list_filter = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Enable a search box that ANDs `LIKE %term%` across these columns.
    ///
    /// When the `?q=` query parameter is present on the list URL, the
    /// handler adds a `WHERE (col1 LIKE ? OR col2 LIKE ?)` clause to the
    /// base query before pagination.
    ///
    /// ```ignore
    /// AdminConfig::new("post")
    ///     .search_fields(&["title", "body"])
    /// ```
    pub fn search_fields(mut self, fields: &[&str]) -> Self {
        self.search_fields = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Set the default sort order for the list view.
    ///
    /// Each entry is a column name optionally prefixed with `-` for descending.
    ///
    /// ```ignore
    /// AdminConfig::new("post")
    ///     .ordering(&["-published_at", "title"])
    /// ```
    pub fn ordering(mut self, fields: &[&str]) -> Self {
        self.ordering = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Register bulk actions available via the action dropdown on the list view.
    ///
    /// The built-in [`Action::delete_selected`] is always available as a
    /// convenience; call this method to add custom actions *in addition to*
    /// whatever the default list produces (or replace the defaults entirely by
    /// passing only the actions you want).
    ///
    /// ```ignore
    /// AdminConfig::new("post")
    ///     .actions(vec![
    ///         Action::delete_selected(),
    ///         Action::new("publish", "Publish selected", |ids, _ctx| async move {
    ///             Ok(format!("Published {} posts.", ids.len()))
    ///         }),
    ///     ])
    /// ```
    pub fn actions(mut self, actions: Vec<Action>) -> Self {
        self.actions = actions;
        self
    }

    /// Mark fields as read-only on the create/edit form.
    ///
    /// These fields are still shown on the form (so the operator can see
    /// the value) but render as `<input readonly>` so they cannot be
    /// changed by the browser submission.
    ///
    /// ```ignore
    /// AdminConfig::new("post")
    ///     .readonly_fields(&["created_at", "id"])
    /// ```
    pub fn readonly_fields(mut self, fields: &[&str]) -> Self {
        self.readonly_fields = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    /// The table this config applies to.
    pub fn table(&self) -> &str {
        &self.table
    }
}
