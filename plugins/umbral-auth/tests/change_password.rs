//! gaps3 #20 — `umbral_auth::change_password` verifies the current password,
//! enforces the strength policy on the new one, then rotates the hash. Backs the
//! `POST {prefix}/change-password` default route.

use tokio::sync::OnceCell;
use umbral_auth::{AuthPlugin, AuthUser};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("umbral_change_password.sqlite");
        std::mem::forget(tmp);

        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
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

        // Default password policy ON (so weak-password rejection is exercised);
        // throttle off to avoid rate-limit interference.
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default().disable_throttle())
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

#[tokio::test]
async fn change_password_verifies_current_validates_new_and_rotates() {
    boot().await;
    let user = umbral_auth::create_user("dave", "dave@example.com", "Old$Passw0rd")
        .await
        .expect("create user");

    // Wrong current password → rejected, hash untouched.
    assert!(
        umbral_auth::change_password(&user, "WRONG-pass", "N3w$Passw0rd!")
            .await
            .is_err(),
        "a wrong current password must be rejected"
    );

    // Correct current but WEAK new password → rejected by the strength policy.
    assert!(
        umbral_auth::change_password(&user, "Old$Passw0rd", "123")
            .await
            .is_err(),
        "a weak new password must be rejected"
    );

    // Correct current + strong new → rotated. The new password authenticates,
    // the old one no longer does.
    umbral_auth::change_password(&user, "Old$Passw0rd", "N3w$Passw0rd!")
        .await
        .expect("change should succeed");

    let updated = AuthUser::objects()
        .filter(umbral_auth::auth_user::ID.eq(user.id))
        .first()
        .await
        .expect("query")
        .expect("user still exists");
    assert!(
        umbral_auth::verify_password("N3w$Passw0rd!", &updated.password_hash).unwrap(),
        "the new password authenticates"
    );
    assert!(
        !umbral_auth::verify_password("Old$Passw0rd", &updated.password_hash).unwrap(),
        "the old password no longer authenticates"
    );
}
