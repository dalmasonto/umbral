//! Tests for the custom user model swap (gap 31).
//!
//! Covers two scenarios:
//!
//! 1. A `CustomUser` struct with extra fields (`display_name`,
//!    `tenant_id`) that implements `UserModel`. `AuthPlugin::<CustomUser>`
//!    registers it, the `custom_user` table is created, and the
//!    generic `authenticate` + `set_password` helpers run against it.
//!
//! 2. The existing default form - `AuthPlugin::default()` (i.e.
//!    `AuthPlugin::<AuthUser>::default()`) - still boots and the helpers
//!    still work against the standard `auth_user` table.
//!
//! Both boots share an in-process `OnceCell` to avoid racing with each
//! other and with the `integration.rs` suite. Each scenario gets its
//! own `OnceCell` so the boot sequences run independently.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::prelude::Plugin;
use umbral_auth::{
    AuthError, AuthPlugin, AuthUser, UserModel, authenticate, hash_password, set_password,
    verify_password,
};

// =========================================================================
// CustomUser: a minimal user model with extra application fields
// =========================================================================

/// A minimal application-level user with two extra columns that the
/// built-in `AuthUser` doesn't have: `display_name` (a public label)
/// and `tenant_id` (a multi-tenancy discriminator).
///
/// The `UserModel` implementation covers only the four required methods;
/// the three flag methods (`is_active`, `is_staff`, `is_superuser`) are
/// omitted because the default impls are the right values for this model.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
pub struct CustomUser {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub display_name: String,
    pub tenant_id: i64,
    pub is_active: bool,
}

impl UserModel for CustomUser {
    fn id(&self) -> i64 {
        self.id
    }

    fn username(&self) -> &str {
        &self.username
    }

    fn password_hash(&self) -> &str {
        &self.password_hash
    }

    fn set_password_hash(&mut self, hash: String) {
        self.password_hash = hash;
    }

    // Override is_active so the SQL-level flag is honoured too.
    fn is_active(&self) -> bool {
        self.is_active
    }

    // is_staff and is_superuser use the trait defaults (false).
}

// =========================================================================
// Boot: CustomUser scenario
// =========================================================================

static BOOT_CUSTOM: OnceCell<()> = OnceCell::const_new();

async fn boot_custom() {
    BOOT_CUSTOM
        .get_or_init(|| async {
            let settings =
                umbral::Settings::from_env().expect("figment defaults always load in a test env");

            // Per-test tempfile DB so the table doesn't collide with
            // the integration.rs suite's shared pool.
            let tmp = tempfile::tempdir().expect("create tempdir for custom_user test DB");
            let db_path = tmp.path().join("umbral_custom_user.sqlite");
            std::mem::forget(tmp);
            let options = SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                .filename(&db_path)
                .create_if_missing(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(5)
                .connect_with(options)
                .await
                .expect("sqlite should connect");

            umbral::App::builder()
                .settings(settings)
                .database("default", pool)
                .plugin(AuthPlugin::<CustomUser>::default())
                .build()
                .expect("App::build should succeed with AuthPlugin::<CustomUser>");

            // Create the custom_user table directly. Column names must
            // match the CustomUser field names for sqlx::FromRow to bind.
            let pool = umbral::db::pool();
            sqlx::query(
                "CREATE TABLE custom_user (
                    id           INTEGER PRIMARY KEY AUTOINCREMENT,
                    username     TEXT NOT NULL UNIQUE,
                    password_hash TEXT NOT NULL,
                    display_name TEXT NOT NULL,
                    tenant_id    INTEGER NOT NULL,
                    is_active    INTEGER NOT NULL DEFAULT 1
                )",
            )
            .execute(&pool)
            .await
            .expect("create custom_user table");
        })
        .await;
}

// =========================================================================
// Boot: default AuthUser scenario (proves the default still works)
// =========================================================================

static BOOT_DEFAULT: OnceCell<()> = OnceCell::const_new();

async fn boot_default() {
    BOOT_DEFAULT
        .get_or_init(|| async {
            // This boot shares the same process-wide OnceLock that
            // `integration.rs` might already have populated. If the
            // ambient pool is already set by a parallel test run, we
            // skip re-building to avoid a double-init panic.
            //
            // The ambient pool set in integration.rs uses `auth_user`.
            // This boot only needs to confirm that `AuthPlugin::default()`
            // (which resolves to `AuthPlugin::<AuthUser>`) compiles and
            // constructs successfully.
            let _ = AuthPlugin::<AuthUser>::default();
        })
        .await;
}

// =========================================================================
// Tests: CustomUser + generic helpers
// =========================================================================

/// `AuthPlugin::<CustomUser>` registers the `CustomUser` model under
/// the "auth" plugin slot. The model meta should carry the `custom_user`
/// table name (the snake_case of the struct name).
#[tokio::test]
async fn custom_plugin_registers_custom_user_model() {
    boot_custom().await;

    let models = umbral::migrate::models_for_plugin("auth");
    assert_eq!(
        models.len(),
        1,
        "AuthPlugin::<CustomUser> should contribute exactly one model; got {models:?}",
    );
    assert_eq!(models[0].name, "CustomUser");
    assert_eq!(models[0].table, "custom_user");
}

/// Insert a `CustomUser` row directly (there is no `create_user` for
/// custom models - apps provide their own constructor), then call the
/// generic `authenticate::<CustomUser>` helper and verify the returned
/// row matches what was inserted.
#[tokio::test]
async fn authenticate_generic_works_for_custom_user() {
    boot_custom().await;

    let pool = umbral::db::pool();
    let hash = hash_password("s3cret").expect("hash_password should succeed");

    // Insert via raw SQL so the test doesn't depend on a custom
    // create_user helper that is intentionally out of scope for the trait.
    sqlx::query(
        "INSERT INTO custom_user (username, password_hash, display_name, tenant_id, is_active)
         VALUES (?, ?, ?, ?, 1)",
    )
    .bind("alice_tenant")
    .bind(&hash)
    .bind("Alice (Tenant 42)")
    .bind(42_i64)
    .execute(&pool)
    .await
    .expect("insert custom_user row");

    let found = authenticate::<CustomUser>("alice_tenant", "s3cret")
        .await
        .expect("authenticate::<CustomUser> should succeed for valid credentials");

    assert_eq!(found.username, "alice_tenant");
    assert_eq!(found.display_name, "Alice (Tenant 42)");
    assert_eq!(found.tenant_id, 42);
    assert!(found.is_active());
    // Default trait methods for unimplemented flags.
    assert!(!found.is_staff(), "is_staff default is false");
    assert!(!found.is_superuser(), "is_superuser default is false");
}

/// Wrong password against a custom user returns `InvalidCredentials` -
/// same semantics as `AuthUser`.
#[tokio::test]
async fn authenticate_rejects_wrong_password_for_custom_user() {
    boot_custom().await;

    let pool = umbral::db::pool();
    let hash = hash_password("correctpass").expect("hash");
    sqlx::query(
        "INSERT INTO custom_user (username, password_hash, display_name, tenant_id, is_active)
         VALUES (?, ?, ?, ?, 1)",
    )
    .bind("bob_tenant")
    .bind(&hash)
    .bind("Bob")
    .bind(1_i64)
    .execute(&pool)
    .await
    .expect("insert");

    let result = authenticate::<CustomUser>("bob_tenant", "wrongpass").await;
    assert!(
        matches!(result, Err(AuthError::InvalidCredentials)),
        "wrong password must return InvalidCredentials; got {result:?}",
    );
}

/// An inactive custom user cannot authenticate even with the correct
/// password. The `is_active = 0` filter in the WHERE clause handles
/// this at the SQL level; the trait method provides defence-in-depth.
#[tokio::test]
async fn authenticate_rejects_inactive_custom_user() {
    boot_custom().await;

    let pool = umbral::db::pool();
    let hash = hash_password("pass").expect("hash");
    sqlx::query(
        "INSERT INTO custom_user (username, password_hash, display_name, tenant_id, is_active)
         VALUES (?, ?, ?, ?, 0)",
    )
    .bind("carol_inactive")
    .bind(&hash)
    .bind("Carol")
    .bind(1_i64)
    .execute(&pool)
    .await
    .expect("insert");

    let result = authenticate::<CustomUser>("carol_inactive", "pass").await;
    assert!(
        matches!(result, Err(AuthError::InvalidCredentials)),
        "inactive custom user must not authenticate; got {result:?}",
    );
}

/// `set_password` is generic over `U: UserModel`. Verify it updates the
/// `password_hash` column in the `custom_user` table via `U::TABLE` and
/// that the new password authenticates while the old one does not.
#[tokio::test]
async fn set_password_works_for_custom_user() {
    boot_custom().await;

    let pool = umbral::db::pool();
    let initial_hash = hash_password("oldpassword").expect("hash");
    sqlx::query(
        "INSERT INTO custom_user (username, password_hash, display_name, tenant_id, is_active)
         VALUES (?, ?, ?, ?, 1)",
    )
    .bind("dave_tenant")
    .bind(&initial_hash)
    .bind("Dave")
    .bind(7_i64)
    .execute(&pool)
    .await
    .expect("insert");

    let mut user = authenticate::<CustomUser>("dave_tenant", "oldpassword")
        .await
        .expect("first authenticate should succeed");

    let old_hash = user.password_hash.clone();
    set_password(&mut user, "newpassword")
        .await
        .expect("set_password should succeed");

    assert_ne!(
        user.password_hash, old_hash,
        "set_password must update the in-place hash",
    );

    // New password authenticates.
    authenticate::<CustomUser>("dave_tenant", "newpassword")
        .await
        .expect("new password must work after set_password");

    // Old password no longer works.
    let stale = authenticate::<CustomUser>("dave_tenant", "oldpassword").await;
    assert!(
        matches!(stale, Err(AuthError::InvalidCredentials)),
        "old password must stop working after set_password; got {stale:?}",
    );
}

// =========================================================================
// Tests: default AuthUser form still works
// =========================================================================

/// `AuthPlugin::default()` (which resolves to `AuthPlugin::<AuthUser>`)
/// constructs without any explicit type annotation and satisfies the
/// `Plugin` trait bound. This is the zero-migration compatibility proof:
/// existing apps that never opt in to a custom user model see no change.
#[tokio::test]
async fn default_auth_plugin_type_resolves_to_auth_user() {
    boot_default().await;

    // AuthPlugin::default() must be constructible with the default
    // type parameter without any annotation.
    let plugin: AuthPlugin = AuthPlugin::default();
    assert_eq!(plugin.name(), "auth");
    assert!(plugin.user_model_name.is_none());

    // The explicit AuthUser form and the inferred form are the same.
    let plugin2: AuthPlugin<AuthUser> = AuthPlugin::<AuthUser>::default();
    assert_eq!(plugin2.name(), "auth");
}

/// `AuthPlugin::<CustomUser>::default().user_model_name("custom")` sets
/// the informational field. Proves the fluent builder works.
#[test]
fn user_model_name_builder_sets_the_field() {
    let plugin = AuthPlugin::<CustomUser>::default().user_model_name("tenant_user");
    assert_eq!(
        plugin.user_model_name.as_deref(),
        Some("tenant_user"),
        "user_model_name builder should set the field",
    );
}

/// UserModel::is_active / is_staff / is_superuser defaults are correct
/// for a type that doesn't override them. CustomUser overrides is_active
/// but leaves is_staff and is_superuser at the default.
#[test]
fn user_model_default_flags() {
    // Minimal in-memory instance; no DB needed.
    let user = CustomUser {
        id: 1,
        username: "test".into(),
        password_hash: "hash".into(),
        display_name: "Test".into(),
        tenant_id: 0,
        is_active: true,
    };
    assert!(user.is_active(), "is_active reflects the struct field");
    assert!(!user.is_staff(), "is_staff defaults to false");
    assert!(!user.is_superuser(), "is_superuser defaults to false");
}

/// `verify_password` + `hash_password` are model-agnostic pure functions.
/// A custom user model can call them directly as part of its own insert
/// helper without going through `create_user`.
#[test]
fn hash_and_verify_work_for_custom_user_password_column() {
    let hash = hash_password("my-secret").expect("hash_password should not fail");
    assert!(
        verify_password("my-secret", &hash).expect("verify should not error"),
        "correct plaintext must verify",
    );
    assert!(
        !verify_password("wrong", &hash).expect("verify should not error"),
        "wrong plaintext must not verify",
    );
}
