//! gaps4 #57: owner-only media gating. The `MediaFile` row carries an
//! `owner` column, `set_media_owner` stamps it, and `media_access_owner()`
//! resolves the caller (via the app-wide auth backend, gaps4 #42) and
//! serves a file only to its owner.
//!
//! Own test binary: it boots a full `App` (process-global OnceLocks — one
//! build per binary) because the owner gate does an ORM lookup.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral_storage::{MediaFile, StoragePlugin, set_media_owner};

static BOOT: OnceCell<(axum::Router, std::path::PathBuf)> = OnceCell::const_new();

async fn boot() -> &'static (axum::Router, std::path::PathBuf) {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let dbtmp = tempfile::tempdir().expect("tempdir");
        let db_path = dbtmp.path().join("media_owner.sqlite");
        std::mem::forget(dbtmp);
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

        // The media dir — a real path the FS backend serves from.
        let media_dir = tempfile::tempdir().expect("media tempdir");
        let media_path = media_dir.path().to_path_buf();
        std::mem::forget(media_dir);
        std::fs::write(media_path.join("secret.pdf"), b"OWNED-BYTES").unwrap();

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            // The ambient auth backend the owner gate resolves the caller
            // through (gaps4 #42): `x-user` header → i64 identity.
            .authentication(umbral::auth::FnAuthentication::new(
                |headers: umbral::web::HeaderMap| async move {
                    let uid: i64 = headers.get("x-user")?.to_str().ok()?.parse().ok()?;
                    Some(umbral::auth::Identity::user(uid))
                },
            ))
            .plugin(
                StoragePlugin::new()
                    .media("/media", &media_path)
                    .media_access_owner(),
            )
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create schema");

        // A tracked media_file row for the on-disk file, then stamp user 7
        // as its owner (the one line an app writes at its upload site).
        MediaFile::objects()
            .create(MediaFile {
                id: 0,
                key: "secret.pdf".into(),
                filename: "secret.pdf".into(),
                content_type: "application/pdf".into(),
                size: 11,
                uploaded_at: chrono::Utc::now(),
                status: "ready".into(),
                owner: None,
            })
            .await
            .expect("create media_file row");
        let stamped = set_media_owner("secret.pdf", "7")
            .await
            .expect("stamp owner");
        assert!(stamped, "the row existed, so stamping returns true");

        (app.into_router(), media_path)
    })
    .await
}

async fn get_as(user: Option<i64>) -> StatusCode {
    let (router, _) = boot().await;
    let mut req = Request::builder().uri("/media/secret.pdf");
    if let Some(u) = user {
        req = req.header("x-user", u.to_string());
    }
    router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .expect("request")
        .status()
}

#[tokio::test]
async fn only_the_owner_may_fetch_the_file() {
    // The owner (user 7) is served.
    assert_eq!(get_as(Some(7)).await, StatusCode::OK, "owner is served");
    // A different authenticated user is denied.
    assert_eq!(
        get_as(Some(8)).await,
        StatusCode::FORBIDDEN,
        "a non-owner is denied"
    );
    // Anonymous is denied.
    assert_eq!(
        get_as(None).await,
        StatusCode::FORBIDDEN,
        "an anonymous caller is denied"
    );
}

#[tokio::test]
async fn set_media_owner_reports_a_missing_row() {
    boot().await; // ensure the pool is installed
    let stamped = set_media_owner("does-not-exist.png", "7")
        .await
        .expect("no error");
    assert!(!stamped, "no media_file row for the key → Ok(false)");
}
