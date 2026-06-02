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
    /// this is `"app"`.
    pub app_label: String,
    /// The lowercased model / table suffix. For `blog_post` this is `"post"`.
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
    /// Which model this permission is scoped to.
    pub content_type_id: ForeignKey<ContentType>,
    /// Short machine-readable key. Examples: `"publish_post"`,
    /// `"add_post"`.
    pub codename: String,
    /// Human-readable label shown in the admin. Examples: `"Can publish post"`,
    /// `"Can add post"`.
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
    pub name: String,
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
    pub group_id: ForeignKey<Group>,
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
    /// The `UserModel::id()` of the user.
    pub user_id: i64,
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
    /// The `UserModel::id()` of the user.
    pub user_id: i64,
    pub permission_id: ForeignKey<Permission>,
}
