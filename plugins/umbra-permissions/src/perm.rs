//! The `has_perm` / `user_perms` query layer.
//!
//! ## Design: free functions, not a trait method
//!
//! `has_perm` takes `user_id: i64` rather than a `&impl UserModel` receiver
//! so this crate does not depend on `umbra-auth`. If `has_perm` lived on the
//! `UserModel` trait in `umbra-auth`, we'd need `umbra-auth` → `umbra-permissions`
//! (to call the perm query) AND `umbra-permissions` → `umbra-auth` (to read
//! `UserModel`). That is a circular crate dependency Cargo refuses to compile.
//!
//! The free-function shape is also simpler at the call site:
//!
//! ```ignore
//! use umbra_permissions::has_perm;
//!
//! // In a handler, after extracting the logged-in user:
//! if !has_perm(user.id(), "blog.publish_post").await? {
//!     return Err(StatusCode::FORBIDDEN);
//! }
//! ```
//!
//! ## Superuser bypass
//!
//! `has_perm` does NOT read `is_superuser` from the DB — this module knows
//! nothing about the user table schema. The caller is responsible for the
//! superuser bypass. The typical shape:
//!
//! ```ignore
//! if user.is_superuser() || has_perm(user.id(), "blog.publish_post").await? {
//!     // allowed
//! }
//! ```
//!
//! `has_perm_for_superuser` is provided as a convenience that accepts an
//! `is_superuser: bool` flag and short-circuits immediately.

use std::collections::{HashMap, HashSet};

use crate::models::{
    ContentType, GroupPermission, Permission, UserGroup, UserPermission, content_type,
    group_permission, permission, user_group, user_permission,
};

/// Errors the perm helpers can produce.
#[derive(Debug)]
pub enum PermError {
    /// A sqlx-level error (connection failure, bad SQL, etc.).
    Sqlx(sqlx::Error),
}

impl std::fmt::Display for PermError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PermError::Sqlx(e) => write!(f, "umbra-permissions: sqlx: {e}"),
        }
    }
}

impl std::error::Error for PermError {}

impl From<sqlx::Error> for PermError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

/// Return `true` if the user with `user_id` holds the permission identified
/// by `"app_label.codename"` (e.g. `"blog.publish_post"`).
///
/// Checks both direct user permissions and group-mediated permissions in a
/// single query. Does NOT short-circuit on superuser — callers that want the
/// superuser bypass should call `has_perm_for_superuser` or check
/// `user.is_superuser()` themselves.
///
/// Returns `Ok(false)` when the perm string is malformed (no dot) or the
/// permission does not exist, rather than returning an error.
pub async fn has_perm(user_id: i64, perm: &str) -> Result<bool, PermError> {
    let Some((app_label, codename)) = perm.split_once('.') else {
        return Ok(false);
    };
    has_perm_scoped(user_id, app_label, codename).await
}

/// Like `has_perm` but takes the `app_label` and `codename` as separate
/// parameters. Prefer `has_perm` for call sites that already have the
/// `"app_label.codename"` string form.
pub async fn has_perm_scoped(
    user_id: i64,
    app_label: &str,
    codename: &str,
) -> Result<bool, PermError> {
    // The original SQL was one UNION-JOIN; we trade that for 3-4 ORM
    // calls. Each step filters by an indexed column and the result sets
    // stay small for typical RBAC tables, so the wall-clock cost is
    // negligible. The win is portability across backends and no raw
    // SQL hidden inside the plugin.

    // 1. content_type rows for this app_label (usually 1 per model).
    let ct_ids: Vec<i64> = ContentType::objects()
        .filter(content_type::APP_LABEL.eq(app_label))
        .fetch()
        .await?
        .into_iter()
        .map(|c| c.id)
        .collect();
    if ct_ids.is_empty() {
        return Ok(false);
    }

    // 2. permissions matching codename within those content_types.
    let perm_ids: Vec<i64> = Permission::objects()
        .filter(
            permission::CODENAME.eq(codename) & permission::CONTENT_TYPE_ID.in_(&ct_ids),
        )
        .fetch()
        .await?
        .into_iter()
        .map(|p| p.id)
        .collect();
    if perm_ids.is_empty() {
        return Ok(false);
    }

    // 3. Direct user-permission grant.
    let direct = UserPermission::objects()
        .filter(
            user_permission::USER_ID.eq(user_id)
                & user_permission::PERMISSION_ID.in_(&perm_ids),
        )
        .exists()
        .await?;
    if direct {
        return Ok(true);
    }

    // 4. Group-mediated grant: the user's group ids cross-joined with
    //    permission ids via the group_permission M2M.
    let group_ids: Vec<i64> = UserGroup::objects()
        .filter(user_group::USER_ID.eq(user_id))
        .fetch()
        .await?
        .into_iter()
        .map(|ug| ug.group_id.id())
        .collect();
    if group_ids.is_empty() {
        return Ok(false);
    }

    Ok(GroupPermission::objects()
        .filter(
            group_permission::GROUP_ID.in_(&group_ids)
                & group_permission::PERMISSION_ID.in_(&perm_ids),
        )
        .exists()
        .await?)
}

/// Convenience wrapper that short-circuits immediately when `is_superuser`
/// is `true`, otherwise delegates to `has_perm`.
///
/// This is the idiomatic form for handler code that has already loaded the
/// user struct:
///
/// ```ignore
/// let allowed = has_perm_for_superuser(user.id(), user.is_superuser(), "blog.publish_post").await?;
/// ```
pub async fn has_perm_for_superuser(
    user_id: i64,
    is_superuser: bool,
    perm: &str,
) -> Result<bool, PermError> {
    if is_superuser {
        return Ok(true);
    }
    has_perm(user_id, perm).await
}

/// Return the full permission set for `user_id` as a `HashSet` of
/// `"app_label.codename"` strings. Covers both direct user permissions
/// and group-mediated permissions.
///
/// Superuser bypass is NOT applied — the set reflects only what is in the
/// database. Callers that want to short-circuit for superusers should check
/// `user.is_superuser()` before calling this.
pub async fn user_perms(user_id: i64) -> Result<HashSet<String>, PermError> {
    // Direct permission ids granted to this user.
    let mut perm_ids: Vec<i64> = UserPermission::objects()
        .filter(user_permission::USER_ID.eq(user_id))
        .fetch()
        .await?
        .into_iter()
        .map(|up| up.permission_id.id())
        .collect();

    // Group-mediated: find the user's groups, then the permissions on those.
    let group_ids: Vec<i64> = UserGroup::objects()
        .filter(user_group::USER_ID.eq(user_id))
        .fetch()
        .await?
        .into_iter()
        .map(|ug| ug.group_id.id())
        .collect();
    if !group_ids.is_empty() {
        let mediated: Vec<i64> = GroupPermission::objects()
            .filter(group_permission::GROUP_ID.in_(&group_ids))
            .fetch()
            .await?
            .into_iter()
            .map(|gp| gp.permission_id.id())
            .collect();
        perm_ids.extend(mediated);
    }
    perm_ids.sort();
    perm_ids.dedup();
    if perm_ids.is_empty() {
        return Ok(HashSet::new());
    }

    // Hydrate permission rows for the codenames, then their content_types
    // for the app_labels. Two more queries; small result sets at v1.
    let perms: Vec<Permission> = Permission::objects()
        .filter(permission::ID.in_(&perm_ids))
        .fetch()
        .await?;
    let ct_ids: Vec<i64> = perms.iter().map(|p| p.content_type_id.id()).collect();
    let ct_map: HashMap<i64, String> = ContentType::objects()
        .filter(content_type::ID.in_(&ct_ids))
        .fetch()
        .await?
        .into_iter()
        .map(|c| (c.id, c.app_label))
        .collect();

    Ok(perms
        .into_iter()
        .filter_map(|p| {
            ct_map
                .get(&p.content_type_id.id())
                .map(|app| format!("{app}.{}", p.codename))
        })
        .collect())
}
