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
    let pool = umbra::db::pool();

    // One query covering both direct user permissions and group-mediated
    // permissions via a UNION. Using UNION (not UNION ALL) means we get
    // at most one row back even when both paths match, which is all we need
    // for the boolean answer.
    //
    // Direct path:
    //   user_permission -> permission -> content_type
    //
    // Group path:
    //   user_group -> group_permission -> permission -> content_type
    //
    // EXISTS is cheaper than COUNT(*) because the DB can stop after finding
    // the first matching row.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
            FROM permissions_userpermission up
            JOIN permissions_permission p ON p.id = up.permission_id
            JOIN permissions_contenttype ct ON ct.id = p.content_type_id
            WHERE up.user_id = ?
              AND p.codename = ?
              AND ct.app_label = ?

            UNION

            SELECT 1
            FROM permissions_usergroup ug
            JOIN permissions_grouppermission gp ON gp.group_id = ug.group_id
            JOIN permissions_permission p ON p.id = gp.permission_id
            JOIN permissions_contenttype ct ON ct.id = p.content_type_id
            WHERE ug.user_id = ?
              AND p.codename = ?
              AND ct.app_label = ?
        )",
    )
    .bind(user_id)
    .bind(codename)
    .bind(app_label)
    .bind(user_id)
    .bind(codename)
    .bind(app_label)
    .fetch_one(&pool)
    .await?;

    Ok(exists)
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
    let pool = umbra::db::pool();

    // Collect direct permissions.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT ct.app_label, p.codename
         FROM permissions_userpermission up
         JOIN permissions_permission p ON p.id = up.permission_id
         JOIN permissions_contenttype ct ON ct.id = p.content_type_id
         WHERE up.user_id = ?",
    )
    .bind(user_id)
    .fetch_all(&pool)
    .await?;

    let mut set: HashSet<String> = rows
        .into_iter()
        .map(|(app, code)| format!("{app}.{code}"))
        .collect();

    // Collect group-mediated permissions and union them in.
    let group_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT ct.app_label, p.codename
         FROM permissions_usergroup ug
         JOIN permissions_grouppermission gp ON gp.group_id = ug.group_id
         JOIN permissions_permission p ON p.id = gp.permission_id
         JOIN permissions_contenttype ct ON ct.id = p.content_type_id
         WHERE ug.user_id = ?",
    )
    .bind(user_id)
    .fetch_all(&pool)
    .await?;

    for (app, code) in group_rows {
        set.insert(format!("{app}.{code}"));
    }

    Ok(set)
}
