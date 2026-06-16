//! M2M-shape write helpers for the User ↔ Group and User ↔ Permission
//! relations.
//!
//! Gap #61 part 2 spec asked for `AuthUser { groups: M2M<Group>,
//! permissions: M2M<Permission> }` — the framework auto-generates the
//! junction tables, no user-facing junction models. The Group →
//! Permission side of that vision is shipped (`Group { permissions:
//! M2M<Permission> }`; the explicit `GroupPermission` model was
//! retired). The User side is structurally blocked: `AuthUser` lives
//! in `umbra-auth`, `Group` / `Permission` live in `umbra-permissions`,
//! and the dep arrow runs `umbra-permissions → umbra-auth` so the
//! permissions plugin can call into the session helper. Reversing the
//! arrow to let AuthUser name `Group` would create a cycle.
//!
//! The pragmatic substitute: keep `UserGroup` / `UserPermission` as
//! explicit junction models (which they have to be to stay user-PK-
//! agnostic — they carry `user_id: String`, not `ForeignKey<AuthUser>`)
//! but expose THIS module as the M2M-shaped API on top of them. Call
//! sites read like:
//!
//! ```ignore
//! use umbra_permissions::membership;
//! membership::add_user_to_group(user_id, &group).await?;
//! membership::set_user_groups(user_id, &[group_a.id, group_b.id]).await?;
//! membership::grant_user_permission(user_id, &perm).await?;
//! let groups = membership::groups_for_user(user_id).await?;
//! ```
//!
//! Same call shape they'd write if `AuthUser { groups: M2M<Group> }`
//! actually existed; just routed through the visible junction tables.

use crate::models::{Group, Permission, UserGroup, UserPermission, user_group, user_permission};
use crate::perm::PermError;

/// Add `user_id` to `group`. Idempotent — re-adding an existing
/// membership is a no-op (the junction's `unique_together = [["user_id",
/// "group_id"]]` constraint would otherwise reject the duplicate INSERT
/// with a UNIQUE violation). Use [`set_user_groups`] when you want
/// "exactly these groups, no others."
pub async fn add_user_to_group(user_id: &str, group: &Group) -> Result<(), PermError> {
    if is_in_group(user_id, group.id).await? {
        return Ok(());
    }
    match UserGroup::objects()
        .create(UserGroup {
            id: 0,
            user_id: user_id.to_string(),
            group_id: umbra::orm::ForeignKey::new(group.id),
        })
        .await
    {
        Ok(_) | Err(umbra::orm::write::WriteError::UniqueViolation { .. }) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// Remove `user_id` from `group`. No error if the user wasn't a
/// member — same forgive-and-continue posture as
/// [`add_user_to_group`].
pub async fn remove_user_from_group(user_id: &str, group: &Group) -> Result<(), PermError> {
    UserGroup::objects()
        .filter(
            user_group::USER_ID.eq(user_id.to_string())
                & umbra::orm::Predicate::<UserGroup>::col_eq("group_id", group.id),
        )
        .delete()
        .await?;
    Ok(())
}

/// Replace `user_id`'s entire group membership set with `group_ids`.
/// Removes any current memberships not in the new set, adds any
/// missing ones. Two queries total (one DELETE, one bulk INSERT)
/// regardless of the diff size.
///
/// Empty `group_ids` clears every membership. Use when the admin's
/// "Save & continue" on the user-edit form persists the new
/// checkbox set — the form posts the whole desired state, not a
/// delta.
pub async fn set_user_groups(user_id: &str, group_ids: &[i64]) -> Result<(), PermError> {
    let rows: Vec<UserGroup> = group_ids
        .iter()
        .map(|gid| UserGroup {
            id: 0,
            user_id: user_id.to_string(),
            group_id: umbra::orm::ForeignKey::new(*gid),
        })
        .collect();
    let user_id = user_id.to_string();
    umbra::db::transaction(|tx| {
        Box::pin(async move {
            UserGroup::objects()
                .filter(user_group::USER_ID.eq(user_id))
                .on_tx(tx)
                .delete()
                .await?;
            if !rows.is_empty() {
                UserGroup::objects().bulk_create_in_tx(rows, tx).await?;
            }
            Ok::<_, PermError>(())
        })
    })
    .await
}

/// Resolve `user_id`'s group memberships as full `Group` rows. One
/// query (the `UserGroup` fetch) + one IN-list query against `Group`
/// via `select_related` — no N+1.
pub async fn groups_for_user(user_id: &str) -> Result<Vec<Group>, PermError> {
    let memberships = UserGroup::objects()
        .filter(user_group::USER_ID.eq(user_id.to_string()))
        .select_related("group_id")
        .fetch()
        .await?;
    Ok(memberships
        .into_iter()
        .filter_map(|ug| ug.group_id.resolved().cloned())
        .collect())
}

/// Lightweight check: is `user_id` a member of the group with the
/// given id? One query (`SELECT 1 FROM permissions_usergroup WHERE
/// user_id = ? AND group_id = ? LIMIT 1`).
pub async fn is_in_group(user_id: &str, group_id: i64) -> Result<bool, PermError> {
    Ok(UserGroup::objects()
        .filter(
            user_group::USER_ID.eq(user_id.to_string())
                & umbra::orm::Predicate::<UserGroup>::col_eq("group_id", group_id),
        )
        .exists()
        .await?)
}

/// Grant `perm` directly to `user_id`. Idempotent (re-granting is a
/// no-op). For group-mediated grants, add the user to a group whose
/// `permissions` set carries the perm instead.
pub async fn grant_user_permission(user_id: &str, perm: &Permission) -> Result<(), PermError> {
    if has_direct_user_permission(user_id, &perm.codename).await? {
        return Ok(());
    }
    match UserPermission::objects()
        .create(UserPermission {
            id: 0,
            user_id: user_id.to_string(),
            permission_id: umbra::orm::ForeignKey::new(perm.codename.clone()),
        })
        .await
    {
        Ok(_) | Err(umbra::orm::write::WriteError::UniqueViolation { .. }) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// Revoke `perm` from `user_id`. No error if the user didn't hold it
/// directly. Group-mediated grants are unaffected — remove the user
/// from the group OR remove the perm from the group's `permissions`
/// M2M to revoke those.
pub async fn revoke_user_permission(user_id: &str, perm: &Permission) -> Result<(), PermError> {
    UserPermission::objects()
        .filter(
            user_permission::USER_ID.eq(user_id.to_string())
                & umbra::orm::Predicate::<UserPermission>::col_eq(
                    "permission_id",
                    perm.codename.clone(),
                ),
        )
        .delete()
        .await?;
    Ok(())
}

/// Resolve `user_id`'s DIRECT permissions (not group-mediated). For
/// the full effective set, call [`crate::perm::user_perms`] which
/// unions direct + group-mediated. One query + one IN-list via
/// `select_related`.
pub async fn direct_permissions_for_user(user_id: &str) -> Result<Vec<Permission>, PermError> {
    let grants = UserPermission::objects()
        .filter(user_permission::USER_ID.eq(user_id.to_string()))
        .select_related("permission_id")
        .fetch()
        .await?;
    Ok(grants
        .into_iter()
        .filter_map(|up| up.permission_id.resolved().cloned())
        .collect())
}

/// Lightweight check: does `user_id` hold `codename` as a direct
/// grant? One query. For "any path" (direct OR via group), use
/// [`crate::perm::has_perm`].
pub async fn has_direct_user_permission(user_id: &str, codename: &str) -> Result<bool, PermError> {
    Ok(UserPermission::objects()
        .filter(
            user_permission::USER_ID.eq(user_id.to_string())
                & umbra::orm::Predicate::<UserPermission>::col_eq(
                    "permission_id",
                    codename.to_string(),
                ),
        )
        .exists()
        .await?)
}

/// Internal helper used by the perm-check hot path
/// ([`crate::perm::has_perm_scoped`] and friends). Returns the bare
/// list of group ids the user is a member of. One query.
///
/// Public-but-low-level: prefer [`groups_for_user`] when you want
/// the full `Group` rows; this one exists for cases where the
/// caller will pass the ids into a typed M2M helper like
/// `Group::permissions_contains_any`.
pub async fn group_ids_for_user(user_id: &str) -> Result<Vec<i64>, PermError> {
    Ok(UserGroup::objects()
        .filter(user_group::USER_ID.eq(user_id.to_string()))
        .fetch()
        .await?
        .into_iter()
        .map(|ug| ug.group_id.id())
        .collect())
}
