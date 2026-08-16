//! Porting caveat: a database migrated from Django (or any stack) carries
//! password hashes umbral-auth can't verify. `reset_unverifiable_passwords`
//! neutralizes them into valid argon2 hashes of unknown passwords, so those
//! accounts cleanly fail login and recover via the password-reset flow — while
//! umbral-native hashes are left untouched.

use tokio::sync::OnceCell;
use umbral_auth::{AuthPlugin, AuthUser};

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("umbral_reset_foreign.sqlite");
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

/// Force a user's stored hash to an arbitrary (foreign-format) string.
async fn set_raw_hash(id: i64, hash: &str) {
    let mut patch = serde_json::Map::new();
    patch.insert(
        "password_hash".to_string(),
        serde_json::Value::String(hash.to_string()),
    );
    umbral::orm::Manager::<AuthUser>::default()
        .filter(umbral::orm::Predicate::<AuthUser>::col_eq("id", id))
        .update_values(patch)
        .await
        .expect("update hash");
}

#[tokio::test]
async fn neutralizes_only_foreign_hashes_and_leaves_umbral_hashes() {
    boot().await;

    // A user with a real umbral (argon2) hash...
    let native = umbral_auth::create_user("native", "native@example.com", "Real$Passw0rd")
        .await
        .expect("create native user");
    // ...and one whose hash we replace with a Django pbkdf2 string (a hash umbral
    // can't even parse — `verify_password` errors on it).
    let foreign = umbral_auth::create_user("foreign", "foreign@example.com", "Temp$Passw0rd")
        .await
        .expect("create foreign user");
    let django_hash = "pbkdf2_sha256$260000$abc123$Zm9vYmFyYmF6cXV4"; // not a PHC argon2 string
    set_raw_hash(foreign.id, django_hash).await;

    // Precondition: umbral can't verify the foreign hash — it ERRORS (can't parse).
    assert!(
        umbral_auth::verify_password("anything", django_hash).is_err(),
        "a Django hash must be unverifiable (parse error) before neutralization",
    );

    let audit = umbral_auth::reset_unverifiable_passwords()
        .await
        .expect("neutralize");
    assert_eq!(audit.total, 2, "both users scanned");
    assert_eq!(audit.reset, 1, "only the foreign hash reset");

    // Reload both users.
    let users = umbral::orm::Manager::<AuthUser>::default()
        .fetch()
        .await
        .expect("fetch users");
    let native_now = users.iter().find(|u| u.id == native.id).unwrap();
    let foreign_now = users.iter().find(|u| u.id == foreign.id).unwrap();

    // The native hash is untouched and still verifies its real password.
    assert_eq!(native_now.password_hash, native.password_hash);
    assert!(
        umbral_auth::verify_password("Real$Passw0rd", &native_now.password_hash).unwrap(),
        "the umbral-native user's password must still work",
    );

    // The foreign user now holds a valid argon2 hash of an unknown password:
    // login cleanly returns `false` (NOT an error), so the account can recover
    // through the password-reset flow.
    assert_ne!(foreign_now.password_hash, django_hash);
    assert!(
        !umbral_auth::verify_password("Temp$Passw0rd", &foreign_now.password_hash)
            .expect("neutralized hash must be a parseable argon2 hash, so verify is Ok(false)"),
        "no known password matches the random hash",
    );

    // Idempotent: a second run neutralizes nothing (all hashes now umbral-valid).
    let again = umbral_auth::reset_unverifiable_passwords()
        .await
        .expect("re-run");
    assert_eq!(again.reset, 0, "re-run is a no-op");
}
