//! Proof point for the polymorphic-`UserModel` refactor (gap #72
//! follow-up).
//!
//! `UuidUser` is a user model keyed by `uuid::Uuid` instead of
//! `i64`. The refactor makes `UserModel::id()` return
//! `<Self as Model>::PrimaryKey` (the typed PK from the derive),
//! adds `id_string()` with a `Display`-backed default, and threads
//! the typed PK through `resolve_user` / `current_session_user_pk`
//! via a `<U::PrimaryKey as FromStr>` bound. None of those paths
//! parse to `i64` anywhere in the framework anymore.
//!
//! What this test pins:
//!
//! - `UuidUser::id()` returns `uuid::Uuid` and the
//!   `<Self as Model>::PrimaryKey` shape matches.
//! - `UuidUser::id_string()` produces the canonical UUID string
//!   form via the trait default (no hand override needed).
//! - `current_session_user_pk::<UuidUser>` parses the stored text
//!   `session.user_id` back to a typed `uuid::Uuid` — round-trips
//!   exactly without going through `i64`.
//! - `resolve_user::<UuidUser>` queries with the typed PK
//!   (`Predicate::<UuidUser>::col_eq("id", uuid)`) and returns the
//!   hydrated `UuidUser` row.
//! - `set_password::<UuidUser>` writes through to the correct
//!   uuid-keyed row.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbral::orm::Model;
use umbral_auth::login_required::{current_session_user_pk, resolve_user};
use umbral_auth::{AuthPlugin, UserModel, hash_password, set_password};
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
pub struct UuidUser {
    pub id: uuid::Uuid,
    pub username: String,
    pub password_hash: String,
    pub is_active: bool,
}

impl UserModel for UuidUser {
    // Typed PK accessor — the framework no longer hardcodes i64.
    fn id(&self) -> <Self as Model>::PrimaryKey {
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

    // `id_string()` left as the trait default — uses Display from
    // `PrimaryKey`, which `uuid::Uuid` implements as the canonical
    // 8-4-4-4-12 hyphenated form.
}

// =========================================================================
// Boot — fresh SQLite DB with auth + uuid_user schemas.
// =========================================================================

static BOOT_UUID: OnceCell<()> = OnceCell::const_new();

async fn boot_uuid() {
    BOOT_UUID
        .get_or_init(|| async {
            // File-backed temp DB instead of `:memory:` — sqlx's
            // pool drops idle connections across tokio test
            // runtimes, and a fresh connection to `:memory:` is a
            // fresh empty database. A tempfile gives us a single
            // persistent on-disk SQLite that survives connection
            // churn for the lifetime of the test process.
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("uuid_user_test.sqlite");
            std::mem::forget(tmp);

            let pool = SqlitePoolOptions::new()
                .max_connections(5)
                .connect_with(
                    SqliteConnectOptions::new()
                        .busy_timeout(std::time::Duration::from_secs(5))
                        .filename(&path)
                        .create_if_missing(true),
                )
                .await
                .unwrap();

            let settings = umbral::Settings::from_env().unwrap();
            let app = umbral::App::builder()
                .settings(settings)
                .database("default", pool)
                .plugin(umbral_sessions::SessionsPlugin::default().without_auto_layer())
                .plugin(AuthPlugin::<UuidUser>::default())
                .build()
                .expect("App::build with UuidUser");

            umbral::migrate::create_tables_for_tests()
                .await
                .expect("create the test schema");

            // The auth plugin's migrations create `auth_user`; we
            // need `uuid_user` and `session` too. Build them by
            // hand — simpler than threading the migration engine
            // through a test.

            // Hold the App alive past this scope so its ambient
            // pool registration sticks for the duration of the
            // test process.
            std::mem::forget(app);
        })
        .await;
}

// =========================================================================
// Helpers — mint a uuid user and a session row pointing at it.
// =========================================================================

async fn seed_user() -> UuidUser {
    let user = UuidUser {
        id: Uuid::new_v4(),
        username: format!("alice-{}", Uuid::new_v4()),
        password_hash: hash_password("hunter2").expect("hash"),
        is_active: true,
    };
    UuidUser::objects()
        .create(user)
        .await
        .expect("insert UuidUser")
}

/// Write a session row directly via SQL — the cookie is the
/// plaintext, the row stores SHA-256 of it. The session helpers
/// then resolve back via the cookie.
async fn seed_session_for(user: &UuidUser) -> String {
    use sha2::{Digest, Sha256};

    let plaintext = format!("test-token-{}", Uuid::new_v4().simple());
    let mut h = Sha256::new();
    h.update(plaintext.as_bytes());
    let stored_id = format!("{:x}", h.finalize());

    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::hours(1);
    let pool = umbral::db::pool();
    sqlx::query(
        "INSERT INTO session (id, user_id, data, created_at, expires_at) \
         VALUES (?, ?, '{}', ?, ?)",
    )
    .bind(&stored_id)
    // Store the UUID's canonical Display form — `id_string()`
    // would do the same, but writing the SQL by hand keeps the
    // test honest about what the row actually contains.
    .bind(user.id.to_string())
    .bind(now.to_rfc3339())
    .bind(expires_at.to_rfc3339())
    .execute(&pool)
    .await
    .expect("insert session");
    plaintext
}

fn header_map_with_cookie(plaintext: &str) -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::COOKIE,
        format!("umbral_session={plaintext}").parse().unwrap(),
    );
    headers
}

// =========================================================================
// The assertions.
// =========================================================================

#[tokio::test]
async fn id_returns_typed_uuid_and_id_string_matches_display() {
    boot_uuid().await;
    let user = seed_user().await;
    // Typed PK accessor stays UUID-shaped.
    let typed: uuid::Uuid = UserModel::id(&user);
    assert_eq!(typed, user.id);
    // The trait default uses Display from PrimaryKey — for UUID
    // that's the hyphenated 8-4-4-4-12 form.
    assert_eq!(UserModel::id_string(&user), user.id.to_string());
    assert_eq!(UserModel::id_string(&user).len(), 36);
}

#[tokio::test]
async fn current_session_user_pk_parses_uuid_without_going_through_i64() {
    boot_uuid().await;
    let user = seed_user().await;
    let cookie = seed_session_for(&user).await;
    let headers = header_map_with_cookie(&cookie);

    // The session text round-trips back into a typed `uuid::Uuid`
    // via the polymorphic helper. No `parse::<i64>()` step.
    let pk: Option<uuid::Uuid> = current_session_user_pk::<UuidUser>(&headers).await;
    assert_eq!(pk, Some(user.id));
}

#[tokio::test]
async fn resolve_user_hydrates_a_uuid_keyed_user_row() {
    boot_uuid().await;
    let user = seed_user().await;
    let cookie = seed_session_for(&user).await;
    let headers = header_map_with_cookie(&cookie);

    let hydrated: UuidUser = resolve_user::<UuidUser>(&headers)
        .await
        .expect("resolve_user should find the uuid-keyed row");
    assert_eq!(hydrated.id, user.id);
    assert_eq!(hydrated.username, user.username);
}

#[tokio::test]
async fn set_password_updates_the_correct_uuid_keyed_row() {
    boot_uuid().await;
    let mut user = seed_user().await;
    let original_hash = user.password_hash.clone();
    set_password::<UuidUser>(&mut user, "fresh-password")
        .await
        .expect("set_password against UuidUser");
    assert_ne!(user.password_hash, original_hash);
    // Re-read from the DB to confirm the WHERE clause used the
    // typed UUID PK, not some accidentally-stringified version.
    let reread = UuidUser::objects()
        .filter(umbral::orm::Predicate::<UuidUser>::col_eq("id", user.id))
        .first()
        .await
        .expect("re-read")
        .expect("row still exists");
    assert_eq!(reread.password_hash, user.password_hash);
}
