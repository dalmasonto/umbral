//! Integration tests for umbral-permissions (gap 33).
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
//! name lowercased (the content-type model name, which is the
//! lowercase struct name). So `BlogPost` → model `"blogpost"`, `Post` → `"post"`.
//! `app_label` is the authoritative `#[umbral(plugin = "...")]` value carried on
//! the model and surfaced via `Model::APP_LABEL` → `ModelMeta::app_label`
//! (gaps2 #80g). A model with no `plugin` attribute defaults to `"app"`. This
//! replaced the old heuristic of splitting the table name at the first `_`,
//! which collided distinct models.
//!
//! Standard permissions for `BlogPost` (`#[umbral(plugin = "blog")]`) therefore
//! have codenames `blog.add_blogpost`, `blog.change_blogpost`, etc., while a
//! plugin-less `Memo` lands under `app.add_memo`.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral_permissions::{
    PermissionsPlugin, has_perm, has_perm_for_superuser, has_perm_scoped, user_perms,
};

// A minimal model to exercise standard perm auto-creation.
// With `#[umbral(plugin = "blog")]`, the table becomes `blog_blog_post`.
// app_label = "blog", model = "blogpost" (lowercase struct name).
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(plugin = "blog")]
pub struct BlogPost {
    pub id: i64,
    pub title: String,
}

// A second model with NO `#[umbral(plugin)]` attribute. Its app_label
// defaults to "app", so its codenames are `app.add_memo` etc. — distinct
// from BlogPost's `blog.add_blogpost`. Under the old table-name-split
// heuristic both a bare model and a plugin model could collide; this model
// proves the app_label now comes from the attribute, not the table.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
pub struct Memo {
    pub id: i64,
    pub body: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let db_path = tmp.path().join("umbral_permissions_integration.sqlite");
        std::mem::forget(tmp);

        let options = SqliteConnectOptions::new()
            .busy_timeout(std::time::Duration::from_secs(5))
            .filename(&db_path)
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .expect("sqlite should connect");

        let settings =
            umbral::Settings::from_env().expect("figment defaults always load in a test env");

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
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<BlogPost>()
            .model::<Memo>()
            .plugin(PermissionsPlugin)
            .build()
            .expect("App::build with PermissionsPlugin should succeed");

        // Apply every pending migration so the schema exists.
        let migration_dir = tempfile::tempdir().expect("migration dir");
        let migration_dir_path = migration_dir.path().to_path_buf();
        std::mem::forget(migration_dir);
        umbral::migrate::make_in(&migration_dir_path)
            .await
            .expect("make migrations");
        umbral::migrate::run_in(&migration_dir_path)
            .await
            .expect("run migrations");
        // Re-run the on_ready seed step now that the tables exist.
        umbral_permissions::seed_standard_permissions_for_tests()
            .await
            .expect("seed permissions");
    })
    .await;
}

fn pool() -> sqlx::SqlitePool {
    umbral::db::pool()
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
    // model = lowercase struct name = "blogpost".
    // Post-gap-#60: codename is the composite `<app_label>.<verb>_<model>`
    // form (e.g. `blog.add_blogpost`) and serves as the table's PK.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM permissions_permission \
         WHERE codename IN ('blog.add_blogpost','blog.change_blogpost', \
                            'blog.delete_blogpost','blog.view_blogpost')",
    )
    .fetch_one(&pool)
    .await
    .expect("count query should succeed");

    assert_eq!(
        count, 4,
        "expected 4 standard permissions for blogpost, got {count}"
    );
}

/// gaps2 #80g: two distinct registered models that the OLD heuristic could
/// have collided now get DISTINCT app_labels (and therefore distinct
/// codenames). `BlogPost` is `#[umbral(plugin = "blog")]` → `blog.add_blogpost`;
/// `Memo` has no plugin attribute → `app.add_memo`. The two never collide.
#[tokio::test(flavor = "multi_thread")]
async fn distinct_models_get_distinct_app_labels() {
    boot().await;
    let pool = pool();

    // The plugin-less Memo model is seeded under app_label "app".
    let memo_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM permissions_permission \
         WHERE codename IN ('app.add_memo','app.change_memo', \
                            'app.delete_memo','app.view_memo')",
    )
    .fetch_one(&pool)
    .await
    .expect("memo count query");
    assert_eq!(
        memo_count, 4,
        "expected 4 standard perms for memo under the 'app' label"
    );

    // The Memo model must NOT have been seeded under the 'blog' label, and
    // BlogPost must NOT appear under the 'app' label — the app_labels are
    // taken from `#[umbral(plugin)]`, not from any shared table-name prefix.
    let crossed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM permissions_permission \
         WHERE codename IN ('blog.add_memo','app.add_blogpost')",
    )
    .fetch_one(&pool)
    .await
    .expect("cross-label query");
    assert_eq!(
        crossed, 0,
        "models must not bleed across app_labels: no blog.add_memo / app.add_blogpost"
    );

    // The two distinct content types carry distinct app_labels.
    let blog_ct: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM permissions_contenttype \
         WHERE app_label = 'blog' AND model = 'blogpost')",
    )
    .fetch_one(&pool)
    .await
    .expect("blog ct");
    let memo_ct: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM permissions_contenttype \
         WHERE app_label = 'app' AND model = 'memo')",
    )
    .fetch_one(&pool)
    .await
    .expect("memo ct");
    assert!(blog_ct, "BlogPost content type under app_label 'blog'");
    assert!(memo_ct, "Memo content type under app_label 'app'");
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
    let result = has_perm("9999", "blog.publish_blogpost")
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

    let result = has_perm("1", "nodotsomewhere")
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

    let user_id: &str = "101";

    // Get the ContentType for blog/blogpost.
    let ct_id: i64 = sqlx::query_scalar(
        "SELECT id FROM permissions_contenttype WHERE app_label = 'blog' AND model = 'blogpost'",
    )
    .fetch_one(&pool)
    .await
    .expect("ContentType for blog/blogpost must exist");

    // Insert a custom permission with composite codename PK. Post-
    // gap-#60: no integer id; the codename string IS the PK.
    sqlx::query(
        "INSERT OR IGNORE INTO permissions_permission (codename, content_type_id, name)
         VALUES ('blog.publish_blogpost', ?, 'Can publish blog post')",
    )
    .bind(ct_id)
    .execute(&pool)
    .await
    .expect("insert permission");

    // Grant the permission directly to user 101. permission_id now
    // holds the codename string instead of an integer FK.
    sqlx::query(
        "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id)
         VALUES (?, ?)",
    )
    .bind(user_id)
    .bind("blog.publish_blogpost")
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

    let user_id: &str = "202";

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

    // The codename `blog.add_blogpost` is the standard permission's
    // PK; no intermediate id lookup needed post-gap-#60.
    let perm_pk = "blog.add_blogpost";

    // Grant the permission to the group. Post BUG-16 phase 3 cleanup
    // the join lives in the auto-generated M2M junction
    // `permissions_group_permissions` with `parent_id` (group_id) +
    // `child_id` (codename string) — no more standalone
    // `permissions_grouppermission` table.
    sqlx::query(
        "INSERT OR IGNORE INTO permissions_group_permissions (parent_id, child_id)
         VALUES (?, ?)",
    )
    .bind(group_id)
    .bind(perm_pk)
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
    let result = has_perm_for_superuser("8888", true, "blog.delete_blogpost")
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
    let result = has_perm_for_superuser("7777", false, "blog.delete_blogpost")
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

    let user_id: &str = "303";

    // --- direct permission: blog.view_blogpost ---
    // Post-gap-#60: the composite codename IS the PK; no integer id
    // lookup. Bind the codename string directly into permission_id.
    sqlx::query(
        "INSERT OR IGNORE INTO permissions_userpermission (user_id, permission_id) VALUES (?, ?)",
    )
    .bind(user_id)
    .bind("blog.view_blogpost")
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

    sqlx::query(
        "INSERT OR IGNORE INTO permissions_group_permissions (parent_id, child_id) VALUES (?, ?)",
    )
    .bind(group_id)
    .bind("blog.change_blogpost")
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
    let result = has_perm_scoped("9999", "blog", "add_blogpost")
        .await
        .expect("should not error");

    assert!(!result, "user with no perms should fail has_perm_scoped");
}

// =========================================================================
// gap #61 part 2 — M2M-shape membership helpers
//
// add_user_to_group / remove_user_from_group / set_user_groups /
// grant_user_permission / revoke_user_permission etc. give callers an
// "AuthUser { groups: M2M<Group> }"-feel API on top of the explicit
// junction models that have to stay user-facing (cross-crate dep
// arrow blocks moving the M2M field onto AuthUser itself).
// =========================================================================

use umbral_permissions::membership;
use umbral_permissions::models::{Group, Permission};

async fn fetch_or_create_group(name: &str) -> Group {
    let pool = pool();
    sqlx::query(&format!(
        "INSERT OR IGNORE INTO permissions_group (name) VALUES ('{name}')"
    ))
    .execute(&pool)
    .await
    .expect("seed group");
    let id: i64 = sqlx::query_scalar(&format!(
        "SELECT id FROM permissions_group WHERE name = '{name}'"
    ))
    .fetch_one(&pool)
    .await
    .expect("group id");
    Group::objects()
        .filter(umbral::orm::Predicate::<Group>::col_eq("id", id))
        .first()
        .await
        .expect("fetch group")
        .expect("group present")
}

#[tokio::test(flavor = "multi_thread")]
async fn add_and_remove_user_to_group_round_trips() {
    boot().await;
    let group = fetch_or_create_group("membership_test_editors").await;
    let user = "8001";

    // Pre-state: not a member.
    assert!(!membership::is_in_group(user, group.id).await.unwrap());

    membership::add_user_to_group(user, &group)
        .await
        .expect("add");
    assert!(membership::is_in_group(user, group.id).await.unwrap());

    // Idempotent re-add — no error, still a member, no duplicate row.
    membership::add_user_to_group(user, &group)
        .await
        .expect("idempotent add");
    let groups = membership::groups_for_user(user).await.expect("fetch");
    let count = groups.iter().filter(|g| g.id == group.id).count();
    assert_eq!(count, 1, "re-adding must not insert a duplicate row");

    membership::remove_user_from_group(user, &group)
        .await
        .expect("remove");
    assert!(!membership::is_in_group(user, group.id).await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn set_user_groups_replaces_full_set() {
    boot().await;
    let g_a = fetch_or_create_group("set_users_test_a").await;
    let g_b = fetch_or_create_group("set_users_test_b").await;
    let g_c = fetch_or_create_group("set_users_test_c").await;
    let user = "8002";

    // Start with two groups.
    membership::set_user_groups(user, &[g_a.id, g_b.id])
        .await
        .unwrap();
    let ids: Vec<i64> = membership::groups_for_user(user)
        .await
        .unwrap()
        .into_iter()
        .map(|g| g.id)
        .collect();
    assert!(ids.contains(&g_a.id));
    assert!(ids.contains(&g_b.id));
    assert!(!ids.contains(&g_c.id));

    // Replace with a different set — A goes, C arrives.
    membership::set_user_groups(user, &[g_b.id, g_c.id])
        .await
        .unwrap();
    let ids: Vec<i64> = membership::groups_for_user(user)
        .await
        .unwrap()
        .into_iter()
        .map(|g| g.id)
        .collect();
    assert!(!ids.contains(&g_a.id), "set must drop A");
    assert!(ids.contains(&g_b.id), "B was in both sets, keeps");
    assert!(ids.contains(&g_c.id), "C is new");

    // Empty set wipes all memberships.
    membership::set_user_groups(user, &[]).await.unwrap();
    let groups = membership::groups_for_user(user).await.unwrap();
    assert!(groups.is_empty(), "empty set clears every membership");
}

/// Verify that `set_user_groups` is atomic: if the INSERT half of the
/// DELETE+INSERT replacement fails (here: a duplicate group_id in the
/// input triggers the UNIQUE constraint on `(user_id, group_id)` inside
/// `bulk_create_in_tx`), the whole transaction rolls back and the prior
/// membership set is preserved unchanged.
///
/// Note: we cannot inject a true concurrent race deterministically in a
/// unit test, but we can prove the transactional-replacement contract
/// holds — "on INSERT failure, the old set survives" — which is
/// equivalent: if the transaction commits, the caller sees the new set;
/// if it aborts, the caller sees the old set; there is no intermediate
/// empty window visible to any reader.
#[tokio::test(flavor = "multi_thread")]
async fn set_user_groups_rolls_back_on_insert_failure() {
    boot().await;
    let g_a = fetch_or_create_group("rollback_test_a").await;
    let g_b = fetch_or_create_group("rollback_test_b").await;
    let user = "8099";

    // Establish a known initial state: user is a member of g_a only.
    membership::set_user_groups(user, &[g_a.id]).await.unwrap();
    let before: Vec<i64> = membership::groups_for_user(user)
        .await
        .unwrap()
        .into_iter()
        .map(|g| g.id)
        .collect();
    assert_eq!(before, vec![g_a.id], "pre-condition: user is in g_a only");

    // Attempt a replacement that will fail: passing the same group_id
    // twice triggers the UNIQUE(user_id, group_id) constraint on the
    // multi-row INSERT inside `bulk_create_in_tx`, causing the whole
    // transaction (including the preceding DELETE) to roll back.
    let result = membership::set_user_groups(user, &[g_b.id, g_b.id]).await;
    assert!(result.is_err(), "duplicate group_id must return an error");

    // After the failed replacement, the original membership must be intact.
    let after: Vec<i64> = membership::groups_for_user(user)
        .await
        .unwrap()
        .into_iter()
        .map(|g| g.id)
        .collect();
    assert_eq!(
        after,
        vec![g_a.id],
        "transaction rolled back: prior membership (g_a) must be preserved"
    );
    assert!(
        !after.contains(&g_b.id),
        "g_b must NOT appear after rollback"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn grant_and_revoke_user_permission_round_trips() {
    boot().await;
    let user = "8003";
    let perm = Permission::objects()
        .filter(umbral::orm::Predicate::<Permission>::col_eq(
            "codename",
            "blog.view_blogpost".to_string(),
        ))
        .first()
        .await
        .expect("fetch perm")
        .expect("standard permission seeded");

    assert!(
        !membership::has_direct_user_permission(user, &perm.codename)
            .await
            .unwrap()
    );
    membership::grant_user_permission(user, &perm)
        .await
        .expect("grant");
    assert!(
        membership::has_direct_user_permission(user, &perm.codename)
            .await
            .unwrap()
    );

    // grant_user_permission is idempotent.
    membership::grant_user_permission(user, &perm)
        .await
        .expect("re-grant ok");
    let direct = membership::direct_permissions_for_user(user).await.unwrap();
    let count = direct
        .iter()
        .filter(|p| p.codename == perm.codename)
        .count();
    assert_eq!(count, 1, "re-grant must not duplicate the junction row");

    membership::revoke_user_permission(user, &perm)
        .await
        .expect("revoke");
    assert!(
        !membership::has_direct_user_permission(user, &perm.codename)
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn has_perm_uses_membership_helpers_after_refactor() {
    // Regression on the `has_perm_scoped` refactor: the body now uses
    // `membership::group_ids_for_user` for the group lookup. The
    // group-mediated path must still return `true` end-to-end.
    boot().await;
    let user = "8004";
    let group = fetch_or_create_group("has_perm_refactor_group").await;
    membership::add_user_to_group(user, &group).await.unwrap();

    // Grant the permission to the group via the auto-junction.
    sqlx::query(
        "INSERT OR IGNORE INTO permissions_group_permissions (parent_id, child_id) VALUES (?, ?)",
    )
    .bind(group.id)
    .bind("blog.delete_blogpost")
    .execute(&pool())
    .await
    .expect("seed group permission");

    assert!(
        has_perm(user, "blog.delete_blogpost").await.unwrap(),
        "user → group → permission path must survive the refactor"
    );
}

// =========================================================================
// audit_2 plugin-authz P2 / gaps3 #28 — object (row-level) permissions
//
// The object-level analogue of the direct/group grants above. These
// exercise the full public path — grant → check → list-filter → revoke —
// against real rows, and pin the IDOR-fixing property: a grant on one
// object does NOT authorize a different object, and a MODEL-level grant
// does NOT satisfy the object-level check.
// =========================================================================

use umbral_permissions::{
    grant_object_permission, has_object_perm, has_object_perm_for_superuser, objects_with_perm,
    revoke_object_permission, revoke_object_permissions_for,
};

/// Fetch a real seeded `Permission` row by its composite codename PK so the
/// object-permission grants reference a permission that actually exists.
async fn fetch_perm(codename: &str) -> Permission {
    Permission::objects()
        .filter(umbral::orm::Predicate::<Permission>::col_eq(
            "codename", codename,
        ))
        .first()
        .await
        .expect("fetch permission")
        .expect("standard permission must be seeded")
}

#[tokio::test(flavor = "multi_thread")]
async fn object_perm_scopes_to_the_granted_row_only() {
    boot().await;
    let perm = fetch_perm("blog.change_blogpost").await;
    let user = "obj-1001";

    // No grant yet → false for every object.
    assert!(
        !has_object_perm(user, "blog.change_blogpost", "42")
            .await
            .unwrap(),
        "no grant → no object perm"
    );

    // Grant change on post #42 only.
    grant_object_permission(user, &perm, "42")
        .await
        .expect("grant");

    // Granted object passes; a DIFFERENT object does NOT — this is the
    // IDOR fix: holding the grant on #42 gives no authority over #99.
    assert!(
        has_object_perm(user, "blog.change_blogpost", "42")
            .await
            .unwrap(),
        "granted object #42 must pass"
    );
    assert!(
        !has_object_perm(user, "blog.change_blogpost", "99")
            .await
            .unwrap(),
        "un-granted object #99 must NOT pass — per-row scoping"
    );

    // A different permission on the same object is also unscoped.
    assert!(
        !has_object_perm(user, "blog.delete_blogpost", "42")
            .await
            .unwrap(),
        "grant is per (permission, object) — change ≠ delete"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn model_level_grant_does_not_satisfy_object_check() {
    boot().await;
    let user = "obj-1002";

    // Give the user the MODEL-level grant (any blogpost).
    let perm = fetch_perm("blog.change_blogpost").await;
    umbral_permissions::grant_user_permission(user, &perm)
        .await
        .expect("model-level grant");

    // Model-level check passes...
    assert!(
        has_perm(user, "blog.change_blogpost").await.unwrap(),
        "model-level grant satisfies has_perm"
    );
    // ...but the object-level check does NOT fall back to it. Without an
    // explicit per-object grant the instance-aware check is false — the
    // whole point of the primitive.
    assert!(
        !has_object_perm(user, "blog.change_blogpost", "7")
            .await
            .unwrap(),
        "has_object_perm must NOT inherit the model-level grant"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn objects_with_perm_returns_exactly_the_granted_pks() {
    boot().await;
    let perm = fetch_perm("blog.view_blogpost").await;
    let user = "obj-1003";

    for pk in ["a", "b", "c"] {
        grant_object_permission(user, &perm, pk).await.unwrap();
    }
    // A grant for a DIFFERENT permission must not bleed into this set.
    let other = fetch_perm("blog.delete_blogpost").await;
    grant_object_permission(user, &other, "z").await.unwrap();

    let pks = objects_with_perm(user, "blog.view_blogpost").await.unwrap();
    assert_eq!(
        pks,
        ["a", "b", "c"].into_iter().map(String::from).collect(),
        "objects_with_perm returns exactly the granted pks for that permission"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn grant_is_idempotent_and_revoke_removes() {
    boot().await;
    let perm = fetch_perm("blog.change_blogpost").await;
    let user = "obj-1004";

    grant_object_permission(user, &perm, "55").await.unwrap();
    // Re-granting the same triple is a no-op, not a UNIQUE-violation error.
    grant_object_permission(user, &perm, "55")
        .await
        .expect("idempotent re-grant");
    let pks = objects_with_perm(user, "blog.change_blogpost")
        .await
        .unwrap();
    assert_eq!(pks.len(), 1, "re-grant must not duplicate the row");

    // Revoke removes exactly that grant.
    revoke_object_permission(user, &perm, "55").await.unwrap();
    assert!(
        !has_object_perm(user, "blog.change_blogpost", "55")
            .await
            .unwrap(),
        "revoke removes the grant"
    );
    // Revoking a non-existent grant is a forgiving no-op.
    revoke_object_permission(user, &perm, "does-not-exist")
        .await
        .expect("revoke of missing grant is a no-op");
}

#[tokio::test(flavor = "multi_thread")]
async fn revoke_object_permissions_for_clears_every_grantee() {
    boot().await;
    let perm = fetch_perm("blog.change_blogpost").await;

    // Two different users both hold the grant on post "shared-77".
    grant_object_permission("obj-a", &perm, "shared-77")
        .await
        .unwrap();
    grant_object_permission("obj-b", &perm, "shared-77")
        .await
        .unwrap();
    // A grant on a DIFFERENT object must survive the row cleanup.
    grant_object_permission("obj-a", &perm, "other-88")
        .await
        .unwrap();

    // Simulate the target row being deleted: clear all grants for it.
    revoke_object_permissions_for(&perm, "shared-77")
        .await
        .unwrap();

    assert!(
        !has_object_perm("obj-a", "blog.change_blogpost", "shared-77")
            .await
            .unwrap(),
        "grantee A's grant on the deleted row is gone"
    );
    assert!(
        !has_object_perm("obj-b", "blog.change_blogpost", "shared-77")
            .await
            .unwrap(),
        "grantee B's grant on the deleted row is gone"
    );
    assert!(
        has_object_perm("obj-a", "blog.change_blogpost", "other-88")
            .await
            .unwrap(),
        "grants on a different object are untouched"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn superuser_bypasses_object_perm() {
    boot().await;
    // No grant anywhere, but is_superuser short-circuits to true.
    assert!(
        has_object_perm_for_superuser("obj-super", true, "blog.change_blogpost", "1")
            .await
            .unwrap(),
        "superuser passes without any object grant"
    );
    // Non-superuser with no grant still fails.
    assert!(
        !has_object_perm_for_superuser("obj-super", false, "blog.change_blogpost", "1")
            .await
            .unwrap(),
        "non-superuser with no grant fails"
    );
}
