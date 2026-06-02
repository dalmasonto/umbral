//! The six permission data models.
//!
//! All six tables are namespaced under the "permissions" plugin prefix so they
//! don't collide with user models or other plugins. Explicit `table = "..."`
//! attributes are used to get the exact table names we want (the macro's default
//! snake_case would produce `permissions_content_type` instead of
//! `permissions_contenttype`, etc.).
//!
//! | Struct | Table |
//! |---|---|
//! | `ContentType` | `permissions_contenttype` |
//! | `Permission` | `permissions_permission` |
//! | `Group` | `permissions_group` |
//! | `GroupPermission` | `permissions_grouppermission` |
//! | `UserGroup` | `permissions_usergroup` |
//! | `UserPermission` | `permissions_userpermission` |
//!
//! `user_id` in `UserGroup` and `UserPermission` is `i64` (not
//! `ForeignKey<U>`) to keep the data model generic — we don't tie to a
//! concrete user type, so any `UserModel` implementation works.
//!
//! ## Edit / no-edit policy
//!
//! Most columns are marked `#[umbra(noedit)]` because changing them breaks
//! the system: renaming a `codename` invalidates every `has_permission(...)`
//! check in code; flipping a join row's FK is semantically a delete+create,
//! not an edit. The columns that *are* editable carry display-only labels —
//! they're safe to rename and the system keeps working.
//!
//! | Table | Editable columns | No-edit columns |
//! |---|---|---|
//! | `ContentType` | (none — system-managed at boot) | `app_label`, `model` |
//! | `Permission` | `name` (human label) | `content_type_id`, `codename` |
//! | `Group` | `name`, `description` | (none — both are user-facing) |
//! | `GroupPermission` | (none — delete + create instead) | `group_id`, `permission_id` |
//! | `UserGroup` | (none — delete + create instead) | `user_id`, `group_id` |
//! | `UserPermission` | (none — delete + create instead) | `user_id`, `permission_id` |
//!
//! The `noedit` attribute is metadata only (it lands in `ModelMeta`, not in
//! the DDL), so adding it doesn't dirty any existing schema.

use serde::{Deserialize, Serialize};
use umbra::orm::ForeignKey;

/// One row per Model in the project. Identifies a model by its plugin
/// (app_label) and lowercased struct name.
///
/// Standard rows are auto-created during `PermissionsPlugin::on_ready`
/// for every model registered with the framework.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(
    table = "permissions_contenttype",
    display = "Content types",
    icon = "list"
)]
pub struct ContentType {
    pub id: i64,
    /// The plugin name that owns the model. For bare (un-namespaced) tables
    /// this is `"app"`. System-managed at boot — editing would orphan every
    /// permission attached to this row.
    #[umbra(noedit)]
    pub app_label: String,
    /// The lowercased model / table suffix. For `blog_post` this is `"post"`.
    /// System-managed — see `app_label`.
    #[umbra(noedit)]
    pub model: String,
}

/// One permission. Standard permissions (add_X, change_X, delete_X, view_X)
/// are auto-created at boot; custom ones are inserted by user code or
/// management commands.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(
    table = "permissions_permission",
    display = "Permissions",
    icon = "key"
)]
pub struct Permission {
    pub id: i64,
    /// Which model this permission is scoped to. Re-targeting is a
    /// delete-and-create, not an edit.
    #[umbra(noedit)]
    pub content_type_id: ForeignKey<ContentType>,
    /// Short machine-readable key referenced from `has_permission(...)`
    /// call sites across the project. Renaming this breaks every check.
    #[umbra(noedit)]
    pub codename: String,
    /// Human-readable label shown in the admin. Examples: `"Can publish post"`,
    /// `"Can add post"`. Editable — it's display text, no code reads it.
    #[umbra(string, max_length = 150)]
    pub name: String,
}

/// A named group that bundles multiple permissions. Users can be assigned
/// to one or more groups; they inherit all permissions from every group
/// they are in.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "permissions_group", display = "Groups", icon = "users")]
pub struct Group {
    pub id: i64,
    /// A unique, human-readable name (e.g. `"editors"`, `"moderators"`).
    /// Capped at 150 chars — admin renders a single-line input with
    /// `maxlength=150`, mirroring a SQL `VARCHAR(150)` length cap. The
    /// `string` flag marks this as the row's `__str__` representation
    /// for FK pickers.
    #[umbra(string, max_length = 150)]
    pub name: String,
    /// Free-form description of what the group is for. Nullable so a
    /// just-created group can skip it; the admin renders a textarea
    /// (no `max_length`) because group purpose commentary can be long.
    pub description: Option<String>,
}

/// Join table between groups and permissions (M2M).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(
    table = "permissions_grouppermission",
    display = "Group permissions",
    icon = "link-2"
)]
pub struct GroupPermission {
    pub id: i64,
    /// Re-targeting a join row is a delete+create, not an edit.
    #[umbra(noedit)]
    pub group_id: ForeignKey<Group>,
    #[umbra(noedit)]
    pub permission_id: ForeignKey<Permission>,
}

/// Join table between users and groups (M2M).
///
/// `user_id` is a plain `i64` (not `ForeignKey<U>`) so the table stays
/// backend-agnostic. Any `UserModel` implementation works without a
/// crate dependency loop.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(
    table = "permissions_usergroup",
    display = "User groups",
    icon = "user-check"
)]
pub struct UserGroup {
    pub id: i64,
    /// The `UserModel::id()` of the user. Re-targeting is a delete+create.
    #[umbra(noedit)]
    pub user_id: i64,
    #[umbra(noedit)]
    pub group_id: ForeignKey<Group>,
}

/// Direct user-to-permission assignment (M2M). Bypasses groups — a user
/// can hold a permission independently of group membership.
///
/// `user_id` is a plain `i64` for the same reason as in `UserGroup`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(
    table = "permissions_userpermission",
    display = "User permissions",
    icon = "user-cog"
)]
pub struct UserPermission {
    pub id: i64,
    /// The `UserModel::id()` of the user. Re-targeting is a delete+create.
    #[umbra(noedit)]
    pub user_id: i64,
    #[umbra(noedit)]
    pub permission_id: ForeignKey<Permission>,
}
