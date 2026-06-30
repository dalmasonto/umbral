//! Per-model permission checks for the admin handlers.
//!
//! Feature #75. Bridges `umbral-permissions::has_perm_for_superuser`
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
use umbral::web::{IntoResponse, Response, StatusCode};
use umbral_auth::AuthUser;

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
///
/// Returns `false` (not installed → allow) when the model registry
/// hasn't been initialised yet, which happens in unit tests that never
/// call `App::build()`.
pub(crate) fn permissions_installed() -> bool {
    if !umbral::migrate::is_initialised() {
        return false;
    }
    umbral::migrate::registered_plugins()
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
    umbral_permissions::has_perm_for_superuser(&user_id, user.is_superuser, &perm)
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
        Err((StatusCode::FORBIDDEN, "umbral-admin: permission denied").into_response())
    }
}

/// Check an arbitrary permission codename directly (not the
/// `(plugin, table, action)` triple) — used by custom admin views, which
/// aren't model-bound. Returns `true` when permissions aren't installed
/// (staff-only baseline), the user is a superuser, or the user holds the
/// codename directly / via a group.
pub(crate) async fn has_codename(user: &AuthUser, codename: &str) -> bool {
    if !permissions_installed() {
        return true;
    }
    let user_id = user.id.to_string();
    umbral_permissions::has_perm_for_superuser(&user_id, user.is_superuser, codename)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(
                user_id = user_id.as_str(),
                perm = codename,
                error = %err,
                "codename permission check failed; denying by default"
            );
            false
        })
}

/// Handler-side guard for a raw codename. `Ok(())` when allowed, else a
/// 403 [`Response`]. Mirrors [`require`] for the model-bound path.
pub(crate) async fn require_codename(user: &AuthUser, codename: &str) -> Result<(), Response> {
    if has_codename(user, codename).await {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "umbral-admin: permission denied").into_response())
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
    /// Resolve all four flags from a pre-loaded codename set.
    ///
    /// Pure, no I/O — called by [`Self::load`] after the single
    /// `user_perms` query and tested directly in unit tests.
    fn from_codenames(
        codenames: &std::collections::HashSet<String>,
        plugin: &str,
        table: &str,
    ) -> Self {
        Self {
            can_view: codenames.contains(&codename(plugin, table, Action::View)),
            can_add: codenames.contains(&codename(plugin, table, Action::Add)),
            can_change: codenames.contains(&codename(plugin, table, Action::Change)),
            can_delete: codenames.contains(&codename(plugin, table, Action::Delete)),
        }
    }

    /// Probe all four standard actions for one (user, plugin, table)
    /// tuple. Loads the user's permission set once (one DB query) and
    /// resolves all flags in memory — no per-action queries.
    pub(crate) async fn load(user: &AuthUser, plugin: &str, table: &str) -> Self {
        if !permissions_installed() || user.is_superuser {
            return Self {
                can_view: true,
                can_add: true,
                can_change: true,
                can_delete: true,
            };
        }
        let user_id = user.id.to_string();
        let perms = match umbral_permissions::user_perms(&user_id).await {
            Ok(perms) => perms,
            Err(err) => {
                tracing::warn!(
                    user_id = user_id.as_str(),
                    error = %err,
                    "permission set load failed; denying admin model actions by default"
                );
                return Self {
                    can_view: false,
                    can_add: false,
                    can_change: false,
                    can_delete: false,
                };
            }
        };
        Self::from_codenames(&perms, plugin, table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn codename_follows_expected_shape() {
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

    // -----------------------------------------------------------------------
    // AdminPerms::from_codenames — pure flag resolution, no DB, no async.
    //
    // These tests confirm that the in-memory resolution layer (which is the
    // whole point of the one-query fix: load once, check in memory) maps
    // codenames to the correct boolean flags. The `load` function calls
    // `from_codenames` after the single `user_perms` query, so verifying
    // this function verifies the flag logic without any I/O.
    // -----------------------------------------------------------------------

    /// A user with only `view_<model>` in their codename set gets can_view=true
    /// and the other three flags false. Confirms per-flag granularity.
    #[test]
    fn from_codenames_view_only() {
        let codenames: HashSet<String> = ["blog.view_post".to_string()].into_iter().collect();
        let perms = AdminPerms::from_codenames(&codenames, "blog", "post");
        assert!(
            perms.can_view,
            "expected can_view=true with view_post codename"
        );
        assert!(!perms.can_add, "expected can_add=false without add_post");
        assert!(
            !perms.can_change,
            "expected can_change=false without change_post"
        );
        assert!(
            !perms.can_delete,
            "expected can_delete=false without delete_post"
        );
    }

    /// A user with change + delete but NOT view or add gets exactly those two.
    #[test]
    fn from_codenames_change_and_delete_subset() {
        let codenames: HashSet<String> = [
            "shop.change_product".to_string(),
            "shop.delete_product".to_string(),
        ]
        .into_iter()
        .collect();
        let perms = AdminPerms::from_codenames(&codenames, "shop", "product");
        assert!(!perms.can_view, "no view_product → can_view must be false");
        assert!(!perms.can_add, "no add_product → can_add must be false");
        assert!(
            perms.can_change,
            "change_product present → can_change must be true"
        );
        assert!(
            perms.can_delete,
            "delete_product present → can_delete must be true"
        );
    }

    /// A user with ALL four codenames gets all four flags true.
    #[test]
    fn from_codenames_full_set() {
        let codenames: HashSet<String> = [
            "blog.view_post".to_string(),
            "blog.add_post".to_string(),
            "blog.change_post".to_string(),
            "blog.delete_post".to_string(),
        ]
        .into_iter()
        .collect();
        let perms = AdminPerms::from_codenames(&codenames, "blog", "post");
        assert!(perms.can_view);
        assert!(perms.can_add);
        assert!(perms.can_change);
        assert!(perms.can_delete);
    }

    /// An empty codename set → all flags false.
    #[test]
    fn from_codenames_empty_set_denies_all() {
        let codenames: HashSet<String> = HashSet::new();
        let perms = AdminPerms::from_codenames(&codenames, "blog", "post");
        assert!(!perms.can_view);
        assert!(!perms.can_add);
        assert!(!perms.can_change);
        assert!(!perms.can_delete);
    }

    /// Codenames for a DIFFERENT model in the same plugin do NOT bleed into
    /// the checked model's flags. This guards against accidental prefix matches.
    #[test]
    fn from_codenames_does_not_bleed_across_models() {
        let codenames: HashSet<String> = [
            // `post` perms — should NOT affect `comment` flags
            "blog.view_post".to_string(),
            "blog.add_post".to_string(),
            "blog.change_post".to_string(),
            "blog.delete_post".to_string(),
        ]
        .into_iter()
        .collect();
        let perms = AdminPerms::from_codenames(&codenames, "blog", "comment");
        assert!(
            !perms.can_view,
            "post perm must not bleed into comment.can_view"
        );
        assert!(
            !perms.can_add,
            "post perm must not bleed into comment.can_add"
        );
        assert!(
            !perms.can_change,
            "post perm must not bleed into comment.can_change"
        );
        assert!(
            !perms.can_delete,
            "post perm must not bleed into comment.can_delete"
        );
    }

    /// Codenames from a different plugin don't grant access for the target plugin.
    #[test]
    fn from_codenames_does_not_bleed_across_plugins() {
        let codenames: HashSet<String> = [
            "other_plugin.view_post".to_string(),
            "other_plugin.add_post".to_string(),
        ]
        .into_iter()
        .collect();
        let perms = AdminPerms::from_codenames(&codenames, "blog", "post");
        assert!(
            !perms.can_view,
            "other plugin's perm must not grant blog.view_post"
        );
        assert!(
            !perms.can_add,
            "other plugin's perm must not grant blog.add_post"
        );
    }

    // has_codename / require_codename: when the permissions plugin is NOT
    // installed (the unit-test process), both must allow (staff-only baseline).
    #[tokio::test]
    async fn codename_checks_allow_when_permissions_absent() {
        use chrono::Utc;
        let user = umbral_auth::AuthUser {
            id: 1,
            username: "staff".to_string(),
            email: "staff@example.com".to_string(),
            password_hash: "!".to_string(),
            is_active: true,
            is_staff: true,
            is_superuser: false,
            date_joined: Utc::now(),
            last_login: None,
            email_verified_at: None,
        };
        assert!(
            super::has_codename(&user, "reports.view_sales").await,
            "absent permissions plugin → allow"
        );
        assert!(
            super::require_codename(&user, "reports.view_sales")
                .await
                .is_ok(),
            "require_codename Ok when allowed"
        );
    }
}
