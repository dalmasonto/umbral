//! `CookieStore` — stateless, AEAD-encrypted session-in-cookie (Phase 2b).
//!
//! Covers:
//!   (a) round-trip: `save` -> cookie value -> `load` returns the same record;
//!   (b) tamper detection: flip a byte in the cookie -> `load` -> `None`;
//!   (c) lazy expiry: a past-due record -> `load` -> `None`;
//!   (d) size limit: oversized data -> `save` errors with `CookieTooLarge`;
//!   (e) end-to-end through `session_layer` with
//!       `SessionsPlugin::default().store(CookieStore::new())`: a writing
//!       handler emits a Set-Cookie carrying the encrypted blob, ZERO DB rows
//!       are created (the DB is never touched), and a second request carrying
//!       that cookie sees the written data.
//!
//! Own test binary (own ambient pool + store `OnceLock`s) so the
//! zero-rows assertion in (e) isn't polluted by sibling suites.

use chrono::{Duration, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use umbral::web::header;
use umbral_sessions::store::{SessionRecord, SessionStore};
use umbral_sessions::{COOKIE_NAME, CookieStore, Session, SessionError, SessionsPlugin};

/// A fixed secret so the key is deterministic across calls within a test.
const TEST_SECRET: &str = "cookie-store-test-secret-key-abc123";

fn record(user_id: Option<&str>, data: &str) -> SessionRecord {
    let now = Utc::now();
    SessionRecord {
        user_id: user_id.map(str::to_string),
        data: data.to_string(),
        created_at: now,
        expires_at: now + Duration::seconds(3600),
    }
}

// (a) Round-trip: save produces a cookie value; load of it returns the record.
#[tokio::test]
async fn save_then_load_round_trips_the_record() {
    let store = CookieStore::with_secret(TEST_SECRET);
    let rec = record(Some("user-7"), r#"{"cart":[1,2,3]}"#);

    let cookie = store.save("ignored-token", &rec).await.expect("save");
    assert!(
        !cookie.is_empty(),
        "save must produce a non-empty cookie value"
    );

    let loaded = store
        .load(&cookie)
        .await
        .expect("load ok")
        .expect("a freshly-saved cookie must load");

    assert_eq!(loaded.user_id, Some("user-7".to_string()));
    assert_eq!(loaded.data, r#"{"cart":[1,2,3]}"#);
}

// The token argument is irrelevant for a stateless store: the cookie IS the
// session, so loading with a different token still recovers the record.
#[tokio::test]
async fn load_ignores_the_token_argument() {
    let store = CookieStore::with_secret(TEST_SECRET);
    let cookie = store
        .save("token-A", &record(None, r#"{"k":1}"#))
        .await
        .expect("save");

    let loaded = store.load(&cookie).await.expect("load").expect("present");
    assert_eq!(loaded.data, r#"{"k":1}"#);
}

// (b) Tamper detection: flipping a byte in the cookie value fails the AEAD
// auth tag -> load returns None (treated as no session, not an error).
#[tokio::test]
async fn tampered_cookie_loads_as_none() {
    let store = CookieStore::with_secret(TEST_SECRET);
    let cookie = store
        .save("tok", &record(Some("victim"), r#"{"admin":false}"#))
        .await
        .expect("save");

    // Flip one character near the end (inside the ciphertext/tag region).
    let mut bytes = cookie.into_bytes();
    let last = bytes.len() - 1;
    bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(bytes).unwrap();

    let loaded = store.load(&tampered).await.expect("load is Ok, not Err");
    assert!(
        loaded.is_none(),
        "a tampered/forged cookie must load as None, never as a forged session",
    );
}

// A cookie encrypted under a DIFFERENT key must not decrypt — another arm of
// forgery resistance (you can't lift a session from one app's key to another).
#[tokio::test]
async fn cookie_from_another_key_loads_as_none() {
    let writer = CookieStore::with_secret("key-one");
    let reader = CookieStore::with_secret("key-two");
    let cookie = writer
        .save("tok", &record(Some("u"), "{}"))
        .await
        .expect("save");

    assert!(
        reader.load(&cookie).await.expect("load Ok").is_none(),
        "a cookie minted under a different secret_key must not decrypt",
    );
}

// Garbage (not even valid base64) loads as None.
#[tokio::test]
async fn garbage_cookie_loads_as_none() {
    let store = CookieStore::with_secret(TEST_SECRET);
    assert!(store.load("!!!not base64!!!").await.unwrap().is_none());
    assert!(store.load("").await.unwrap().is_none());
    assert!(
        store.load("AAAA").await.unwrap().is_none(),
        "too short to hold a nonce"
    );
}

// (c) Expired record: a successfully-decrypted but past-due record -> None.
#[tokio::test]
async fn expired_record_loads_as_none() {
    let store = CookieStore::with_secret(TEST_SECRET);
    let now = Utc::now();
    let expired = SessionRecord {
        user_id: Some("u".to_string()),
        data: "{}".to_string(),
        created_at: now - Duration::seconds(7200),
        expires_at: now - Duration::seconds(60), // already past
    };
    let cookie = store.save("tok", &expired).await.expect("save");

    assert!(
        store.load(&cookie).await.expect("load Ok").is_none(),
        "a decrypted-but-expired record must load as None (lazy expiry)",
    );
}

// (d) Size limit: an oversized data payload -> save errors with CookieTooLarge.
#[tokio::test]
async fn oversized_data_errors_on_save() {
    let store = CookieStore::with_secret(TEST_SECRET);
    // ~6 KB of JSON string data — comfortably over the 4 KB encoded limit even
    // after the constant base64 expansion factor.
    let big = "x".repeat(6 * 1024);
    let data = format!(r#"{{"blob":"{big}"}}"#);
    let rec = record(None, &data);

    let err = store
        .save("tok", &rec)
        .await
        .expect_err("oversized session must fail to save");
    assert!(
        matches!(err, SessionError::CookieTooLarge(n) if n > 4096),
        "expected CookieTooLarge with the over-limit byte count, got {err:?}",
    );
}

// destroy is a no-op (no server row) and always succeeds.
#[tokio::test]
async fn destroy_is_a_noop_ok() {
    let store = CookieStore::with_secret(TEST_SECRET);
    store.destroy("anything").await.expect("destroy is Ok");
}

// (e) End-to-end through session_layer with CookieStore installed: a writing
// handler emits a Set-Cookie carrying the encrypted blob, the DB stays empty
// (zero rows — never touched), and a second request with that cookie sees the
// written value.
#[tokio::test]
async fn end_to_end_through_session_layer_zero_db_rows() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use tower::ServiceExt;
    use umbral::plugin::Plugin;

    // Boot a real app so the ambient pool + settings exist; the CookieStore is
    // installed as the active store. The session table is created so we can
    // assert it stays EMPTY (proving CookieStore never writes a row).
    let settings = umbral::Settings::from_env().expect("settings");
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("cookie_store_e2e.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(SessionsPlugin::default().store(CookieStore::new()))
        .build()
        .expect("App::build with CookieStore");

    let pool = umbral::db::pool();
    sqlx::query(
        "CREATE TABLE session (\
            id TEXT PRIMARY KEY,\
            user_id TEXT,\
            data TEXT NOT NULL,\
            created_at TEXT NOT NULL,\
            expires_at TEXT NOT NULL\
         )",
    )
    .execute(&pool)
    .await
    .expect("create session table");

    assert_eq!(Session::objects().count().await.unwrap(), 0, "fresh DB");

    // Handler writes the session through the RequestSession path (current_mut),
    // which is the path that goes through store.save -> the cookie value.
    async fn writer() -> impl IntoResponse {
        umbral_sessions::current_mut(|s| s.set_raw("flavor", serde_json::json!("vanilla")))
            .expect("inside request scope");
        "wrote"
    }

    let inner = axum::Router::new().route("/w", get(writer));
    let router = SessionsPlugin::default()
        .store(CookieStore::new())
        .wrap_router(inner);

    // Request 1: no cookie -> handler writes -> Set-Cookie carries the blob.
    let req = Request::builder().uri("/w").body(Body::empty()).unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    let set_cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("a written cookie session must emit Set-Cookie")
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.starts_with(&format!("{COOKIE_NAME}=")));

    let cookie_value = set_cookie
        .strip_prefix(&format!("{COOKIE_NAME}="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    // The cookie value is the ENCRYPTED blob, not a bare UUID token: it must
    // decrypt back to the written record under the same key.
    let direct = CookieStore::new()
        .load(&cookie_value)
        .await
        .expect("load Ok")
        .expect("the Set-Cookie blob must decrypt");
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&direct.data).expect("data JSON");
    assert_eq!(
        map.get("flavor").and_then(|v| v.as_str()),
        Some("vanilla"),
        "the encrypted cookie must carry the handler-written data",
    );

    // ZERO DB rows: CookieStore never touched the database.
    assert_eq!(
        Session::objects().count().await.unwrap(),
        0,
        "CookieStore is stateless — a written session must create NO DB rows",
    );

    // Request 2: carry the cookie back; the handler must SEE the data on entry.
    async fn reader() -> impl IntoResponse {
        let flavor = umbral_sessions::current(|s| {
            s.get_raw("flavor")
                .and_then(|v| v.as_str().map(str::to_string))
        })
        .flatten();
        axum::Json(flavor)
    }

    let inner2 = axum::Router::new().route("/r", get(reader));
    let router2 = SessionsPlugin::default()
        .store(CookieStore::new())
        .wrap_router(inner2);

    let req2 = Request::builder()
        .uri("/r")
        .header(header::COOKIE, format!("{COOKIE_NAME}={cookie_value}"))
        .body(Body::empty())
        .unwrap();
    let resp2 = router2.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), http::StatusCode::OK);

    let body = http_body_util::BodyExt::collect(resp2.into_body())
        .await
        .unwrap()
        .to_bytes();
    let seen: Option<String> = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        seen,
        Some("vanilla".to_string()),
        "a returning request with the cookie must see the data the first wrote",
    );

    // Still zero rows after the read.
    assert_eq!(
        Session::objects().count().await.unwrap(),
        0,
        "reading a cookie session must also create no rows",
    );
}

/// audit_2 H7: a stateless CookieStore cannot revoke by user id — there's no
/// server-side record to delete. `destroy_user` must fail LOUDLY with
/// `RevocationUnsupported` (which `revoke_user_sessions` surfaces to the
/// password-reset flow) rather than silently succeed and leave stolen cookies
/// live.
#[tokio::test]
async fn cookie_store_destroy_user_is_unsupported() {
    let store = CookieStore::with_secret(TEST_SECRET);
    match store.destroy_user("42").await {
        Err(SessionError::RevocationUnsupported) => {}
        other => panic!("expected RevocationUnsupported, got {other:?}"),
    }
}
