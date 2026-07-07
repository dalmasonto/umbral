//! Phase 4 user-prefs tests.
//!
//! 1. GET /admin/api/prefs returns defaults for a new user.
//! 2. PUT /admin/api/prefs updates them.
//! 3. Invalid theme values are ignored (not rejected).

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;

use umbral_admin::AdminPlugin;
use umbral_auth::{AuthPlugin, AuthUser, create_user_with_flags};
use umbral_sessions::SessionsPlugin;

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();
static LOCK: Mutex<()> = Mutex::const_new(());

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("settings");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("phase4_prefs.sqlite");
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

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(SessionsPlugin::default().without_auto_layer())
            .plugin(AdminPlugin::default())
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS auth_user (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                username TEXT NOT NULL UNIQUE,\
                email TEXT NOT NULL,\
                password_hash TEXT NOT NULL,\
                is_active INTEGER NOT NULL DEFAULT 1,\
                is_staff INTEGER NOT NULL DEFAULT 0,\
                is_superuser INTEGER NOT NULL DEFAULT 0,\
                date_joined TEXT NOT NULL,\
                last_login TEXT,\
                email_verified_at TEXT\
            )",
        )
        .execute(&pool)
        .await
        .ok();

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS session (\
                id TEXT PRIMARY KEY,\
                user_id TEXT,\
                data TEXT NOT NULL DEFAULT '{}',\
                created_at TEXT NOT NULL,\
                expires_at TEXT NOT NULL\
            )",
        )
        .execute(&pool)
        .await
        .ok();

        umbral_admin::models::ensure_tables_for_tests(&pool)
            .await
            .expect("ensure_tables");

        app.into_router()
    })
    .await
}

async fn staff_cookie() -> String {
    let user = match create_user_with_flags(
        "prefs_user",
        "prefs@example.com",
        "pass123",
        true,
        false,
    )
    .await
    {
        Ok(u) => u,
        Err(_) => {
            let pool = umbral::db::pool();
            sqlx::query_as::<_, umbral_auth::AuthUser>(
                "SELECT id, username, email, password_hash, is_active, is_staff, is_superuser, date_joined, last_login, email_verified_at \
                 FROM auth_user WHERE username = 'prefs_user'",
            )
            .fetch_one(&pool)
            .await
            .expect("lookup prefs_user")
        }
    };
    let tok = umbral_sessions::create_session(Some(user.id.to_string()), None)
        .await
        .expect("session");
    format!("umbral_session={tok}")
}

// =========================================================================
// Tests
// =========================================================================

#[tokio::test]
async fn prefs_get_returns_defaults_for_new_user() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    // Wipe any prefs row left behind by the PUT tests — tokio test
    // ordering inside one binary isn't guaranteed even with the
    // file-local mutex, so each test that asserts default state must
    // reset that state first.
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM admin_user_pref")
        .execute(&pool)
        .await
        .expect("wipe admin_user_pref");

    let req = Request::builder()
        .uri("/admin/api/prefs")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();

    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["theme"].as_str().unwrap_or(""), "dark");
    assert_eq!(json["density"].as_str().unwrap_or(""), "comfortable");
    assert_eq!(json["sidebar_collapsed"].as_bool().unwrap_or(true), false);
}

#[tokio::test]
async fn prefs_put_updates_and_get_reflects_change() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let put_req = Request::builder()
        .method("PUT")
        .uri("/admin/api/prefs")
        .header(header::COOKIE, cookie.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"theme":"light","density":"compact"}"#))
        .unwrap();

    let put_resp = router.clone().oneshot(put_req).await.unwrap();
    assert_eq!(put_resp.status(), StatusCode::OK);

    let get_req = Request::builder()
        .uri("/admin/api/prefs")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();

    let get_resp = router.clone().oneshot(get_req).await.unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);

    let body = get_resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["theme"].as_str().unwrap_or(""), "light");
    assert_eq!(json["density"].as_str().unwrap_or(""), "compact");
}

#[tokio::test]
async fn prefs_put_ignores_invalid_theme_value() {
    let _guard = LOCK.lock().await;
    let router = boot().await;
    let cookie = staff_cookie().await;

    let put_req = Request::builder()
        .method("PUT")
        .uri("/admin/api/prefs")
        .header(header::COOKIE, cookie.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"theme":"fuchsia"}"#))
        .unwrap();
    let resp = router.clone().oneshot(put_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// =========================================================================
// gaps2 #11 — per-table preference round-trip via the JSON blob column
// =========================================================================

async fn fresh_test_user(username: &str) -> i64 {
    let pool = umbral::db::pool();
    // Lazily create; reuse if a prior test in this run already made it.
    if let Ok(row) = sqlx::query_as::<_, umbral_auth::AuthUser>(
        "SELECT id, username, email, password_hash, is_active, is_staff, \
         is_superuser, date_joined, last_login, email_verified_at FROM auth_user WHERE username = ?",
    )
    .bind(username)
    .fetch_one(&pool)
    .await
    {
        return row.id;
    }
    create_user_with_flags(
        username,
        &format!("{username}@example.com"),
        "pw",
        true,
        false,
    )
    .await
    .expect("create user")
    .id
}

#[tokio::test]
async fn table_pref_returns_none_for_user_with_no_prefs_row() {
    let _guard = LOCK.lock().await;
    let _router = boot().await;
    let uid = fresh_test_user("pref_none").await;

    // No prefs row yet — read should be `None` (not a panic, not a
    // parse error, not an empty TablePref).
    let pref = umbral_admin::models::get_table_pref(uid, "product")
        .await
        .expect("read");
    assert!(pref.is_none(), "no row yet → None: got {pref:?}");
}

#[tokio::test]
async fn table_pref_round_trips_filters_search_sort_per_page() {
    let _guard = LOCK.lock().await;
    let _router = boot().await;
    let uid = fresh_test_user("pref_round_trip").await;

    let mut filters = std::collections::HashMap::new();
    filters.insert("status".to_string(), "active".to_string());
    filters.insert("brand".to_string(), "acme".to_string());
    let original = umbral_admin::models::TablePref {
        filters,
        search: "widget".to_string(),
        sort: "-price".to_string(),
        per_page: Some(50),
        hidden_cols: vec![],
    };

    umbral_admin::models::set_table_pref(uid, "product", &original)
        .await
        .expect("save");

    let loaded = umbral_admin::models::get_table_pref(uid, "product")
        .await
        .expect("read")
        .expect("pref present after save");

    assert_eq!(loaded.search, "widget");
    assert_eq!(loaded.sort, "-price");
    assert_eq!(loaded.per_page, Some(50));
    assert_eq!(
        loaded.filters.get("status").map(|s| s.as_str()),
        Some("active")
    );
    assert_eq!(
        loaded.filters.get("brand").map(|s| s.as_str()),
        Some("acme")
    );
}

#[tokio::test]
async fn table_pref_per_table_namespaces_dont_collide() {
    // Setting prefs on `product` must NOT affect `order`. The JSON
    // blob nests by table key; the read for a different table
    // returns `None`.
    let _guard = LOCK.lock().await;
    let _router = boot().await;
    let uid = fresh_test_user("pref_namespace").await;

    let pref_a = umbral_admin::models::TablePref {
        search: "table_a_search".to_string(),
        ..Default::default()
    };
    umbral_admin::models::set_table_pref(uid, "product", &pref_a)
        .await
        .expect("save product");

    let product = umbral_admin::models::get_table_pref(uid, "product")
        .await
        .unwrap()
        .expect("product pref present");
    assert_eq!(product.search, "table_a_search");

    let order = umbral_admin::models::get_table_pref(uid, "order")
        .await
        .unwrap();
    assert!(
        order.is_none(),
        "order pref must be None (prefs not set for that table)"
    );

    // Now write a different pref for `order`; product survives.
    let pref_b = umbral_admin::models::TablePref {
        search: "table_b_search".to_string(),
        ..Default::default()
    };
    umbral_admin::models::set_table_pref(uid, "order", &pref_b)
        .await
        .expect("save order");

    let product_again = umbral_admin::models::get_table_pref(uid, "product")
        .await
        .unwrap()
        .expect("product still set");
    assert_eq!(
        product_again.search, "table_a_search",
        "writing `order` must not clobber `product`"
    );
    let order_loaded = umbral_admin::models::get_table_pref(uid, "order")
        .await
        .unwrap()
        .expect("order now present");
    assert_eq!(order_loaded.search, "table_b_search");
}

#[tokio::test]
async fn table_pref_malformed_json_in_db_reads_as_none() {
    // Pre-existing rows might carry stale or hand-edited JSON. The
    // read path must treat malformed payload as "no prefs" (None) so
    // the next write overwrites with a valid shape — not crash the
    // handler.
    let _guard = LOCK.lock().await;
    let _router = boot().await;
    let uid = fresh_test_user("pref_malformed").await;

    // Manually insert a row with garbage in `preferences` (the upsert
    // path can't produce this; this models a stale row).
    let pool = umbral::db::pool();
    sqlx::query(
        "INSERT INTO admin_user_pref \
         (user_id, theme, density, sidebar_collapsed, dashboard_layout, preferences, updated_at) \
         VALUES (?, 'dark', 'comfortable', 0, '[]', 'not json at all {', datetime('now'))",
    )
    .bind(uid)
    .execute(&pool)
    .await
    .expect("seed malformed row");

    let pref = umbral_admin::models::get_table_pref(uid, "product")
        .await
        .expect("read should not error on garbage");
    assert!(
        pref.is_none(),
        "malformed JSON → None (not panic, not parse error)"
    );
}

// =========================================================================
// gaps2 #11 round 2 — last_path + hidden_cols toggle
// =========================================================================

#[tokio::test]
async fn last_path_round_trips_through_preferences_blob() {
    let _guard = LOCK.lock().await;
    let _router = boot().await;
    let uid = fresh_test_user("pref_last_path").await;

    assert!(
        umbral_admin::models::get_last_path(uid)
            .await
            .unwrap()
            .is_none(),
        "no prefs yet → None"
    );

    umbral_admin::models::set_last_path(uid, "/admin/product/?search=foo")
        .await
        .expect("save");
    assert_eq!(
        umbral_admin::models::get_last_path(uid)
            .await
            .unwrap()
            .as_deref(),
        Some("/admin/product/?search=foo"),
    );

    // Overwriting is fine — last write wins.
    umbral_admin::models::set_last_path(uid, "/admin/order/")
        .await
        .expect("save");
    assert_eq!(
        umbral_admin::models::get_last_path(uid)
            .await
            .unwrap()
            .as_deref(),
        Some("/admin/order/"),
    );
}

#[tokio::test]
async fn last_path_coexists_with_table_pref_writes() {
    // Verify the JSON merge doesn't clobber sibling keys. Setting a
    // table pref must preserve a prior last_path, and vice versa.
    let _guard = LOCK.lock().await;
    let _router = boot().await;
    let uid = fresh_test_user("pref_last_path_coexist").await;

    umbral_admin::models::set_last_path(uid, "/admin/product/")
        .await
        .unwrap();
    umbral_admin::models::set_table_pref(
        uid,
        "order",
        &umbral_admin::models::TablePref {
            search: "shipped".to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Both reads see the values they wrote.
    assert_eq!(
        umbral_admin::models::get_last_path(uid)
            .await
            .unwrap()
            .as_deref(),
        Some("/admin/product/"),
    );
    let order = umbral_admin::models::get_table_pref(uid, "order")
        .await
        .unwrap()
        .expect("order pref present");
    assert_eq!(order.search, "shipped");
}

#[tokio::test]
async fn toggle_table_col_flips_visibility_idempotently() {
    let _guard = LOCK.lock().await;
    let _router = boot().await;
    let uid = fresh_test_user("pref_toggle_col").await;

    // First toggle hides the column.
    let visible = umbral_admin::models::toggle_table_col(uid, "product", "cost")
        .await
        .expect("toggle 1");
    assert!(!visible, "first toggle hides (now_visible = false)");
    let pref = umbral_admin::models::get_table_pref(uid, "product")
        .await
        .unwrap()
        .expect("pref written");
    assert_eq!(pref.hidden_cols, vec!["cost".to_string()]);

    // Second toggle restores it.
    let visible = umbral_admin::models::toggle_table_col(uid, "product", "cost")
        .await
        .expect("toggle 2");
    assert!(visible, "second toggle shows (now_visible = true)");
    let pref = umbral_admin::models::get_table_pref(uid, "product")
        .await
        .unwrap()
        .expect("pref written");
    assert!(
        pref.hidden_cols.is_empty(),
        "cost removed from hidden_cols: {:?}",
        pref.hidden_cols,
    );
}

#[tokio::test]
async fn widget_period_round_trips_through_preferences_blob() {
    let _guard = LOCK.lock().await;
    let _router = boot().await;
    let uid = fresh_test_user("pref_widget_period").await;

    assert!(
        umbral_admin::models::get_widget_period(uid, "shop_daily_sales_chart")
            .await
            .unwrap()
            .is_none(),
        "no override yet → None"
    );

    umbral_admin::models::set_widget_period(uid, "shop_daily_sales_chart", "7d")
        .await
        .expect("save 7d");
    assert_eq!(
        umbral_admin::models::get_widget_period(uid, "shop_daily_sales_chart")
            .await
            .unwrap()
            .as_deref(),
        Some("7d"),
    );

    // Two widgets coexist independently.
    umbral_admin::models::set_widget_period(uid, "shop_activity_chart", "30d")
        .await
        .unwrap();
    assert_eq!(
        umbral_admin::models::get_widget_period(uid, "shop_daily_sales_chart")
            .await
            .unwrap()
            .as_deref(),
        Some("7d"),
        "first widget unaffected by second widget's save"
    );
    assert_eq!(
        umbral_admin::models::get_widget_period(uid, "shop_activity_chart")
            .await
            .unwrap()
            .as_deref(),
        Some("30d"),
    );
}

#[tokio::test]
async fn toggle_table_col_preserves_other_pref_fields() {
    // Writing hidden_cols must not destroy filters/search/sort/
    // per_page already set by the changelist render path.
    let _guard = LOCK.lock().await;
    let _router = boot().await;
    let uid = fresh_test_user("pref_toggle_preserve").await;

    let mut filters = std::collections::HashMap::new();
    filters.insert("status".to_string(), "active".to_string());
    umbral_admin::models::set_table_pref(
        uid,
        "product",
        &umbral_admin::models::TablePref {
            filters,
            search: "widget".to_string(),
            sort: "-price".to_string(),
            per_page: Some(50),
            hidden_cols: vec![],
        },
    )
    .await
    .unwrap();

    umbral_admin::models::toggle_table_col(uid, "product", "cost")
        .await
        .unwrap();

    let pref = umbral_admin::models::get_table_pref(uid, "product")
        .await
        .unwrap()
        .expect("pref still there");
    assert_eq!(pref.search, "widget", "search survived the toggle");
    assert_eq!(pref.sort, "-price", "sort survived the toggle");
    assert_eq!(pref.per_page, Some(50), "per_page survived the toggle");
    assert_eq!(
        pref.filters.get("status").map(|s| s.as_str()),
        Some("active"),
        "filters survived the toggle"
    );
    assert_eq!(pref.hidden_cols, vec!["cost".to_string()]);
}
