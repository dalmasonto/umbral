//! Per-model admin customization bundles.
//!
//! [`AdminModel`] is the admin's equivalent of `umbra_rest::ResourceConfig`.
//! One config per registered model. Build via [`AdminModel::new`] + chainable
//! methods, then register with [`crate::AdminPlugin::register`]:
//!
//! ```ignore
//! use umbra_admin::{AdminPlugin, AdminModel, Action};
//!
//! AdminPlugin::default()
//!     .register(
//!         AdminModel::new("post")
//!             .list_display(&["title", "author", "published_at"])
//!             .list_filter(&["published", "author"])
//!             .search_fields(&["title", "body"])
//!             .ordering(&["-published_at", "title"])
//!             .readonly_fields(&["created_at", "id"])
//!             .actions(vec![Action::delete_selected()]),
//!     )
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sqlx::SqlitePool;

// =========================================================================
// Action result / invocation types
// =========================================================================

/// Severity level for toast notifications.
#[derive(Debug, Clone)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToastLevel::Info => "info",
            ToastLevel::Success => "success",
            ToastLevel::Warning => "warning",
            ToastLevel::Error => "error",
        }
    }
}

/// The result an action handler returns to the admin runtime.
///
/// The runtime encodes each variant as HTMX response directives:
/// - `Toast` → `HX-Trigger: {"showToast": {...}}`
/// - `RefreshTable` → rows fragment swap
/// - `OpenSheet` → `HX-Trigger: {"openSheet": {...}}`
/// - `Download` → `Content-Disposition: attachment` bytes
/// - `Redirect` → `HX-Redirect` header
#[derive(Debug, Clone)]
pub enum ActionResult {
    Toast {
        message: String,
        level: ToastLevel,
    },
    RefreshTable,
    OpenSheet {
        table: String,
        id: i64,
    },
    Download {
        filename: String,
        content_type: String,
        bytes: Vec<u8>,
    },
    Redirect {
        url: String,
    },
}

/// Visual variant for an action button.
#[derive(Debug, Clone)]
pub enum ActionVariant {
    Default,
    Danger,
}

/// Which surfaces an action appears on.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionScope {
    Row,
    Bulk,
    Both,
}

/// Context available to action handlers.
#[derive(Debug, Clone)]
pub struct ActionInvocation {
    /// Selected primary keys.
    pub ids: Vec<i64>,
    /// Username of the currently-logged-in staff user.
    pub username: String,
    /// SQL table the action was invoked on.
    pub table: String,
    /// Ambient pool for DB mutations.
    pub pool: SqlitePool,
}

/// Backwards-compatible context type used by phase 1/2 code paths.
#[derive(Debug, Clone)]
pub struct AdminContext {
    pub username: String,
    pub table: String,
}

pub(crate) type ActionFuture =
    Pin<Box<dyn Future<Output = Result<ActionResult, String>> + Send + 'static>>;

pub(crate) type ActionHandlerFn =
    Arc<dyn Fn(ActionInvocation) -> ActionFuture + Send + Sync + 'static>;

/// A row or bulk admin action.
///
/// Build with [`Action::new`]; chain `.danger()`, `.scope()`, `.confirm()`,
/// `.permission()` to configure. Use [`Action::delete_selected`] for the
/// built-in bulk-delete.
#[derive(Clone)]
pub struct Action {
    pub(crate) key: String,
    /// Display label shown in tooltips / overflow menus.
    pub(crate) label: String,
    /// Lucide icon name (e.g. "send", "trash-2").
    pub(crate) icon: String,
    pub(crate) variant: ActionVariant,
    pub(crate) scope: ActionScope,
    /// If `Some`, a confirm dialog is shown before firing.
    pub(crate) confirm: Option<String>,
    /// Permission codename to check. `None` = any staff user may invoke.
    /// Full umbra-permissions integration deferred (gap 33); today gated
    /// on `is_staff` only. Field is stored for when permissions land.
    pub(crate) permission: Option<String>,
    pub(crate) handler: ActionHandlerFn,
}

impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Action")
            .field("key", &self.key)
            .field("label", &self.label)
            .field("icon", &self.icon)
            .finish()
    }
}

impl Action {
    /// Create a new action.
    ///
    /// `key` must be ASCII lowercase/digits/underscores/hyphens.
    pub fn new<F, Fut>(
        key: impl Into<String>,
        label: impl Into<String>,
        icon: impl Into<String>,
        f: F,
    ) -> Self
    where
        F: Fn(ActionInvocation) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ActionResult, String>> + Send + 'static,
    {
        let key = key.into();
        assert!(
            !key.is_empty() && key.chars().all(is_action_key_char),
            "Action::new: key {key:?} must be ASCII [a-z0-9_-]"
        );
        Action {
            key,
            label: label.into(),
            icon: icon.into(),
            variant: ActionVariant::Default,
            scope: ActionScope::Both,
            confirm: None,
            permission: None,
            handler: Arc::new(move |inv| Box::pin(f(inv))),
        }
    }

    /// Mark this action as danger variant (red styling).
    pub fn danger(mut self) -> Self {
        self.variant = ActionVariant::Danger;
        self
    }

    /// Restrict this action to row-only or bulk-only scope.
    pub fn scope(mut self, scope: ActionScope) -> Self {
        self.scope = scope;
        self
    }

    /// Require a confirm dialog before firing. `message` is shown in the dialog.
    pub fn confirm(mut self, message: impl Into<String>) -> Self {
        self.confirm = Some(message.into());
        self
    }

    /// Require a permission codename (deferred; stored for future use).
    pub fn permission(mut self, codename: impl Into<String>) -> Self {
        self.permission = Some(codename.into());
        self
    }

    /// Built-in bulk-delete. Equivalent to Django's "Delete selected" default.
    pub fn delete_selected() -> Self {
        Self::new(
            "delete_selected",
            "Delete selected",
            "trash-2",
            |inv| async move {
                if inv.ids.is_empty() {
                    return Ok(ActionResult::Toast {
                        message: "No rows selected.".to_string(),
                        level: ToastLevel::Info,
                    });
                }
                let placeholders = inv.ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                let sql = format!(
                    "DELETE FROM \"{}\" WHERE \"id\" IN ({placeholders})",
                    inv.table.replace('"', "\"\"")
                );
                let mut q = sqlx::query(&sql);
                for id in &inv.ids {
                    q = q.bind(*id);
                }
                match q.execute(&inv.pool).await {
                    Ok(r) => Ok(ActionResult::Toast {
                        message: format!("Deleted {} row(s).", r.rows_affected()),
                        level: ToastLevel::Success,
                    }),
                    Err(e) => {
                        tracing::error!(error = %e, "admin: delete_selected failed");
                        Err("database error during delete".to_string())
                    }
                }
            },
        )
        .danger()
        .scope(ActionScope::Bulk)
        .confirm("This will permanently delete the selected rows. Continue?")
    }

    /// The action key (URL-safe identifier).
    pub fn key(&self) -> &str {
        &self.key
    }
}

fn is_action_key_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
}

// =========================================================================
// InlineModel (phase 2 stub)
// =========================================================================

/// Data shape for a related-model inline editor.
#[derive(Debug, Clone)]
pub struct InlineModel {
    pub model: String,
    pub fk_field: String,
    pub list_display: Vec<String>,
}

// =========================================================================
// AdminModel
// =========================================================================

/// Per-model admin customization. Build via [`Self::new`] + chainable methods.
#[derive(Clone, Debug)]
pub struct AdminModel {
    pub(crate) table: String,
    pub(crate) list_display: Vec<String>,
    pub(crate) list_filter: Vec<String>,
    pub(crate) search_fields: Vec<String>,
    pub(crate) ordering: Vec<String>,
    pub(crate) actions: Vec<Action>,
    pub(crate) readonly_fields: Vec<String>,
    pub(crate) list_per_page: usize,
    pub(crate) inlines: Vec<InlineModel>,
    pub(crate) label: Option<String>,
    pub(crate) icon: Option<String>,
    /// Fields that support double-click inline edit in the DataTable.
    pub(crate) inline_edit_fields: Vec<String>,
}

impl AdminModel {
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            list_display: Vec::new(),
            list_filter: Vec::new(),
            search_fields: Vec::new(),
            ordering: Vec::new(),
            actions: Vec::new(),
            readonly_fields: Vec::new(),
            list_per_page: 25,
            inlines: Vec::new(),
            label: None,
            icon: None,
            inline_edit_fields: Vec::new(),
        }
    }

    pub fn list_display(mut self, fields: &[&str]) -> Self {
        self.list_display = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn list_filter(mut self, fields: &[&str]) -> Self {
        self.list_filter = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn search_fields(mut self, fields: &[&str]) -> Self {
        self.search_fields = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn ordering(mut self, fields: &[&str]) -> Self {
        self.ordering = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn actions(mut self, actions: Vec<Action>) -> Self {
        self.actions = actions;
        self
    }

    pub fn readonly_fields(mut self, fields: &[&str]) -> Self {
        self.readonly_fields = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn list_per_page(mut self, n: usize) -> Self {
        self.list_per_page = n;
        self
    }

    pub fn inlines(mut self, inlines: Vec<InlineModel>) -> Self {
        self.inlines = inlines;
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Enable double-click inline cell edit for these columns in the DataTable.
    pub fn inline_edit_fields(mut self, fields: &[&str]) -> Self {
        self.inline_edit_fields = fields.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn get_list_per_page(&self) -> usize {
        self.list_per_page
    }
}

// =========================================================================
// Backwards-compat alias
// =========================================================================

pub type AdminConfig = AdminModel;
