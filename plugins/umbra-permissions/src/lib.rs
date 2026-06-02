//! umbra-permissions — Role-Based Access Control plugin for umbra.
//!
//! Provides Django-style groups + permissions + content_type tables,
//! plus the `has_perm` / `user_perms` query layer. Admin UI for managing
//! groups and permissions is deferred (depends on gap 19's Tailwind admin
//! work). RLS predicate injection is also deferred — that is
//! `umbra-rls`'s job; this plugin provides the permission data.
//!
//! ## Data model
//!
//! | Model | Table | Purpose |
//! |---|---|---|
//! | `ContentType` | `permissions_contenttype` | One row per Model (app_label + model name) |
//! | `Permission` | `permissions_permission` | One row per (ContentType, codename) |
//! | `Group` | `permissions_group` | Named collection of permissions |
//! | `GroupPermission` | `permissions_grouppermission` | M2M: groups ↔ permissions |
//! | `UserGroup` | `permissions_usergroup` | M2M: users ↔ groups |
//! | `UserPermission` | `permissions_userpermission` | M2M: users ↔ permissions (direct) |
//!
//! ## The `has_perm` decision: free function, not a trait method
//!
//! Putting `has_perm` on the `UserModel` trait in `umbra-auth` would
//! introduce a dependency from `umbra-auth` on `umbra-permissions` (to
//! call the perm query) *and* from `umbra-permissions` on `umbra-auth`
//! (to read the `UserModel` trait). That is a circular dependency Cargo
//! will refuse to compile.
//!
//! The clean resolution: `has_perm` / `user_perms` are free functions in
//! this crate that take `user_id: i64`. The `UserModel` trait doesn't
//! need to know about permissions. Any code that has a `U: UserModel` just
//! calls `umbra_permissions::has_perm(user.id(), "blog.publish_post").await`.
//!
//! ## Standard permission auto-creation
//!
//! When `PermissionsPlugin` boots, `on_ready` walks every model registered
//! with the framework via `umbra::migrate::registered_models()` and
//! ensures four standard permission rows exist for each:
//! `add_<model>`, `change_<model>`, `delete_<model>`, `view_<model>`.
//!
//! ## Plugin registration
//!
//! ```ignore
//! use umbra::prelude::*;
//! use umbra_permissions::PermissionsPlugin;
//!
//! App::builder()
//!     .plugin(AuthPlugin::default())
//!     .plugin(PermissionsPlugin::default())
//!     .build()?;
//! ```
//!
//! ## Deferred
//!
//! - Admin UI for RBAC (gap 19 + follow-on to gap 33).
//! - `permission_required(perm)` tower layer / extractor (gap 26 follow-on).
//! - RLS predicate injection wired through `umbra-rls`.
//! - ContentType auto-population for plugins not yet loaded at boot.

pub mod middleware;
pub mod models;
pub mod perm;

pub use middleware::{
    PermissionRequired, PermissionRequiredLayer, permission_required, permission_required_html,
};
pub use models::{ContentType, Group, GroupPermission, Permission, UserGroup, UserPermission};
pub use perm::{PermError, has_perm, has_perm_for_superuser, has_perm_scoped, user_perms};

use umbra::plugin::{AppContext, Plugin, PluginError};
use umbra::web::Router;

// =========================================================================
// PermissionsPlugin
// =========================================================================

/// The RBAC plugin. Contributes the six permission models and, in
/// `on_ready`, auto-creates the four standard permissions for every
/// model registered with the framework.
#[derive(Debug, Default)]
pub struct PermissionsPlugin;

impl Plugin for PermissionsPlugin {
    fn name(&self) -> &'static str {
        "permissions"
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        vec![
            umbra::migrate::ModelMeta::for_::<ContentType>(),
            umbra::migrate::ModelMeta::for_::<Permission>(),
            umbra::migrate::ModelMeta::for_::<Group>(),
            umbra::migrate::ModelMeta::for_::<GroupPermission>(),
            umbra::migrate::ModelMeta::for_::<UserGroup>(),
            umbra::migrate::ModelMeta::for_::<UserPermission>(),
        ]
    }

    fn routes(&self) -> Router {
        Router::new()
    }

    fn on_ready(&self, ctx: &AppContext) -> Result<(), PluginError> {
        let pool = ctx.pool.clone();
        // on_ready is a sync trait method; sqlx is async. Two bridging paths:
        //
        // (a) If we are already inside a tokio runtime (the normal case:
        //     the user's #[tokio::main] or the test's #[tokio::test]),
        //     `block_in_place` parks the current OS thread and runs the
        //     async work on it without blocking the executor thread pool.
        //
        // (b) If there is no ambient runtime (uncommon; a bare main that
        //     calls App::build before spinning up tokio), we fall back to
        //     a one-shot Runtime.
        //
        // This matches the pattern used by other umbra plugins (umbra-rls
        // uses Handle::current().block_on which panics in tokio tests;
        // block_in_place is the correct form when already inside a runtime).
        match tokio::runtime::Handle::try_current() {
            Ok(_) => {
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(ensure_standard_permissions(&pool))
                })
                .map_err(|e| -> PluginError { Box::new(e) })?;
            }
            Err(_) => {
                tokio::runtime::Runtime::new()
                    .expect("tokio runtime for PermissionsPlugin::on_ready")
                    .block_on(ensure_standard_permissions(&pool))
                    .map_err(|e| -> PluginError { Box::new(e) })?;
            }
        }
        Ok(())
    }
}

/// Ensure the six permissions tables exist (DDL idempotently) and then
/// auto-create the standard four permissions for every registered model.
///
/// We create the tables manually here because `on_ready` fires after
/// `App::build` which does not run `migrate` automatically. In production,
/// users run `cargo run -- migrate` once. For the `on_ready` auto-create to
/// work in tests (without a full migrate run), we emit `CREATE TABLE IF NOT
/// EXISTS` for each permissions table.
///
/// Standard permissions created: `add_<model>`, `change_<model>`,
/// `delete_<model>`, `view_<model>`.
async fn ensure_standard_permissions(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    // DDL: create all six tables if they do not yet exist.
    // In a real app these are created by `migrate`; this guard lets
    // `on_ready` work even when the user runs the app before migrate (e.g.
    // in tests that skip the full migrate loop).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS permissions_contenttype (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            app_label TEXT NOT NULL,
            model TEXT NOT NULL,
            UNIQUE(app_label, model)
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS permissions_permission (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content_type_id INTEGER NOT NULL REFERENCES permissions_contenttype(id),
            codename TEXT NOT NULL,
            name TEXT NOT NULL,
            UNIQUE(content_type_id, codename)
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS permissions_group (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS permissions_grouppermission (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            group_id INTEGER NOT NULL REFERENCES permissions_group(id),
            permission_id INTEGER NOT NULL REFERENCES permissions_permission(id),
            UNIQUE(group_id, permission_id)
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS permissions_usergroup (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL,
            group_id INTEGER NOT NULL REFERENCES permissions_group(id),
            UNIQUE(user_id, group_id)
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS permissions_userpermission (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL,
            permission_id INTEGER NOT NULL REFERENCES permissions_permission(id),
            UNIQUE(user_id, permission_id)
        )",
    )
    .execute(pool)
    .await?;

    // Auto-create standard permissions for every registered model.
    let models = umbra::migrate::registered_models();
    tracing::info!(
        plugin = "permissions",
        model_count = models.len(),
        "auto-creating standard permissions"
    );
    for meta in &models {
        // Derive app_label and model_name from the ModelMeta.
        //
        // Django uses:
        //   - app_label = app name (e.g. "blog")
        //   - model     = lowercase class name (e.g. "post" for class Post, "blogpost" for BlogPost)
        //
        // We mirror this: `model` is `meta.name.to_lowercase()` (the Rust struct
        // name lowercased, e.g. "BlogPost" → "blogpost"). `app_label` is derived
        // from the table's first segment before `_`: "blog_blog_post" → "blog";
        // bare tables (no `_`) use "app".
        let model_name = meta.name.to_lowercase();
        let app_label = table_app_label(&meta.table);

        // Upsert the ContentType row (INSERT OR IGNORE gives idempotency).
        sqlx::query(
            "INSERT OR IGNORE INTO permissions_contenttype (app_label, model)
             VALUES (?, ?)",
        )
        .bind(&app_label)
        .bind(&model_name)
        .execute(pool)
        .await?;

        // Fetch the ContentType id.
        let ct_id: i64 = sqlx::query_scalar(
            "SELECT id FROM permissions_contenttype WHERE app_label = ? AND model = ?",
        )
        .bind(&app_label)
        .bind(&model_name)
        .fetch_one(pool)
        .await?;

        // Upsert the four standard permissions.
        let standard_perms = [
            (format!("add_{model_name}"), format!("Can add {model_name}")),
            (
                format!("change_{model_name}"),
                format!("Can change {model_name}"),
            ),
            (
                format!("delete_{model_name}"),
                format!("Can delete {model_name}"),
            ),
            (
                format!("view_{model_name}"),
                format!("Can view {model_name}"),
            ),
        ];

        for (codename, name) in &standard_perms {
            sqlx::query(
                "INSERT OR IGNORE INTO permissions_permission
                 (content_type_id, codename, name)
                 VALUES (?, ?, ?)",
            )
            .bind(ct_id)
            .bind(codename)
            .bind(name)
            .execute(pool)
            .await?;
        }
    }

    Ok(())
}

/// Derive the app_label from a table name by taking the first segment
/// before the first underscore.
///
/// Examples:
/// - `"blog_post"` → `"blog"`
/// - `"blog_blog_post"` → `"blog"`
/// - `"permissions_contenttype"` → `"permissions"`
/// - `"post"` → `"app"` (no prefix, treated as the implicit "app" plugin)
///
/// This function is used only for ContentType population. The `model` field
/// comes from `ModelMeta::name.to_lowercase()`, NOT from the table suffix.
fn table_app_label(table: &str) -> String {
    if let Some(pos) = table.find('_') {
        table[..pos].to_string()
    } else {
        "app".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_app_label_extracts_first_segment() {
        assert_eq!(table_app_label("blog_post"), "blog");
        assert_eq!(table_app_label("blog_blog_post"), "blog");
        assert_eq!(table_app_label("permissions_contenttype"), "permissions");
    }

    #[test]
    fn table_app_label_bare_table_returns_app() {
        assert_eq!(table_app_label("post"), "app");
    }

    #[test]
    fn model_name_is_lowercase_struct_name() {
        // The model field in ContentType is always meta.name.to_lowercase().
        // Verify the expected transformations.
        assert_eq!("blogpost".to_string(), "BlogPost".to_lowercase());
        assert_eq!("post".to_string(), "Post".to_lowercase());
        assert_eq!("contenttype".to_string(), "ContentType".to_lowercase());
    }
}
