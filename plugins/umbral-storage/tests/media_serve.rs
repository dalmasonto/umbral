//! `StoragePlugin` (media side) serves an uploaded file at `<mount>/<key>`.
//! Moved from umbral-media.

use std::fs;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbral::prelude::Plugin;
use umbral_storage::StoragePlugin;

#[tokio::test]
async fn serves_an_uploaded_file_at_mount_slash_key() {
    let dir = tempfile::tempdir().expect("tmp dir");
    fs::write(dir.path().join("abc-logo.png"), b"PNGDATA").unwrap();

    let app = StoragePlugin::new().media("/media", dir.path()).routes();

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/media/abc-logo.png")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "an uploaded file must serve at /media/<key>"
    );
    let body = axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap();
    assert_eq!(&body[..], b"PNGDATA");

    let res = app
        .oneshot(
            Request::builder()
                .uri("/media/nope.png")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
