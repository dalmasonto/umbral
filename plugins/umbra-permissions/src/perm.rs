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

use std::collections::HashSet;

use crate::models::{Group, UserGroup, UserPermission, user_group, user_permission};
// Post-gap-#60 simplification: `has_perm` and `user_perms` no longer
// look up `ContentType` or `Permission` rows directly — the codename
// IS the FK value carried in `UserPermission` (direct grant) and the
// child PK in the `Group.permissions` M2M junction (group-mediated
// grant). The membership check is one equality predicate against the
// FK column, or one macro-emitted `Group::permissions_contains_any`
// call against the auto-generated junction.

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
    // Post-gap-#60: `Permission` is keyed by the composite codename
    // (`"<app_label>.<codename>"`), so the membership check collapses
    // to a direct PK comparison — no content_type → permission ID
    // join needed. Two queries: direct grant, then group-mediated.
    let pk = format!("{app_label}.{codename}");

    // 1. Direct user → permission grant. Predicate::col_eq is the
    //    typed-erased escape hatch for FK columns whose target uses
    //    a non-i64 PK type (string in this case) — `ForeignKeyCol`'s
    //    typed predicate API is still i64-shaped at v1.
    let direct = UserPermission::objects()
        .filter(
            user_permission::USER_ID.eq(user_id)
                & umbra::orm::Predicate::<UserPermission>::col_eq("permission_id", pk.clone()),
        )
        .exists()
        .await?;
    if direct {
        return Ok(true);
    }

    // 2. Group-mediated grant: find every group the user is in, then
    //    ask the macro-emitted `Group::permissions_contains_any`
    //    helper. That lowers to `SELECT 1 FROM <junction> WHERE
    //    parent_id IN (?,?,?) AND child_id = ? LIMIT 1` — one
    //    round-trip regardless of group count, and we never have to
    //    spell the junction-table name ourselves.
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

    Ok(Group::permissions_contains_any(&group_ids, pk).await?)
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
    // Post-gap-#60: the codename IS the permission_id FK value, so
    // there's nothing left to join. Collect FK values directly and
    // return them.
    let mut codenames: Vec<String> = UserPermission::objects()
        .filter(user_permission::USER_ID.eq(user_id))
        .fetch()
        .await?
        .into_iter()
        .map(|up| up.permission_id.id())
        .collect();

    // Group-mediated grants — one round-trip through the macro-
    // emitted `Group::permissions_union_for` helper. It SELECTs
    // `DISTINCT child_id` for the matching `parent_id IN (...)`, so
    // no per-group fan-out and no dedup pass needed here.
    let group_ids: Vec<i64> = UserGroup::objects()
        .filter(user_group::USER_ID.eq(user_id))
        .fetch()
        .await?
        .into_iter()
        .map(|ug| ug.group_id.id())
        .collect();
    if !group_ids.is_empty() {
        let mediated: Vec<String> = Group::permissions_union_for(&group_ids).await?;
        codenames.extend(mediated);
    }

    Ok(codenames.into_iter().collect())
}
