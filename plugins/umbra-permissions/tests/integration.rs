//! Integration tests for umbra-permissions (gap 33).
//!
//! Covers:
//!
//! 1. Standard permissions auto-created for every registered model.
//! 2. `has_perm` returns false when no permission rows exist for the user.
//! 3. Direct user permission -> `has_perm` returns true.
//! 4. Group-mediated permission -> `has_perm` returns true.
//! 5. `has_perm_for_superuser` returns true when `is_superuser = true`
//!    without checking any DB rows.
//! 6. `user_perms` returns the union of direct and group perms.
//!
//! All tests share a single `App::build` via `OnceCell`. The permissions
//! tables are created by `PermissionsPlugin::on_ready`, which runs inside
//! `App::build`.
//!
//! ## Model naming convention
//!
//! `ContentType.model` stores `meta.name.to_lowercase()`, i.e. the Rust struct
//! name lowercased — same as Django's `ContentType.model` which is the
//! lowercase class name. So `BlogPost` → model `"blogpost"`, `Post` → `"post"`.
//! `app_label` is the first segment of the table name: `blog_blog_post` → `"blog"`.
//!
//! Standard permissions for `BlogPost` therefore have codenames
//! `add_blogpost`, `change_blogpost`, `delete_blogpost`, `view_blogpost`.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbra_permissions::{
    PermissionsPlugin, has_perm, has_perm_for_superuser, has_perm_scoped, user_perms,
};

// A minimal model to exercise standard perm auto-creation.
// With `#[umbra(plugin = "blog")]`, the table becomes `blog_blog_post`.
// app_label = "blog", model = "blogpost" (lowercase struct name).
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(plugin = "blog")]
pub struct BlogPost {
    pub id: i64,
    pub title: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let db_path = tmp.path().join("umbra_permissions_integration.sqlite");
        std::mem::forget(tmp);

        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .expect("sqlite should connect");

        let settings =
            umbra::Settings::from_env().expect("figment defaults always load in a test env");

        // The permissions plugin no longer bootstraps its own tables in
        // `on_ready` (the SQLite-only `CREATE TABLE IF NOT EXISTS` block
        // was retired once `Manager::get_or_create` could skip-with-grace
        // on missing tables — see plugin lib.rs). The integration test
        // therefore needs to run the migration engine itself so the six
        // permissions_* tables exist when `on_ready` fires.
        //
        // Two-pass boot: build the App without the plugin first to ensure
        // the model registry has the perm models from the plugin's
        // `Plugin::models()`, run `migrate` to create the tables, then
        // re-boot WITH the plugin so its `on_ready` seeds the standard
        // permission rows.
        //
        // App::build can only fire once per process, so we collapse the
        // two passes: build with the plugin, then manually run migrate
        // (which the typical user binary calls explicitly via
        // `cargo run -- migrate` before `serve`). The plugin's on_ready
        // grace-skips its row seed if the tables aren't there yet, then
        // we run a follow-up `ensure_standard_permissions` pass after
        // migrate has created the schema.
        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<BlogPost>()
            .plugin(PermissionsPlugin)
            .build()
            .expect("App::build with PermissionsPlugin should succeed");

        // Apply every pending migration so the schema exists.
        let migration_dir = tempfile::tempdir().expect("migration dir");
        let migration_dir_path = migration_dir.path().to_path_buf();
        std::mem::forget(migration_dir);
        umbra::migrate::make_in(&migration_dir_path)
            .await
            .expect("make migrations");
        umbra::migrate::run_in(&migration_dir_path)
            .await
            .expect("run migrations");
        // Re-run the on_ready seed step now that the tables exist.
        umbra_permissions::seed_standard_permissions_for_tests()
            .await
            .expect("seed permissions");
    })
    .await;
}

fn pool() -> sqlx::SqlitePool {
    umbra::db::pool()
}

// ===========================================================================================
// Test 1: Standard permissions auto-created on plugin boot
// ===========================================================================================

/// After `PermissionsPlugin::on_ready`, the four standard permissions for
/// `BlogPost` (struct name "BlogPost", model = "blogpost", app_label = "blog")
/// should exist in the permissions tables.
#[tokio::test(flavor = "multi_thread")]
async fn standard_perms_auto_created_for_registered_models() {
    boot().await;
    let pool = pool();

    // The four standard permissions for blogpost should exist.
    // model = lowercase struct name = "blogpost"
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM permissions_permission p
         JOIN permissions_contenttype ct ON ct.id = p.content_type_id
         WHERE ct.app_label = 'blog' AND ct.model = 'blogpost'
           AND p.codename IN ('add_blogpost','change_blogpost','delete_blogpost','view_blogpost')",
    )
    .fetch_one(&pool)
    .await
    .expect("count query should succeed");

    assert_eq!(
        count, 4,
        "expected 4 standard permissions for blogpost, got {count}"
    );
}

/// The ContentType row for BlogPost exists after boot.
/// model = "blogpost" (lowercase of struct name "BlogPost")
#[tokio::test(flavor = "multi_thread")]
async fn content_type_row_created_for_blog_post() {
    boot().await;
    let pool = pool();

    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM permissions_contenttype
             WHERE app_label = 'blog' AND model = 'blogpost'
         )",
    )
    .fetch_one(&pool)
    .await
    .expect("exists query");

    assert!(
        exists,
        "ContentType row for blog/blogpost should exist after boot"
    );
}

// ===========================================================================================
// Test 2: has_perm returns false when no permission rows exist for the user
// ===========================================================================================

#[tokio::test(flavor = "multi_thread")]
async fn has_perm_returns_false_when_user_has_no_permissions() {
    boot().await;

    // user_id 9999 — doesn't exist in any permission table
    let result = has_perm(9999, "blog.publish_blogpost")
        .await
        .expect("has_perm should not error");
    assert!(
        !result,
        "user with no permissions should not have blog.publish_blogpost"
    );
}

/// Malformed perm string (no dot) returns false, not an error.
#[tokio::test(flavor = "multi_thread")]
async fn has_perm_returns_false_for_malformed_perm_string() {
    boot().await;

    let result = has_perm(1, "nodotsomewhere")
        .await
        .expect("should not error on malformed perm");
    assert!(!result, "malformed perm string should return false");
}

// ===========================================================================================
// Test 3: Direct user permission -> has_perm returns true
// ===========================================================================================

#[tokio::test(flavor = "multi_thread")]
async fn has_perm_returns_true_for_direct_user_permission() {
    boot().await;
    let pool = pool();

    let user_id: i64 = 101;

    // Get the ContentType for blog/blogpost.
    let ct_id: i64 = sqlx::query_scalar(
        "SELECT id FROM permissions_contenttype WHERE app_label = 'blog' AND model = 'blogpost'",
    )
    .fetch_one(&pool)
    .await
    .expect("ContentType for blog/blogpost must exist");

    // Insert a custom permission "publish_blogpost" for blog/blogpost.
    sqlx::query(
        "INSERT OR IGNORE INTO permissions_permission (content_type_id, codename, name)
         VALUES (?, 'publish_blogpost', 'Can publish blog post')",
    )
    .bind(ct_id)
    .execute(&pool)
    .await
    .expect("insert permission");

    let perm_id: i64 = sqlx::query_scalar(
        "SELECT id FROM permissions_permission WHERE content_type_id = ? AND codename = 'publish_blogpost'",
    )
    .bind(ct_id)
    .fetch_one(&pool)
    .await
    .expect("fetch perm id");

    // Grant the permission directly to user 101.
    sqlx::query(
        "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id)
         VALUES (?, ?)",
    )
    .bind(user_id)
    .bind(perm_id)
    .execute(&pool)
    .await
    .expect("insert user permission");

    let result = has_perm(user_id, "blog.publish_blogpost")
        .await
        .expect("has_perm should not error");

    assert!(
        result,
        "user with direct permission should have blog.publish_blogpost"
    );
}

// ===========================================================================================
// Test 4: Group-mediated permission -> has_perm returns true
// ===========================================================================================

#[tokio::test(flavor = "multi_thread")]
async fn has_perm_returns_true_for_group_permission() {
    boot().await;
    let pool = pool();

    let user_id: i64 = 202;

    // Create a group "editors".
    sqlx::query("INSERT OR IGNORE INTO permissions_group (name) VALUES ('editors')")
        .execute(&pool)
        .await
        .expect("insert group");

    let group_id: i64 =
        sqlx::query_scalar("SELECT id FROM permissions_group WHERE name = 'editors'")
            .fetch_one(&pool)
            .await
            .expect("fetch group id");

    // Look up "add_blogpost" standard perm (created at boot for BlogPost).
    let ct_id: i64 = sqlx::query_scalar(
        "SELECT id FROM permissions_contenttype WHERE app_label = 'blog' AND model = 'blogpost'",
    )
    .fetch_one(&pool)
    .await
    .expect("ContentType blog/blogpost");

    let perm_id: i64 = sqlx::query_scalar(
        "SELECT id FROM permissions_permission WHERE content_type_id = ? AND codename = 'add_blogpost'",
    )
    .bind(ct_id)
    .fetch_one(&pool)
    .await
    .expect("add_blogpost permission");

    // Grant the permission to the group.
    sqlx::query(
        "INSERT OR IGNORE INTO permissions_grouppermission (group_id, permission_id)
         VALUES (?, ?)",
    )
    .bind(group_id)
    .bind(perm_id)
    .execute(&pool)
    .await
    .expect("insert group permission");

    // Add user 202 to the editors group.
    sqlx::query("INSERT OR IGNORE INTO permissions_usergroup (user_id, group_id) VALUES (?, ?)")
        .bind(user_id)
        .bind(group_id)
        .execute(&pool)
        .await
        .expect("insert user group");

    let result = has_perm(user_id, "blog.add_blogpost")
        .await
        .expect("has_perm should not error");

    assert!(
        result,
        "user in editors group should have blog.add_blogpost via group"
    );
}

// ===========================================================================================
// Test 5: Superuser bypass
// ===========================================================================================

/// `has_perm_for_superuser` returns true for any perm when is_superuser = true,
/// even for a user_id that has zero permission rows.
#[tokio::test(flavor = "multi_thread")]
async fn superuser_always_passes_has_perm() {
    boot().await;

    // user_id 8888 has no DB rows at all.
    let result = has_perm_for_superuser(8888, true, "blog.delete_blogpost")
        .await
        .expect("should not error");

    assert!(
        result,
        "superuser should always pass has_perm regardless of DB state"
    );
}

/// Non-superuser with is_superuser = false still checks the DB.
#[tokio::test(flavor = "multi_thread")]
async fn non_superuser_falls_through_to_db_check() {
    boot().await;

    // user_id 7777 has no DB rows.
    let result = has_perm_for_superuser(7777, false, "blog.delete_blogpost")
        .await
        .expect("should not error");

    assert!(!result, "non-superuser with no DB perms should fail");
}

// ===========================================================================================
// Test 6: user_perms returns the union of direct and group perms
// ===========================================================================================

#[tokio::test(flavor = "multi_thread")]
async fn user_perms_returns_union_of_direct_and_group_perms() {
    boot().await;
    let pool = pool();

    let user_id: i64 = 303;

    // --- direct permission: blog.view_blogpost ---
    let ct_id: i64 = sqlx::query_scalar(
        "SELECT id FROM permissions_contenttype WHERE app_label = 'blog' AND model = 'blogpost'",
    )
    .fetch_one(&pool)
    .await
    .expect("ContentType blog/blogpost");

    let view_perm_id: i64 = sqlx::query_scalar(
        "SELECT id FROM permissions_permission WHERE content_type_id = ? AND codename = 'view_blogpost'",
    )
    .bind(ct_id)
    .fetch_one(&pool)
    .await
    .expect("view_blogpost permission");

    sqlx::query(
        "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id) VALUES (?, ?)",
    )
    .bind(user_id)
    .bind(view_perm_id)
    .execute(&pool)
    .await
    .expect("insert direct perm");

    // --- group permission: blog.change_blogpost ---
    sqlx::query("INSERT OR IGNORE INTO permissions_group (name) VALUES ('reviewers')")
        .execute(&pool)
        .await
        .expect("insert group");

    let group_id: i64 =
        sqlx::query_scalar("SELECT id FROM permissions_group WHERE name = 'reviewers'")
            .fetch_one(&pool)
            .await
            .expect("fetch group id");

    let change_perm_id: i64 = sqlx::query_scalar(
        "SELECT id FROM permissions_permission WHERE content_type_id = ? AND codename = 'change_blogpost'",
    )
    .bind(ct_id)
    .fetch_one(&pool)
    .await
    .expect("change_blogpost permission");

    sqlx::query(
        "INSERT OR IGNORE INTO permissions_grouppermission (group_id, permission_id) VALUES (?, ?)",
    )
    .bind(group_id)
    .bind(change_perm_id)
    .execute(&pool)
    .await
    .expect("insert group permission");

    sqlx::query("INSERT OR IGNORE INTO permissions_usergroup (user_id, group_id) VALUES (?, ?)")
        .bind(user_id)
        .bind(group_id)
        .execute(&pool)
        .await
        .expect("insert user group");

    // --- assert ---
    let perms = user_perms(user_id)
        .await
        .expect("user_perms should not error");

    assert!(
        perms.contains("blog.view_blogpost"),
        "user_perms should contain blog.view_blogpost (direct); got: {perms:?}"
    );
    assert!(
        perms.contains("blog.change_blogpost"),
        "user_perms should contain blog.change_blogpost (via group); got: {perms:?}"
    );
}

/// `has_perm_scoped` works with separate app_label + codename args.
#[tokio::test(flavor = "multi_thread")]
async fn has_perm_scoped_api_works() {
    boot().await;

    // user_id 9999 has no rows — should return false for any perm.
    let result = has_perm_scoped(9999, "blog", "add_blogpost")
        .await
        .expect("should not error");

    assert!(!result, "user with no perms should fail has_perm_scoped");
}
