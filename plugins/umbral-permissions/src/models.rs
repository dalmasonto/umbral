//! The five permission data models.
//!
//! All tables are namespaced under the "permissions" plugin prefix so they
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
//! | `UserGroup` | `permissions_usergroup` |
//! | `UserPermission` | `permissions_userpermission` |
//!
//! Plus one auto-generated M2M junction table:
//!
//! | Owner field | Junction table |
//! |---|---|
//! | `Group.permissions: M2M<Permission>` | `permissions_group_permissions` |
//!
//! Closes the BUG-16 phase 3 follow-up: the original `GroupPermission`
//! explicit join model is gone. The framework's `M2M<T, P>` (with
//! string child PK support landed in BUG-16 phase 2) carries the
//! group → permission set instead, and the perm-query layer reaches
//! the junction via `M2M::any_holds` / `M2M::holders_of_any`. The
//! cutover IS a destructive schema change: any existing
//! `permissions_grouppermission` table goes away and its rows have to
//! be copied into `permissions_group_permissions` by hand or
//! re-seeded.
//!
//! `user_id` in `UserGroup` and `UserPermission` is `String` (not
//! `ForeignKey<U>`, not `i64`) to keep the data model PK-agnostic.
//! The plugin doesn't own a `User` struct to point at — different
//! apps wire different user models, some with `i64` PKs, some with
//! `uuid::Uuid`, some with slug-style `String` codenames. Storing
//! the column as `TEXT` (max 64 chars — covers UUIDs at 36 chars and
//! i64-as-string at 20) lets any PK type round-trip.
//!
//! Callers convert their user's PK to string at the perm-call
//! boundary:
//!
//! ```ignore
//! let granted = has_perm(&user.id().to_string(), "blog.publish").await?;
//! // For UUID user PKs:
//! let granted = has_perm(&user.id().to_string(), "blog.publish").await?;
//! // For Uuid PKs (uuid crate's Display gives the canonical form):
//! let granted = has_perm(&user.id().to_string(), "blog.publish").await?;
//! ```
//!
//! The trade-off vs `ForeignKey<U>` referential integrity: deleting a
//! user does NOT cascade-delete their UserGroup/UserPermission rows.
//! Apps that need that guarantee should add a per-user cleanup hook
//! in their user-delete path.
//!
//! ## Edit / no-edit policy
//!
//! The rule is "lock framework-managed rows, leave user-created rows
//! editable." That splits along table boundaries:
//!
//! - **Framework-managed**: `ContentType` rows (auto-seeded one-per-model
//!   at boot) and the *codename* + *content_type_id* of every `Permission`
//!   row (the standard `add_X` / `change_X` / `delete_X` / `view_X` set is
//!   auto-seeded, and renaming a codename invalidates every
//!   `has_permission(...)` check in user code). Both stay `#[umbral(noedit)]`
//!   — the admin renders them read-only.
//!
//! - **User-created**: every row in `Group`, `UserGroup`, `UserPermission`,
//!   plus the auto-managed `permissions_group_permissions` junction,
//!   is something a staff user wired through the admin. The data is
//!   editable; the junction rows aren't admin-formed at all (the
//!   admin chip-picker lands as a follow-up).
//!
//! The `noedit` attribute is metadata only (it lands in `ModelMeta`, not in
//! the DDL), so adding or removing it doesn't dirty any existing schema.

use serde::{Deserialize, Serialize};
use umbral::orm::{ForeignKey, M2M};

/// One row per Model in the project. Identifies a model by its plugin
/// (app_label) and lowercased struct name.
///
/// Standard rows are auto-created during `PermissionsPlugin::on_ready`
/// for every model registered with the framework.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(
    table = "permissions_contenttype",
    display = "Content types",
    icon = "list",
    // (app_label, model) globally identifies a content type — two
    // rows with the same pair would let two Permission rows with the
    // same `<app>.<codename>` point at different `content_type_id`,
    // silently breaking the perm-name → ContentType lookup. The
    // unique_together makes the invariant a DB-level guarantee
    // instead of a polite convention enforced by on_ready.
    unique_together = [["app_label", "model"]]
)]
pub struct ContentType {
    pub id: i64,
    /// The plugin name that owns the model. For bare (un-namespaced) tables
    /// this is `"app"`. System-managed at boot — editing would orphan every
    /// permission attached to this row.
    #[umbral(noedit)]
    pub app_label: String,
    /// The lowercased model / table suffix. For `blog_post` this is `"post"`.
    /// System-managed — see `app_label`.
    #[umbral(noedit)]
    pub model: String,
}

/// One permission, identified by its composite codename like
/// `"blog.publish_post"` or `"auth.add_user"`. Standard permissions
/// (`add_X`, `change_X`, `delete_X`, `view_X`) are auto-created at
/// boot; custom ones are inserted by user code or management
/// commands.
///
/// The primary key is `codename: String` — gap #60 swapped the
/// historical `id: i64` for the natural string identifier so admins
/// see meaningful labels (`"blog.publish_post"`) instead of opaque
/// integers (`10`), and `has_permission(user_id, "blog.publish_post")`
/// can look up the row by its PK directly. The composite form
/// (`"<app_label>.<codename>"`) guarantees global uniqueness without
/// a separate UNIQUE constraint.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(
    table = "permissions_permission",
    display = "Permissions",
    icon = "key"
)]
pub struct Permission {
    /// Composite codename — `"<app_label>.<codename>"`. Globally
    /// unique. Read-only because renaming a codename invalidates every
    /// `has_permission(...)` call site in code.
    #[umbral(primary_key, string, noedit, max_length = 150)]
    pub codename: String,
    /// Which model this permission is scoped to. Re-targeting is a
    /// delete-and-create, not an edit. Indexed because admin's
    /// "show all permissions for this content type" view filters here
    /// — without an index every page-load full-scans the table.
    #[umbral(noedit, index)]
    pub content_type_id: ForeignKey<ContentType>,
    /// Human-readable label shown in the admin. Examples: `"Can publish post"`,
    /// `"Can add post"`. Editable — it's display text, no code reads it.
    pub name: String,
}

/// A named group that bundles multiple permissions. Users can be assigned
/// to one or more groups; they inherit all permissions from every group
/// they are in.
///
/// The `permissions` field is an auto-junction M2M into [`Permission`].
/// Mutating it (`group.permissions.add(&perm).await?` /
/// `.remove(&perm)` / `.set(&[...])`) writes through to the
/// framework-managed `permissions_group_permissions` junction table
/// the migration engine auto-generates from the `M2M<Permission>`
/// type. Closes the BUG-16 phase 3 follow-up.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "permissions_group", display = "Groups", icon = "users")]
pub struct Group {
    pub id: i64,
    /// A unique, human-readable name (e.g. `"editors"`, `"moderators"`).
    /// Capped at 150 chars — admin renders a single-line input with
    /// `maxlength=150`, mirroring a SQL `VARCHAR(150)` length cap. The
    /// `string` flag marks this as the row's `__str__` representation
    /// for FK pickers. UNIQUE because two groups with the same name
    /// would make the admin sidebar ambiguous and break the typical
    /// `Group::objects().filter(name.eq("editors")).get()` lookup
    /// (`MultipleObjectsReturned`).
    #[umbral(string, unique, max_length = 150)]
    pub name: String,
    /// Free-form description of what the group is for. Nullable so a
    /// just-created group can skip it; the admin renders a textarea
    /// (no `max_length`) because group purpose commentary can be long.
    pub description: Option<String>,
    /// Set of permissions this group grants. Backed by the
    /// auto-generated junction table `permissions_group_permissions`
    /// — see [`Self`]'s docstring above. The field is `#[sqlx(skip)]`
    /// + `#[serde(skip)]` because it has no column on `permissions_group`:
    /// the junction lives in its own table and the framework hydrates
    /// `parent_id` + `junction_table` on every row materialised through
    /// `Group::objects().fetch()` / `.create()` etc.
    #[sqlx(skip)]
    #[serde(skip)]
    pub permissions: M2M<Permission>,
}

// No public junction-table constant here. The framework manages the
// junction's identity end-to-end:
//
// - The migration engine derives the table name from the parent's
//   table + field ident (`permissions_group_permissions`) and emits
//   the CREATE; it never appears in admin or OpenAPI because the
//   junction isn't a registered ModelMeta.
// - Application code that needs bulk-across-parents queries calls
//   `Group::permissions_contains_any(...)` / `Group::permissions_union_for(...)`
//   — typed helpers the `#[derive(Model)]` macro emits per M2M field.
//   The string never appears in user code.
// - The escape hatch (raw queries against the junction) lives at
//   `Group::permissions_junction_table()` for admin chip-picker
//   backends and similar low-level integrations.

/// Join table between users and groups (M2M).
///
/// `user_id` is a plain `i64` (not `ForeignKey<U>`) so the table stays
/// backend-agnostic. Any `UserModel` implementation works without a
/// crate dependency loop.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(
    table = "permissions_usergroup",
    display = "User groups",
    icon = "user-check",
    // Membership is a set, not a multiset: a user is either in a
    // group or not, never "in twice." Without this constraint the
    // admin's "+ Add" could create duplicate membership rows that
    // make `user_perms` return the same codename twice (HashSet
    // de-dupes it, but the redundant rows still pollute the table).
    unique_together = [["user_id", "group_id"]]
)]
pub struct UserGroup {
    pub id: i64,
    /// The `UserModel::id()` of the user, stringified. `String` (not
    /// `i64`) so the plugin works with any user PK type — UUID PKs
    /// round-trip via `Uuid::to_string()`, integer PKs via
    /// `i64::to_string()`. Capped at 64 chars (UUIDs are 36; the
    /// widest integer PKs need ≤20). Indexed because every
    /// `has_perm(user_id, ...)` query starts with "what groups is
    /// this user in?" — that's a `WHERE user_id = ?` on this table
    /// on the perm-check hot path. User-defined membership; staff
    /// users reassign through the admin.
    #[umbral(index, max_length = 64)]
    pub user_id: String,
    pub group_id: ForeignKey<Group>,
}

/// Direct user-to-permission assignment (M2M). Bypasses groups — a user
/// can hold a permission independently of group membership.
///
/// `user_id` is `String` (not `i64`) for the same reason as in
/// `UserGroup` — keeps the plugin PK-agnostic.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(
    table = "permissions_userpermission",
    display = "User permissions",
    icon = "user-cog",
    // Same set-not-multiset reasoning as `UserGroup`: a user holds a
    // permission directly or not, never "twice."
    unique_together = [["user_id", "permission_id"]]
)]
pub struct UserPermission {
    pub id: i64,
    /// The `UserModel::id()` of the user, stringified. Same `String`
    /// + `max_length = 64` shape as `UserGroup.user_id` — keeps the
    /// plugin PK-agnostic. Indexed for the `has_perm` direct-grant
    /// path (`WHERE user_id = ?`), the step-1 query of every
    /// permission check.
    #[umbral(index, max_length = 64)]
    pub user_id: String,
    pub permission_id: ForeignKey<Permission>,
}
