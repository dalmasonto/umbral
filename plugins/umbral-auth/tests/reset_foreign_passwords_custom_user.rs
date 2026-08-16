//! `reset_unverifiable_passwords` is generic over the user model, so an
//! `AuthPlugin<CustomUser>` neutralizes ITS users — not the built-in `AuthUser`.
//! A separate test binary because `App::build` is process-wide (one user model
//! per binary).

use tokio::sync::OnceCell;
use umbral_auth::{AuthPlugin, UserModel};

/// A minimal custom user model (the port target for a non-Django app).
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
pub struct CustomUser {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
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
    fn is_active(&self) -> bool {
        self.is_active
    }
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("umbral_reset_custom.sqlite");
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
            .plugin(AuthPlugin::<CustomUser>::default())
            .build()
            .expect("App::build with AuthPlugin<CustomUser>");
        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

async fn insert_user(id: i64, username: &str, password_hash: &str) {
    let mut row = serde_json::Map::new();
    row.insert("id".into(), serde_json::Value::from(id));
    row.insert("username".into(), serde_json::Value::from(username));
    row.insert(
        "password_hash".into(),
        serde_json::Value::from(password_hash),
    );
    row.insert("is_active".into(), serde_json::Value::from(true));
    umbral::orm::DynQuerySet::for_meta(&umbral::migrate::ModelMeta::for_::<CustomUser>())
        .presealed()
        .insert_json(&row)
        .await
        .expect("insert custom user");
}

#[tokio::test]
async fn resetforeignpasswords_targets_the_plugins_custom_user_model() {
    boot().await;

    // A native (argon2) hash and a foreign (Django pbkdf2) one.
    let native_hash = umbral_auth::hash_password("Real$Passw0rd").expect("hash");
    insert_user(1, "native", &native_hash).await;
    let django_hash = "pbkdf2_sha256$260000$abc123$Zm9vYmFyYmF6cXV4";
    insert_user(2, "foreign", django_hash).await;

    let audit = umbral_auth::reset_unverifiable_passwords::<CustomUser>()
        .await
        .expect("neutralize custom users");
    assert_eq!(audit.total, 2, "both custom users scanned");
    assert_eq!(audit.reset, 1, "only the foreign hash reset");

    let users = umbral::orm::Manager::<CustomUser>::default()
        .fetch()
        .await
        .expect("fetch");
    let native = users.iter().find(|u| u.id == 1).unwrap();
    let foreign = users.iter().find(|u| u.id == 2).unwrap();

    // Native hash untouched and still verifies; foreign now a valid argon2 hash
    // of an unknown password (login cleanly false, recovers via reset).
    assert_eq!(native.password_hash, native_hash);
    assert!(umbral_auth::verify_password("Real$Passw0rd", &native.password_hash).unwrap());
    assert_ne!(foreign.password_hash, django_hash);
    assert!(
        !umbral_auth::verify_password("anything", &foreign.password_hash)
            .expect("neutralized hash parses as argon2"),
    );
}
