//! A UUID/String-keyed user model must not be silently forbidden from everything
//! (gaps3 #59).
//!
//! `WithPermissions::authenticate` probed the user row with
//! `identity.user_id.parse::<i64>()` and, on failure, set `(is_active, is_superuser) =
//! (false, false)`. The built-in `AuthUser` keys by `i64`; a custom `UserModel` may key
//! by `String` or `Uuid`, and its id will NEVER parse as an `i64` — so every request from
//! such a user landed in that arm.
//!
//! Two things then happened, and together they are total:
//!
//! 1. `extras["is_active"] = false` → `HasPermission::check` returns Forbidden before it
//!    looks at a single codename.
//! 2. The `if is_active && !is_superuser` guard skipped populating
//!    `extras["permissions"]` — so even the codename path had nothing to match.
//!
//! Net: **every REST route gated by a permission returned 403 to every non-i64-keyed
//! user, permanently and silently.** Superusers included. Nothing logged, nothing
//! errored — the framework simply denied them forever.
//!
//! The grant machinery was never the problem: `grant_user_permission(user_id: &str, ..)`
//! and `user_perms(&str)` have always been string-keyed. One line switched them off.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::auth::{Authentication, Identity};
use umbral::web::HeaderMap;
use umbral_permissions::models::permission;
use umbral_permissions::rest::{HasPermission, WithPermissions};
use umbral_permissions::{Permission, PermissionsPlugin};
use umbral_rest::permission::{Action, Permission as _, PermissionError};

/// The id a `Uuid`-keyed custom user model would carry. It cannot parse as an `i64`,
/// which is the entire point.
const UUID_USER: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(plugin = "blog")]
pub struct NpPost {
    pub id: i64,
    pub title: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("np.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");
        let settings = umbral::Settings::from_env().expect("figment defaults");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<NpPost>()
            .plugin(PermissionsPlugin)
            .build()
            .expect("App::build");
        let migration_dir = tempfile::tempdir().expect("tempdir");
        let migration_dir_path = migration_dir.path().to_path_buf();
        std::mem::forget(migration_dir);
        umbral::migrate::make_in(&migration_dir_path)
            .await
            .expect("makemigrations");
        umbral::migrate::run_in(&migration_dir_path)
            .await
            .expect("migrate");
        umbral_permissions::seed_standard_permissions_for_tests()
            .await
            .expect("seed standard permissions");
    })
    .await;
}

/// An authenticator standing in for a custom `UserModel` with a UUID primary key.
#[derive(Clone)]
struct UuidKeyedAuth;

#[async_trait::async_trait]
impl Authentication for UuidKeyedAuth {
    async fn authenticate(&self, _headers: &HeaderMap) -> Option<Identity> {
        Some(Identity {
            user_id: UUID_USER.to_string(),
            is_staff: false,
            is_superuser: false,
            extras: Default::default(),
        })
    }
}

#[tokio::test]
async fn a_uuid_keyed_user_keeps_the_permissions_it_was_granted() {
    boot().await;

    // Grant a real codename to the UUID user. This side has always been string-keyed.
    let perm = Permission::objects()
        .filter(permission::CODENAME.eq("blog.change_nppost"))
        .first()
        .await
        .expect("lookup")
        .expect("standard permission must exist");
    umbral_permissions::grant_user_permission(UUID_USER, &perm)
        .await
        .expect("grant");

    let identity = WithPermissions::new(UuidKeyedAuth)
        .authenticate(&HeaderMap::new())
        .await
        .expect("the user is authenticated");

    // (1) It must NOT be branded inactive. `is_active` is *unknowable* here — this is not
    // an AuthUser and the plugin cannot read the app's user model — so the key is absent,
    // which `check` documents as benefit-of-the-doubt. An explicit `false` was the bug.
    assert_ne!(
        identity.extras.get("is_active"),
        Some(&serde_json::Value::Bool(false)),
        "a non-i64 user id was branded INACTIVE — this alone 403s every gated route"
    );

    // (2) Its codenames must be populated, or the codename path has nothing to match.
    let perms = identity
        .extras
        .get("permissions")
        .and_then(|v| v.as_array())
        .expect("permissions must be populated for a user whose flags are unknowable");
    assert!(
        perms.iter().any(|p| p == "blog.change_nppost"),
        "the granted codename is missing: {perms:?}"
    );

    // (3) And the end-to-end claim: the gate actually opens.
    HasPermission::new("blog.change_nppost")
        .check(&Action::Update, Some(&identity))
        .expect("a UUID-keyed user WITH the permission must be allowed through");
}

/// Fail-open check: the fix must not hand a UUID user permissions it was never granted.
/// "Stop denying everyone" is only correct if it does not become "allow everyone".
#[tokio::test]
async fn a_uuid_keyed_user_is_still_denied_what_it_was_not_granted() {
    boot().await;

    let identity = WithPermissions::new(UuidKeyedAuth)
        .authenticate(&HeaderMap::new())
        .await
        .expect("authenticated");

    let err = HasPermission::new("blog.delete_nppost")
        .check(&Action::Delete, Some(&identity))
        .expect_err("no grant for delete — must still be denied");
    assert!(matches!(err, PermissionError::Forbidden), "{err:?}");

    // ...and it is not a superuser just because we could not read its row.
    assert_eq!(
        identity.extras.get("is_superuser"),
        Some(&serde_json::Value::Bool(false)),
        "an unreadable user row must never be treated as a superuser"
    );
}
