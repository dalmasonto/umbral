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

use umbra::db::DbPool;

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
    /// Selected primary keys as raw strings (matches the model's actual PK
    /// type — i64, String, or Uuid — without forcing a parse to i64).
    pub ids: Vec<String>,
    /// Username of the currently-logged-in staff user.
    pub username: String,
    /// SQL table the action was invoked on.
    pub table: String,
    /// Ambient backend-aware pool — match on the `DbPool` variants
    /// (`Sqlite` / `Postgres`) for any escape-hatch raw SQL. New code
    /// should prefer the ORM (`Model::objects()` / `DynQuerySet`)
    /// instead of pulling the pool out at all.
    pub pool: DbPool,
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
    /// When `Some(codename)`, the action handler only runs if the acting
    /// user holds that codename (directly or via group), or is a superuser.
    /// No-op when `umbra-permissions` is not installed (gaps2 #79).
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

    /// Require a permission codename before this action can run (gaps2 #79).
    ///
    /// When set, the admin checks that the acting user holds `codename`
    /// (directly or via a group) before invoking the handler. Superusers
    /// bypass the check. If `umbra-permissions` is not installed, the check
    /// is a no-op and any staff user can run the action.
    ///
    /// `codename` should be the full composite key your permissions plugin
    /// uses, e.g. `"blog.publish_post"`.
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
                let Some((_, meta)) = crate::discovery::find_model(&inv.table) else {
                    return Err(format!("unknown table `{}`", inv.table));
                };
                let pk_name = crate::discovery::pk_column(&meta)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "id".to_string());
                match umbra::orm::DynQuerySet::for_meta(&meta)
                    .filter_in_strings(&pk_name, &inv.ids)
                    .delete()
                    .await
                {
                    Ok(deleted) => Ok(ActionResult::Toast {
                        message: format!("Deleted {deleted} row(s)."),
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

    /// Built-in "Restore selected" for soft-delete models (gaps2 #35).
    ///
    /// Clears `deleted_at` for the selected rows via
    /// [`DynQuerySet::restore`], moving them back out of the trash into
    /// the live changelist. Auto-injected for `soft_delete` models (see
    /// [`effective_actions`]); a non-soft-delete model never sees it.
    pub fn restore_selected() -> Self {
        Self::new(
            "restore_selected",
            "Restore selected",
            "archive-restore",
            |inv| async move {
                if inv.ids.is_empty() {
                    return Ok(ActionResult::Toast {
                        message: "No rows selected.".to_string(),
                        level: ToastLevel::Info,
                    });
                }
                let Some((_, meta)) = crate::discovery::find_model(&inv.table) else {
                    return Err(format!("unknown table `{}`", inv.table));
                };
                let pk_name = crate::discovery::pk_column(&meta)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "id".to_string());
                // `with_deleted()` so the PK filter can address the
                // trashed rows; `restore()` then clears `deleted_at`.
                match umbra::orm::DynQuerySet::for_meta(&meta)
                    .with_deleted()
                    .filter_in_strings(&pk_name, &inv.ids)
                    .restore()
                    .await
                {
                    Ok(restored) => Ok(ActionResult::Toast {
                        message: format!("Restored {restored} row(s)."),
                        level: ToastLevel::Success,
                    }),
                    Err(e) => {
                        tracing::error!(error = %e, "admin: restore_selected failed");
                        Err("database error during restore".to_string())
                    }
                }
            },
        )
        .scope(ActionScope::Bulk)
    }

    /// Built-in "Delete permanently" for soft-delete models (gaps2 #35).
    ///
    /// Issues a real `DELETE` via [`DynQuerySet::hard_delete`], bypassing
    /// the soft-delete stamp so the row leaves the table entirely (gone
    /// even from `with_deleted()`). Behind a confirm interstitial.
    /// Auto-injected for `soft_delete` models; a non-soft-delete model
    /// never sees it (its `delete_selected` already deletes for real).
    pub fn delete_permanently() -> Self {
        Self::new(
            "delete_permanently",
            "Delete permanently",
            "trash-2",
            |inv| async move {
                if inv.ids.is_empty() {
                    return Ok(ActionResult::Toast {
                        message: "No rows selected.".to_string(),
                        level: ToastLevel::Info,
                    });
                }
                let Some((_, meta)) = crate::discovery::find_model(&inv.table) else {
                    return Err(format!("unknown table `{}`", inv.table));
                };
                let pk_name = crate::discovery::pk_column(&meta)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "id".to_string());
                match umbra::orm::DynQuerySet::for_meta(&meta)
                    .hard_delete()
                    .with_deleted()
                    .filter_in_strings(&pk_name, &inv.ids)
                    .delete()
                    .await
                {
                    Ok(deleted) => Ok(ActionResult::Toast {
                        message: format!("Permanently deleted {deleted} row(s)."),
                        level: ToastLevel::Success,
                    }),
                    Err(e) => {
                        tracing::error!(error = %e, "admin: delete_permanently failed");
                        Err("database error during permanent delete".to_string())
                    }
                }
            },
        )
        .danger()
        .scope(ActionScope::Bulk)
        .confirm("This will PERMANENTLY delete the selected rows. They cannot be restored. Continue?")
    }

    /// The action key (URL-safe identifier).
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Compute the effective bulk-action set for a changelist render or
/// action dispatch (gaps2 #35).
///
/// For a soft-delete model, the admin auto-injects the trash workflow
/// actions on top of whatever the developer configured:
///   - In the LIVE view (`trash == false`): nothing extra — the
///     developer's `delete_selected` already soft-deletes (moves rows
///     to trash) because `DynQuerySet::delete` honours `soft_delete`.
///   - In the TRASH view (`trash == true`): the per-row edit/delete
///     affordances don't apply, so we surface **Restore selected** and
///     **Delete permanently** instead. The developer's own actions are
///     dropped in trash view to keep the action set unambiguous.
///
/// A non-soft-delete model returns its configured actions unchanged, so
/// existing installs see zero behavioural difference.
pub(crate) fn effective_actions(
    configured: &[Action],
    soft_delete: bool,
    trash: bool,
) -> Vec<Action> {
    if !soft_delete {
        return configured.to_vec();
    }
    if trash {
        vec![Action::restore_selected(), Action::delete_permanently()]
    } else {
        configured.to_vec()
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
    /// Optional per-column CSS widths rendered as `<col style="width: ...">`.
    /// Each entry is `(column_name, css_width)` e.g. `("title", "40%")`.
    pub(crate) column_widths: Vec<(String, String)>,
    /// When set, this column carries an argon2 password hash and should
    /// never be rendered as a plain input. The admin will:
    /// - Hide the column on edit forms (implicitly noform for the column).
    /// - Show a "Change password" button on the edit sheet that opens
    ///   a dedicated dialog.
    /// - On create forms, render a "Password" + "Confirm password" pair
    ///   that hashes the value on save.
    pub(crate) password_field: Option<String>,
}

/// Names of sensitive columns that are always read-only by default.
/// Any column whose name matches one of these patterns is added to
/// `readonly_fields` automatically even if not explicitly listed.
/// Pattern: exact match OR prefix match for `secret`.
pub(crate) fn is_sensitive_column(name: &str) -> bool {
    matches!(name, "password_hash" | "password" | "salt") || name.starts_with("secret")
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
            column_widths: Vec::new(),
            password_field: None,
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

    /// Set per-column CSS widths for the DataTable `<colgroup>`.
    ///
    /// Each entry is `(column_name, css_width)`.  The width is rendered as
    /// `<col style="width: {css_width}">` so you can use any valid CSS value:
    /// `"40%"`, `"120px"`, `"10rem"`, etc.
    ///
    /// # Example
    /// ```rust,ignore
    /// AdminModel::new("post")
    ///     .column_widths(&[("title", "40%"), ("author", "120px")])
    /// ```
    pub fn column_widths(mut self, widths: &[(&str, &str)]) -> Self {
        self.column_widths = widths
            .iter()
            .map(|(col, w)| (col.to_string(), w.to_string()))
            .collect();
        self
    }

    /// Return the merged readonly set: explicit `readonly_fields` plus any
    /// columns whose names match the built-in sensitive defaults
    /// (`password_hash`, `password`, `salt`, `secret*`).
    pub fn effective_readonly_fields<'a>(&'a self, all_columns: &[&'a str]) -> Vec<&'a str> {
        let mut set: std::collections::HashSet<&str> =
            self.readonly_fields.iter().map(|s| s.as_str()).collect();
        for col in all_columns {
            if is_sensitive_column(col) {
                set.insert(col);
            }
        }
        set.into_iter().collect()
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

    /// Mark `column` as carrying an argon2 password hash.
    ///
    /// The admin will never render this column as a plain text input.
    /// Instead:
    /// - Create forms receive a "Password" + "Confirm password" pair that
    ///   hashes the value before writing.
    /// - Edit forms show a "Change password" button that opens a dedicated
    ///   dialog (separate request).
    ///
    /// Set this on `AuthUser` or any model that carries a password column:
    ///
    /// ```ignore
    /// AdminModel::new("auth_user").password_field("password_hash")
    /// ```
    pub fn password_field(mut self, column: impl Into<String>) -> Self {
        self.password_field = Some(column.into());
        self
    }

    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn get_list_per_page(&self) -> usize {
        self.list_per_page
    }

    /// Expose `column_widths` as a slice for use in templates and tests.
    pub fn get_column_widths(&self) -> &[(String, String)] {
        &self.column_widths
    }
}

// =========================================================================
// Backwards-compat alias
// =========================================================================

pub type AdminConfig = AdminModel;
