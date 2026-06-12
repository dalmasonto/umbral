//! `MediaPlugin` serves an uploaded file at `<mount>/<key>`.
//!
//! Regression test: `routes()` used `Router::route("/media/{*path}", …)`,
//! which does NOT strip the mount prefix, so `ServeDir` resolved
//! `/media/<key>` under `<dir>/media/<key>` and 404'd every upload.
//! `nest_service` strips the prefix, so `/media/<key>` → `<dir>/<key>`.

use std::fs;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use umbra::prelude::Plugin;
use umbra_media::MediaPlugin;

#[tokio::test]
async fn serves_an_uploaded_file_at_mount_slash_key() {
    let dir = tempfile::tempdir().expect("tmp dir");
    // A stored file lives directly under the media dir, keyed by the
    // storage key — exactly what `FsStorage::store` writes.
    fs::write(dir.path().join("abc-logo.png"), b"PNGDATA").unwrap();

    let app = MediaPlugin::new("/media", dir.path()).routes();

    // The uploaded file resolves at `/media/<key>` (the URL FileField::url
    // hands the browser).
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
    let body = axum::body::to_bytes(res.into_body(), 1 << 16)
        .await
        .unwrap();
    assert_eq!(&body[..], b"PNGDATA");

    // A missing key is a clean 404 (not a panic, not a 500).
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
