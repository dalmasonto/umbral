//! Tests for opt-in sliding expiry (gaps2 #80 — part a) and the
//! `clearsessions` management command (gaps2 #80 — part b).
//!
//! Both features are additive — the default path (fixed expiry, no
//! cleanup command) is unchanged. The "sliding OFF does not change
//! expires_at" assertion is in `tests/sliding_expiry_off.rs` so that
//! binary can boot with `SessionsPlugin::default()` (sliding = false).
//!
//! This binary boots with `SessionsPlugin::default().sliding_expiry()`
//! so `SLIDING_EXPIRY_ENABLED` is `true` for all tests in this process.

use chrono::{Duration, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral_auth::{AuthPlugin, AuthUser};
use umbral_sessions::{Session, SessionsPlugin, create_session, read_session};

static BOOT: OnceCell<()> = OnceCell::const_new();

/// Boot once per test binary. Sliding expiry is ON.
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults load");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("sliding_and_clearsessions.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("sqlite tempfile pool");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().sliding_expiry())
            .build()
            .expect("App::build with SessionsPlugin (sliding ON)");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS auth_user (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                username TEXT NOT NULL UNIQUE,\
                email TEXT NOT NULL,\
                password_hash TEXT NOT NULL,\
                is_active INTEGER NOT NULL,\
                is_staff INTEGER NOT NULL,\
                is_superuser INTEGER NOT NULL,\
                date_joined TEXT NOT NULL,\
                last_login TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create auth_user");
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS session (\
                id TEXT PRIMARY KEY,\
                user_id TEXT,\
                data TEXT NOT NULL,\
                created_at TEXT NOT NULL,\
                expires_at TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create session");
    })
    .await;
}

// =========================================================================
// clearsessions — the ORM path: Session::objects().filter(EXPIRES_AT.lt(now)).delete()
// =========================================================================

/// `clearsessions` deletes expired rows and leaves live ones intact,
/// returning the right count.
#[tokio::test]
async fn clearsessions_deletes_expired_leaves_live() {
    boot().await;
    let pool = umbral::db::pool();

    // Insert rows directly with known ids so we can check them after.
    let past = Utc::now() - Duration::seconds(200);
    let future = Utc::now() + Duration::seconds(7200);

    // Unique per-test prefixes avoid collisions with other tests in this binary.
    let id_exp1 = "cs_exp1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let id_exp2 = "cs_exp2_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let id_live = "cs_live_ccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    for (id, exp) in [
        (id_exp1, past),
        (id_exp2, past - Duration::seconds(50)),
        (id_live, future),
    ] {
        sqlx::query(
            "INSERT OR REPLACE INTO session \
             (id, user_id, data, created_at, expires_at) \
             VALUES (?, NULL, '{}', ?, ?)",
        )
        .bind(id)
        .bind(Utc::now())
        .bind(exp)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("insert {id}: {e}"));
    }

    // Verify the two expired rows exist before we call clearsessions.
    let count_before: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM session WHERE id IN (?, ?)",
    )
    .bind(id_exp1)
    .bind(id_exp2)
    .fetch_one(&pool)
    .await
    .expect("count before");
    assert_eq!(count_before.0, 2, "both expired rows should exist before clearsessions");

    // Execute the same ORM call the `clearsessions` command runs.
    let now = Utc::now();
    let deleted = Session::objects()
        .filter(umbral_sessions::session::EXPIRES_AT.lt(now))
        .delete()
        .await
        .expect("clearsessions delete");

    assert!(
        deleted >= 2,
        "clearsessions should have deleted at least 2 rows; got {deleted}"
    );

    // Expired rows are gone.
    for id in [id_exp1, id_exp2] {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT id FROM session WHERE id = ?")
                .bind(id)
                .fetch_optional(&pool)
                .await
                .expect("check expired row");
        assert!(
            row.is_none(),
            "clearsessions must have deleted expired row {id}"
        );
    }

    // Live row is untouched.
    let live_row: Option<(String,)> =
        sqlx::query_as("SELECT id FROM session WHERE id = ?")
            .bind(id_live)
            .fetch_optional(&pool)
            .await
            .expect("check live row");
    assert!(
        live_row.is_some(),
        "clearsessions must not delete live session row {id_live}"
    );
}

// =========================================================================
// Sliding expiry ON: session_layer bumps expires_at forward.
// =========================================================================

#[tokio::test]
async fn sliding_expiry_on_bumps_expires_at_via_layer() {
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;
    use umbral::plugin::Plugin;
    use umbral::web::header;
    use umbral_sessions::{COOKIE_NAME, SessionToken, set_data};

    boot().await;

    // Create a live session with a short remaining TTL (5 minutes).
    let token = create_session(None, Some(Duration::seconds(300)))
        .await
        .expect("create session for sliding test");

    // Record expires_at before the request.
    let before = read_session(&token)
        .await
        .expect("read before")
        .expect("session must exist")
        .expires_at;

    // Sleep a few ms so now() is strictly greater than `before`, making
    // the post-request `expires_at` unambiguously newer.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Drive a request through the wrapped router. The handler writes
    // the session so the row is confirmed live. The sliding bump in
    // session_layer fires on entry when it resolves the existing row.
    async fn write_handler(
        axum::Extension(SessionToken(t)): axum::Extension<SessionToken>,
    ) -> &'static str {
        let _ = set_data(&t, "ping", &true).await;
        "ok"
    }

    let inner = axum::Router::new().route("/", get(write_handler));
    // SLIDING_EXPIRY_ENABLED was set to `true` by boot() via on_ready().
    let router = SessionsPlugin::default().wrap_router(inner);

    let req = Request::builder()
        .uri("/")
        .header(header::COOKIE, format!("{COOKIE_NAME}={token}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), http::StatusCode::OK);

    // expires_at must be strictly later than the pre-request snapshot.
    let after = read_session(&token)
        .await
        .expect("read after")
        .expect("session still exists after sliding update")
        .expires_at;

    assert!(
        after > before,
        "sliding expiry ON: expires_at should have advanced;\n  before={before:?}\n  after= {after:?}"
    );
}
