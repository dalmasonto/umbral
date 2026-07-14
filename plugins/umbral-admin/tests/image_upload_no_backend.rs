#![allow(dead_code)]
//! gaps2 #36 — graceful degradation when no storage backend is installed.
//!
//! Boots an admin router with NO StoragePlugin (and no `set_storage`). A staff
//! POST to `/admin/upload-image` must then return the documented `409
//! Conflict` JSON error rather than panicking — the editor surfaces it
//! through `onError`. This lives in its own test binary because the ambient
//! pool + storage globals are set-once per process: it can't coexist with a
//! backend-installed router in the same binary.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_admin::{AdminModel, AdminPlugin};
use umbral_auth::{AuthPlugin, AuthUser, create_user};
use umbral_sessions::SessionsPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Note {
    id: i64,
    title: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("no_backend.sqlite");
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
            .expect("pool");

        // NOTE: no MemMediaPlugin / set_storage — storage stays unregistered.
        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default().register(AdminModel::new("note")))
            .model::<Note>()
            .build()
            .expect("build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();

        let staff = create_user("nb_admin", "nb@example.com", "pass123")
            .await
            .expect("user");
        sqlx::query("UPDATE auth_user SET is_staff = 1, is_superuser = 1 WHERE id = ?")
            .bind(staff.id)
            .execute(&pool)
            .await
            .expect("set staff");

        app.into_router()
    })
    .await
}

const BOUNDARY: &str = "X-UMBRAL-EDITOR-UPLOAD";

fn multipart_image(field: &str, filename: &str, ct: &str, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    out.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    out.extend_from_slice(format!("Content-Type: {ct}\r\n\r\n").as_bytes());
    out.extend_from_slice(value);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    out
}

fn extract_csrf(html: &str) -> String {
    let marker = r#"name="csrf_token""#;
    let pos = html.find(marker).unwrap_or(0);
    let window = &html[pos..(pos + 200).min(html.len())];
    let val = r#"value=""#;
    let vpos = window.find(val).unwrap_or(0);
    let after = &window[vpos + val.len()..];
    after[..after.find('"').unwrap_or(0)].to_string()
}

async fn login(router: axum::Router) -> String {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("get login");
    let csrf_cookie = resp
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|s| {
            let first = s.split(';').next()?;
            let (k, v) = first.split_once('=')?;
            if k.trim() == "umbral_csrf_token" {
                Some(v.to_string())
            } else {
                None
            }
        })
        .expect("csrf cookie");
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let csrf = extract_csrf(&String::from_utf8_lossy(&bytes));
    let form = serde_urlencoded::to_string([
        ("username", "nb_admin"),
        ("password", "pass123"),
        ("csrf_token", csrf.as_str()),
        ("next", "/admin/"),
    ])
    .unwrap();
    let resp2 = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, format!("umbral_csrf_token={csrf_cookie}"))
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .expect("post login");
    resp2
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|s| {
            let first = s.split(';').next()?;
            let (k, v) = first.split_once('=')?;
            if k.trim() == "umbral_session" {
                Some(v.to_string())
            } else {
                None
            }
        })
        .expect("session cookie")
}

/// No storage backend installed → staff upload yields the documented 409.
#[tokio::test]
async fn no_storage_backend_returns_conflict() {
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    let png = b"\x89PNG\r\n\x1a\nORPHAN";
    let body = multipart_image("image", "z.png", "image/png", png);

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/upload-image")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .header(header::COOKIE, format!("umbral_session={session}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("oneshot");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let text = String::from_utf8_lossy(&bytes);

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "no storage backend must yield 409; body={text}"
    );
    let json: serde_json::Value = serde_json::from_str(&text).expect("JSON error");
    assert!(
        json["error"]
            .as_str()
            .unwrap_or_default()
            .contains("storage backend"),
        "error mentions the missing storage backend, got {text}"
    );
}

/// audit_2 admin #6 — a payload declared `image/png` whose bytes are NOT a PNG
/// (here, an HTML/script blob) must be rejected with 415 by the magic-byte
/// sniff BEFORE the storage path, not accepted on the declared type alone.
#[tokio::test]
async fn declared_png_with_non_png_bytes_is_rejected() {
    let router = boot().await.clone();
    let session = login(router.clone()).await;

    let not_png = b"<script>alert(document.cookie)</script>";
    let body = multipart_image("image", "evil.png", "image/png", not_png);

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/upload-image")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .header(header::COOKIE, format!("umbral_session={session}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("oneshot");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    let text = String::from_utf8_lossy(&bytes);

    assert_eq!(
        status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "content/declared-type mismatch must be 415, not reach storage; body={text}"
    );
    assert!(
        text.contains("does not match its declared image type"),
        "error explains the content mismatch, got {text}"
    );
}
