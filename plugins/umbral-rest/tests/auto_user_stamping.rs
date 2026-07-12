//! `#[umbral(auto_user_add)]` / `#[umbral(auto_user)]` — author stamping (gaps3 #55).
//!
//! Two properties, and the second is the one that makes this worth having:
//!
//! 1. The author is stamped from the **authenticated caller**, with no help from
//!    the app — the who-did-it twin of `auto_now_add` / `auto_now`.
//! 2. The author is **server-owned**: a client cannot forge it by putting someone
//!    else's id in the request body. That is why the stamp happens before the
//!    body is ever consulted.
//!
//! Note what is NOT here: nothing keys off a column *named* `created_by`. The
//! attribute opts in. A plain `created_by` field with no attribute is the app's
//! own column and is never touched.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{AllowAny, FnAuthentication, Identity, ResourceConfig, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "memo")]
pub struct Memo {
    pub id: i64,
    pub title: String,
    /// Stamped once, on create.
    #[umbral(auto_user_add)]
    pub created_by: Option<i64>,
    /// Re-stamped on every write.
    #[umbral(auto_user)]
    pub updated_by: Option<i64>,
    /// NOT an attribute — just a column the app owns. Must never be touched.
    pub author_note: Option<String>,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("auto_user.sqlite");
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

        let auth = FnAuthentication::new(|headers| async move {
            let uid: i64 = headers.get("x-user")?.to_str().ok()?.parse().ok()?;
            Some(Identity::user(uid))
        });

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Memo>()
            // Writes are 403 by default (safe-by-default REST); this test is about
            // WHO gets stamped, not who may write, so open the resource explicitly.
            .plugin(
                RestPlugin::default()
                    .authenticate(auth)
                    .resource(ResourceConfig::new("memo").permission(AllowAny)),
            )
            .build()
            .expect("App::build");

        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE memo (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, \
             created_by INTEGER, updated_by INTEGER, author_note TEXT)",
        )
        .execute(&pool)
        .await
        .expect("ddl");

        app.into_router()
    })
    .await
}

async fn send(
    method: &str,
    path: &str,
    user: Option<i64>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let app = boot().await.clone();
    let mut req = Request::builder().uri(path).method(method);
    if let Some(u) = user {
        req = req.header("x-user", u.to_string());
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = app.oneshot(req).await.expect("request");
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The feature: creating through REST stamps the caller, with no app code.
#[tokio::test]
async fn create_stamps_the_authenticated_caller() {
    let _g = lock().lock().await;
    let (status, row) = send(
        "POST",
        "/api/memo/",
        Some(7),
        Some(json!({"title": "hello"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "got: {row}");
    assert_eq!(
        row["created_by"],
        json!(7),
        "author stamped from the caller: {row}"
    );
    assert_eq!(row["updated_by"], json!(7), "and the toucher too: {row}");
}

/// **The security property.** A client cannot forge the author by putting someone
/// else's id in the body — the stamp happens before the body is consulted.
#[tokio::test]
async fn a_client_cannot_forge_the_author() {
    let _g = lock().lock().await;
    let (status, row) = send(
        "POST",
        "/api/memo/",
        Some(7),
        // user 7 claims the memo was authored by user 99
        Some(json!({"title": "forged", "created_by": 99, "updated_by": 99})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "got: {row}");
    assert_eq!(
        row["created_by"],
        json!(7),
        "the body said 99; the server must stamp the REAL caller (7): {row}",
    );
    assert_eq!(row["updated_by"], json!(7), "got: {row}");
}

/// `auto_user` re-stamps on update; `auto_user_add` stays frozen at the original
/// author. This is the whole reason they are two attributes.
#[tokio::test]
async fn update_restamps_auto_user_but_not_auto_user_add() {
    let _g = lock().lock().await;
    let (_, created) = send(
        "POST",
        "/api/memo/",
        Some(7),
        Some(json!({"title": "orig"})),
    )
    .await;
    let id = created["id"].as_i64().expect("id");

    let (status, row) = send(
        "PATCH",
        &format!("/api/memo/{id}"),
        Some(8), // a DIFFERENT user edits it
        Some(json!({"title": "edited"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {row}");
    assert_eq!(
        row["created_by"],
        json!(7),
        "created_by is frozen at the original author, not the editor: {row}",
    );
    assert_eq!(
        row["updated_by"],
        json!(8),
        "updated_by follows whoever touched it last: {row}",
    );
}

/// A column the user merely *named* `author_note`, with no attribute, is theirs.
/// The framework keys off the ATTRIBUTE, never the column name.
#[tokio::test]
async fn an_unattributed_column_is_left_alone() {
    let _g = lock().lock().await;
    let (_, row) = send(
        "POST",
        "/api/memo/",
        Some(7),
        Some(json!({"title": "x", "author_note": "written by ada"})),
    )
    .await;
    assert_eq!(
        row["author_note"],
        json!("written by ada"),
        "a plain column is the app's own — never stamped or clobbered: {row}",
    );
}

/// No authenticated caller → NULL, not a guess and not a failure. A background
/// job, the CLI, an anonymous request: none of them have an author.
#[tokio::test]
async fn an_anonymous_write_stamps_null() {
    let _g = lock().lock().await;
    let (status, row) = send("POST", "/api/memo/", None, Some(json!({"title": "anon"}))).await;
    assert_eq!(status, StatusCode::CREATED, "got: {row}");
    assert_eq!(
        row["created_by"],
        Value::Null,
        "no caller → NULL author: {row}"
    );
}
