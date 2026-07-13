//! `#[umbral(trim, lowercase)]` applies on EVERY write path (features #83).
//!
//! It didn't. The declarative normalizers were honoured on the **dynamic** path —
//! REST, the admin, forms — and silently ignored on the **typed** one, so
//! `Model::objects().create(u)` stored `"  Ada@Example.COM  "` verbatim while the
//! identical value through REST became `ada@example.com`.
//!
//! That is a data-integrity bug, not a cosmetic one. A user who signs up through
//! the API and one seeded by a script or a background job end up as two different
//! rows for one human; add a case-insensitive unique index later and a legitimate
//! signup starts failing. Declaring the rule on the field has to mean every write
//! path obeys it, or the declaration is a lie — and the framework's own docs
//! promise "declare it once and every write path normalises".

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "nz_user")]
pub struct NzUser {
    pub id: i64,
    #[umbral(trim, lowercase)]
    pub email: String,
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("nz.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<NzUser>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

const MESSY: &str = "  Ada@Example.COM  ";
const CLEAN: &str = "ada@example.com";

/// The typed path — `Model::objects().create()`. This is the one that was broken.
#[tokio::test]
async fn typed_create_normalizes() {
    let _g = lock().lock().await;
    boot().await;
    let row = NzUser::objects()
        .create(NzUser {
            id: 0,
            email: MESSY.into(),
        })
        .await
        .expect("create");
    assert_eq!(
        row.email, CLEAN,
        "`Model::objects().create()` must honour #[umbral(trim, lowercase)] — REST \
         already did, and a field that normalizes only for SOME writers is how one \
         human ends up as two rows",
    );
}

/// `bulk_create` must not become the path that skips it.
#[tokio::test]
async fn typed_bulk_create_normalizes() {
    let _g = lock().lock().await;
    boot().await;
    let n = NzUser::objects()
        .bulk_create(vec![NzUser {
            id: 0,
            email: "  BOB@Example.COM ".into(),
        }])
        .await
        .expect("bulk_create");
    assert_eq!(n, 1);

    let stored: (String,) =
        sqlx::query_as("SELECT email FROM nz_user WHERE email LIKE '%bob%' OR email LIKE '%BOB%'")
            .fetch_one(&umbral::db::pool())
            .await
            .expect("read back");
    assert_eq!(stored.0, "bob@example.com", "bulk_create normalizes too");
}

/// An UPDATE must normalize as well — otherwise a field arrives clean on create
/// and dirty on edit, which is the same bug wearing a different hat.
#[tokio::test]
async fn typed_update_normalizes() {
    let _g = lock().lock().await;
    boot().await;
    let row = NzUser::objects()
        .create(NzUser {
            id: 0,
            email: "seed@x.com".into(),
        })
        .await
        .expect("create");

    NzUser::objects()
        .filter(nz_user::ID.eq(row.id))
        .update_values(
            serde_json::json!({"email": "  Cleo@Example.COM  "})
                .as_object()
                .unwrap()
                .clone(),
        )
        .await
        .expect("update");

    let stored: (String,) = sqlx::query_as("SELECT email FROM nz_user WHERE id = ?")
        .bind(row.id)
        .fetch_one(&umbral::db::pool())
        .await
        .expect("read back");
    assert_eq!(stored.0, "cleo@example.com", "update normalizes too");
}

/// The dynamic path (REST / admin) already worked — pinned so a refactor can't
/// silently take it away while "fixing" the typed one.
#[tokio::test]
async fn the_dynamic_path_still_normalizes() {
    let _g = lock().lock().await;
    boot().await;
    let meta = umbral::migrate::ModelMeta::for_::<NzUser>();
    let row = umbral::orm::DynQuerySet::for_meta(&meta)
        .insert_json(
            serde_json::json!({"email": "  Dan@Example.COM  "})
                .as_object()
                .unwrap(),
        )
        .await
        .expect("dyn insert");
    assert_eq!(row["email"], serde_json::json!("dan@example.com"));
}
