//! Per-model permission checks for the admin handlers.
//!
//! Feature #75. Bridges `umbra-permissions::has_perm_for_superuser`
//! into the admin's handler + template surface so an editor without
//! `<plugin>.change_<model>` can't reach the edit form (via direct URL)
//! and never sees the Edit / Delete / Save buttons (via the template
//! ctx).
//!
//! **Graceful no-op when permissions aren't installed.** If
//! `PermissionsPlugin` is not registered with the framework, every
//! check returns `true` so the admin reverts to pre-#75 behaviour
//! (staff-only via `require_staff`). The opt-in is "install the
//! permissions plugin"; nothing in `AdminPlugin` needs flipping.
//!
//! Codename convention follows the permissions plugin's
//! `add_<model>` / `change_<model>` / `delete_<model>` / `view_<model>`
//! auto-creation, scoped by the plugin name that registered the model
//! (e.g. `"blog.change_post"`).

use serde::Serialize;
use umbra::web::{IntoResponse, Response, StatusCode};
use umbra_auth::AuthUser;

/// CRUD actions the admin enforces. Matches the four standard
/// permissions `PermissionsPlugin::on_ready` auto-creates per model.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Action {
    View,
    Add,
    Change,
    Delete,
}

impl Action {
    fn codename_verb(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Add => "add",
            Self::Change => "change",
            Self::Delete => "delete",
        }
    }
}

/// Compute the codename string the permissions plugin keys against —
/// `"<plugin>.<verb>_<table>"`. Lives here so the templating layer and
/// the handler layer agree on the exact key.
fn codename(plugin: &str, table: &str, action: Action) -> String {
    format!("{plugin}.{verb}_{table}", verb = action.codename_verb())
}

/// Returns `true` when the permissions plugin is registered with the
/// framework. Read once per request from the in-memory plugin list, so
/// the cost is a single Vec scan with no I/O.
fn permissions_installed() -> bool {
    umbra::migrate::registered_plugins()
        .iter()
        .any(|p| p == "permissions")
}

/// Run one permission check for `(plugin, table, action)`. Returns
/// `true` when:
///   - permissions plugin isn't installed (no-op fallback);
///   - the user is a superuser;
///   - the user has the codename either directly or via a group.
pub(crate) async fn check(user: &AuthUser, plugin: &str, table: &str, action: Action) -> bool {
    if !permissions_installed() {
        return true;
    }
    let perm = codename(plugin, table, action);
    let user_id = user.id.to_string();
    // A DB-layer error here would be the permissions tables missing or
    // a transient pool issue. Either way the safe behaviour is "deny" —
    // refusing a 403 over leaking write access to an unprivileged user.
    umbra_permissions::has_perm_for_superuser(&user_id, user.is_superuser, &perm)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(
                user_id = user_id.as_str(),
                perm = perm.as_str(),
                error = %err,
                "permission check failed; denying by default"
            );
            false
        })
}

/// Handler-side guard. Returns a 403 [`Response`] when the user lacks
/// the required permission, otherwise `Ok(())` so the caller can `?`
/// the result on a single line.
pub(crate) async fn require(
    user: &AuthUser,
    plugin: &str,
    table: &str,
    action: Action,
) -> Result<(), Response> {
    if check(user, plugin, table, action).await {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "umbra-admin: permission denied").into_response())
    }
}

/// Per-(user, model) permission bundle passed to templates. Serializes
/// as `{can_view, can_add, can_change, can_delete}` so template guards
/// stay declarative: `{% if perms.can_change %}…{% endif %}`.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct AdminPerms {
    pub can_view: bool,
    pub can_add: bool,
    pub can_change: bool,
    pub can_delete: bool,
}

impl AdminPerms {
    /// Probe all four standard actions for one (user, plugin, table)
    /// tuple. Four independent queries; small enough to avoid batching.
    pub(crate) async fn load(user: &AuthUser, plugin: &str, table: &str) -> Self {
        Self {
            can_view: check(user, plugin, table, Action::View).await,
            can_add: check(user, plugin, table, Action::Add).await,
            can_change: check(user, plugin, table, Action::Change).await,
            can_delete: check(user, plugin, table, Action::Delete).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codename_follows_django_shape() {
        assert_eq!(codename("blog", "post", Action::View), "blog.view_post");
        assert_eq!(codename("blog", "post", Action::Add), "blog.add_post");
        assert_eq!(codename("blog", "post", Action::Change), "blog.change_post");
        assert_eq!(codename("blog", "post", Action::Delete), "blog.delete_post");
    }

    #[test]
    fn codename_keeps_plugin_dot_table_separation() {
        // Plugin and table names can include underscores; the only
        // structural separators are the literal `.` and `_<table>`.
        assert_eq!(
            codename("user_mgmt", "auth_user", Action::Change),
            "user_mgmt.change_auth_user"
        );
    }
}
