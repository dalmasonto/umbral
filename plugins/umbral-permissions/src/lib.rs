//! umbral-permissions — Role-Based Access Control plugin for umbral.
//!
//! Provides groups + permissions + content_type tables,
//! plus the `has_perm` / `user_perms` query layer. Admin UI for managing
//! groups and permissions is deferred (depends on gap 19's Tailwind admin
//! work). RLS predicate injection is also deferred — that is
//! `umbral-rls`'s job; this plugin provides the permission data.
//!
//! ## Data model
//!
//! | Model | Table | Purpose |
//! |---|---|---|
//! | `ContentType` | `permissions_contenttype` | One row per Model. `UNIQUE (app_label, model)` |
//! | `Permission` | `permissions_permission` | One row per (ContentType, codename). PK is the composite codename string; `content_type_id` indexed |
//! | `Group` | `permissions_group` | Named collection of permissions. `UNIQUE (name)` |
//! | `UserGroup` | `permissions_usergroup` | Explicit join: users ↔ groups. `UNIQUE (user_id, group_id)`; `user_id` indexed |
//! | `UserPermission` | `permissions_userpermission` | Explicit join: users ↔ permissions (direct). `UNIQUE (user_id, permission_id)`; `user_id` indexed |
//!
//! Plus one framework-managed M2M junction:
//!
//! | Junction table | Backing field |
//! |---|---|
//! | `permissions_group_permissions` | `Group.permissions: M2M<Permission>` |
//!
//! The `User`-side joins stay as explicit models because this plugin
//! is user-agnostic — it doesn't own a `User` struct to attach
//! `M2M<...>` fields to.
//!
//! ## The `has_perm` decision: free function, not a trait method
//!
//! Putting `has_perm` on the `UserModel` trait in `umbral-auth` would
//! introduce a dependency from `umbral-auth` on `umbral-permissions` (to
//! call the perm query) *and* from `umbral-permissions` on `umbral-auth`
//! (to read the `UserModel` trait). That is a circular dependency Cargo
//! will refuse to compile.
//!
//! The clean resolution: `has_perm` / `user_perms` are free functions in
//! this crate that take `user_id: i64`. The `UserModel` trait doesn't
//! need to know about permissions. Any code that has a `U: UserModel` just
//! calls `umbral_permissions::has_perm(user.id(), "blog.publish_post").await`.
//!
//! ## Standard permission auto-creation
//!
//! When `PermissionsPlugin` boots, `on_ready` walks every model registered
//! with the framework via `umbral::migrate::registered_models()` and
//! ensures four standard permission rows exist for each:
//! `add_<model>`, `change_<model>`, `delete_<model>`, `view_<model>`.
//!
//! ## Plugin registration
//!
//! ```ignore
//! use umbral::prelude::*;
//! use umbral_permissions::PermissionsPlugin;
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
//! - RLS predicate injection wired through `umbral-rls`.
//! - ContentType auto-population for plugins not yet loaded at boot.

pub mod membership;
pub mod middleware;
pub mod models;
pub mod perm;
pub mod routes_ext;

/// REST plugin extension — adapter types that let `umbral-rest`'s
/// viewset permission gates check `umbral-permissions` codenames.
/// Off by default; enable with `umbral-permissions = { features =
/// ["rest"] }`.
#[cfg(feature = "rest")]
pub mod rest;

pub use middleware::{
    PermissionRequired, PermissionRequiredLayer, permission_required, permission_required_html,
};
pub use models::{ContentType, Group, Permission, UserGroup, UserPermission};
pub use perm::{PermError, has_perm, has_perm_for_superuser, has_perm_scoped, user_perms};
pub use routes_ext::RoutesPermExt;

/// gap #61 part 2: typed M2M-shape membership helpers
/// (`add_user_to_group`, `set_user_groups`, `grant_user_permission`,
/// `groups_for_user`, etc.). Sit on top of the explicit `UserGroup`
/// / `UserPermission` junction models — see `membership.rs`'s
/// module docstring for the cross-crate dep-arrow reasoning that
/// keeps those models user-facing.
pub use membership::{
    add_user_to_group, direct_permissions_for_user, grant_user_permission, group_ids_for_user,
    groups_for_user, has_direct_user_permission, is_in_group, remove_user_from_group,
    revoke_user_permission, set_user_groups,
};

use umbral::plugin::{AppContext, Plugin, PluginError, block_on_ready};
use umbral::web::Router;

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

    fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
        // Five explicit models — the sixth (group ↔ permission join)
        // is gone; its data lives in the auto-generated
        // `permissions_group_permissions` junction the migration
        // engine emits from `Group.permissions: M2M<Permission>`.
        vec![
            umbral::migrate::ModelMeta::for_::<ContentType>(),
            umbral::migrate::ModelMeta::for_::<Permission>(),
            umbral::migrate::ModelMeta::for_::<Group>(),
            umbral::migrate::ModelMeta::for_::<UserGroup>(),
            umbral::migrate::ModelMeta::for_::<UserPermission>(),
        ]
    }

    fn routes(&self) -> Router {
        Router::new()
    }

    fn on_ready(&self, ctx: &AppContext) -> Result<(), PluginError> {
        let pool = ctx.pool.clone();
        // on_ready is a sync trait method; sqlx is async. Use the shared
        // block_on_ready helper which handles multi-thread runtimes,
        // current-thread runtimes (#[tokio::test]), and the no-runtime
        // case without panicking.
        block_on_ready(ensure_standard_permissions(&pool))
            .map_err(|e| -> PluginError { Box::new(e) })?;
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
/// Re-run the standard-permission seed loop. Public for integration
/// tests that need to call it AFTER they've run the migration engine
/// to create the schema (the plugin's `on_ready` skip-with-grace
/// would otherwise leave the rows un-seeded on a fresh DB).
///
/// Not part of the v1 public plugin contract — the typical user flow
/// is `cargo run -- migrate && cargo run -- serve` which seeds on the
/// second boot. Marked `#[doc(hidden)]` to keep it off the stable
/// surface.
#[doc(hidden)]
pub async fn seed_standard_permissions_for_tests() -> Result<(), sqlx::Error> {
    let pool = umbral::db::pool_dispatched().clone();
    ensure_standard_permissions(&pool).await
}

async fn ensure_standard_permissions(_pool: &umbral::db::DbPool) -> Result<(), sqlx::Error> {
    // Walk the model registry and create the four standard permissions
    // (`add_<model>`, `change_<model>`, `delete_<model>`, `view_<model>`)
    // for every registered model. Every row write goes through the ORM
    // — `Manager::get_or_create` is the right primitive for "fetch the
    // row if it exists, insert with these defaults otherwise". The
    // UNIQUE constraints on `(app_label, model)` and
    // `(content_type_id, codename)` are the race-condition backstop.
    //
    // `on_ready` fires inside `App::build()` — which happens BEFORE
    // `migrate` in the typical user-binary flow. On a fresh database
    // the permissions tables therefore don't exist yet. We probe with
    // a cheap `count` on ContentType; if the table is missing we skip
    // gracefully so boot completes. The next boot (post-migrate) seeds
    // the rows. Previously this file carried a SQLite-only
    // `CREATE TABLE IF NOT EXISTS` bootstrap block as a documented
    // schema-DDL exception — that block is gone now that the ORM's
    // `get_or_create` lets us skip-with-grace on missing tables.
    if ContentType::objects().count().await.is_err() {
        tracing::debug!(
            "permissions: skipping row seed — permissions_contenttype not present yet \
             (run `migrate`, then re-boot to seed standard permissions)"
        );
        return Ok(());
    }
    let registered_models = umbral::migrate::registered_models();
    tracing::info!(
        plugin = "permissions",
        model_count = registered_models.len(),
        "auto-creating standard permissions"
    );
    for meta in &registered_models {
        // Derive app_label and model_name from the ModelMeta.
        //
        // Convention:
        //   - app_label = app name (e.g. "blog")
        //   - model     = lowercase struct name (e.g. "post" for struct
        //                 Post, "blogpost" for BlogPost)
        //
        // `model` is `meta.name.to_lowercase()`. The
        // `app_label` is the authoritative value carried on the model's
        // `#[umbral(plugin = "...")]` attribute (gaps2 #80g), surfaced via
        // `Model::APP_LABEL` → `ModelMeta::app_label`; bare models default
        // to `"app"`. This replaces the old table-name-split heuristic,
        // which collided distinct models (a bare `post` and a plugin
        // `app_post` both produced `app.add_post`).
        let model_name = meta.name.to_lowercase();
        let app_label = meta.app_label.clone();

        let (ct, _created) = ContentType::objects()
            .get_or_create(
                models::content_type::APP_LABEL.eq(&app_label)
                    & models::content_type::MODEL.eq(&model_name),
                ContentType {
                    id: 0,
                    app_label: app_label.clone(),
                    model: model_name.clone(),
                },
            )
            .await
            .map_err(|e| sqlx::Error::Protocol(format!("permissions seed content_type: {e:?}")))?;

        let standard_perms = [
            ("add", format!("Can add {model_name}")),
            ("change", format!("Can change {model_name}")),
            ("delete", format!("Can delete {model_name}")),
            ("view", format!("Can view {model_name}")),
        ];

        for (verb, label) in &standard_perms {
            // Composite codename — the new PK shape (gap #60). One
            // string identifies the permission across the whole
            // system; admin / has_perm / FK references all use it.
            let codename = format!("{app_label}.{verb}_{model_name}");
            Permission::objects()
                .get_or_create(
                    umbral::orm::Predicate::<Permission>::col_eq("codename", codename.clone()),
                    Permission {
                        codename,
                        content_type_id: umbral::orm::ForeignKey::new(ct.id),
                        name: label.clone(),
                    },
                )
                .await
                .map_err(|e| {
                    sqlx::Error::Protocol(format!("permissions seed permission: {e:?}"))
                })?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use umbral::migrate::ModelMeta;

    /// gaps2 #80g: the app_label used for permission codenames now comes
    /// straight off `ModelMeta::app_label` (sourced from
    /// `#[umbral(plugin = "...")]`), NOT from splitting the table name at
    /// the first `_`. Two models whose tables would split to the same
    /// prefix under the old heuristic must no longer collide.
    #[test]
    fn app_label_comes_from_model_meta_not_table_split() {
        // A plugin-tagged model: table is namespaced `blog_post`, but the
        // authoritative app_label is the plugin name "blog".
        let plugin_model = ModelMeta {
            name: "Post".to_string(),
            table: "blog_post".to_string(),
            app_label: "blog".to_string(),
            ..ModelMeta::default()
        };
        // A bare app model whose table happens to start with "blog_": the
        // OLD table-split heuristic would have read "blog" here too, but the
        // model never set a plugin, so its app_label is the default "app".
        let bare_model = ModelMeta {
            name: "BlogEntry".to_string(),
            table: "blog_entry".to_string(),
            app_label: "app".to_string(),
            ..ModelMeta::default()
        };

        // The codename is `<app_label>.<verb>_<model>`. The two distinct
        // models get DISTINCT app_labels and therefore distinct codenames.
        let plugin_codename = format!(
            "{}.add_{}",
            plugin_model.app_label,
            plugin_model.name.to_lowercase()
        );
        let bare_codename = format!(
            "{}.add_{}",
            bare_model.app_label,
            bare_model.name.to_lowercase()
        );
        assert_eq!(plugin_codename, "blog.add_post");
        assert_eq!(bare_codename, "app.add_blogentry");
        assert_ne!(
            plugin_codename, bare_codename,
            "distinct models must not collide on permission codename"
        );
    }

    /// The old heuristic split BOTH `post` and `app_post` to a shared
    /// `app` prefix, colliding their codenames. With `app_label` carried
    /// on the meta, a plugin model and a bare model keep separate labels.
    #[test]
    fn previously_colliding_models_now_diverge() {
        let bare = ModelMeta {
            name: "Post".to_string(),
            table: "post".to_string(),
            app_label: "app".to_string(),
            ..ModelMeta::default()
        };
        // A plugin shipping its own `Post` model, table namespaced to
        // `app_post`. Old code: `table_app_label("app_post") == "app"`,
        // same as the bare `post` → both `app.add_post`. New code: the
        // plugin sets `#[umbral(plugin = "shop")]`, so app_label == "shop".
        let plugin = ModelMeta {
            name: "Post".to_string(),
            table: "app_post".to_string(),
            app_label: "shop".to_string(),
            ..ModelMeta::default()
        };
        let bare_cn = format!("{}.add_{}", bare.app_label, bare.name.to_lowercase());
        let plugin_cn = format!("{}.add_{}", plugin.app_label, plugin.name.to_lowercase());
        assert_eq!(bare_cn, "app.add_post");
        assert_eq!(plugin_cn, "shop.add_post");
        assert_ne!(bare_cn, plugin_cn, "collision must be gone");
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
