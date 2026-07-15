//! gaps4 #9 — row-level mutation scope. A `mutable` GraphQL model with
//! `.owned_by(table, owner_col)` lets an authenticated caller mutate ONLY the
//! rows they own; another user's row (or an anonymous caller) affects zero rows.
//!
//! Two users, two posts. The `x-user` header names the caller. Own its own
//! binary because `App::build` publishes settings/registry into process-wide
//! `OnceLock`s.

#![allow(dead_code)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral_graphql::GraphqlPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "os_user")]
pub struct OsUser {
    pub id: i64,
    pub username: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "os_post")]
pub struct OsPost {
    pub id: i64,
    pub title: String,
    pub author: umbral::orm::ForeignKey<OsUser>,
}

/// Identity from an `x-user: <id>` header — the caller's own primary key.
struct HeaderUser;

#[async_trait::async_trait]
impl umbral::auth::Authentication for HeaderUser {
    async fn authenticate(
        &self,
        headers: &umbral::web::HeaderMap,
    ) -> Option<umbral::auth::Identity> {
        let id = headers.get("x-user")?.to_str().ok()?.to_string();
        Some(umbral::auth::Identity {
            user_id: id,
            is_staff: false,
            is_superuser: false,
            extras: Default::default(),
        })
    }
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> axum::Router {
    BOOT.get_or_init(|| async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("os.sqlite");
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

        let app = umbral::App::builder()
            .settings(umbral::Settings::from_env().expect("settings"))
            .database("default", pool)
            .model::<OsUser>()
            .model::<OsPost>()
            .plugin(
                GraphqlPlugin::new()
                    .expose("os_user")
                    .expose("os_post")
                    .mutable("os_post")
                    .owned_by("os_post", "author") // <- the feature under test
                    .authenticate(HeaderUser),
            )
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let p = umbral::db::pool();
        for ddl in [
            "INSERT INTO os_user (id, username) VALUES (1, 'ada'), (2, 'grace')",
            // post 1 owned by ada(1), post 2 owned by grace(2)
            "INSERT INTO os_post (id, title, author) VALUES (1, 'ada-post', 1), (2, 'grace-post', 2)",
        ] {
            sqlx::query(ddl).execute(&p).await.expect("ddl");
        }
        app.into_router()
    })
    .await
    .clone()
}

/// POST a mutation as user `x-user`.
async fn gql_as(user: Option<&str>, query: &str) -> serde_json::Value {
    let body = serde_json::json!({ "query": query }).to_string();
    let mut req = Request::builder()
        .method("POST")
        .uri("/graphql")
        .header("content-type", "application/json");
    if let Some(u) = user {
        req = req.header("x-user", u);
    }
    let res = boot()
        .await
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "graphql should answer 200");
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn title_of(id: i64) -> Option<String> {
    let p = umbral::db::pool();
    sqlx::query_scalar::<_, String>("SELECT title FROM os_post WHERE id = ?")
        .bind(id)
        .fetch_optional(&p)
        .await
        .expect("select")
}

#[tokio::test]
async fn owner_can_update_their_own_row() {
    let _ = boot().await;
    let out = gql_as(
        Some("1"),
        r#"mutation { updateOsPost(id: "1", data: { title: "ada-edited" }) { title } }"#,
    )
    .await;
    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(out["data"]["updateOsPost"]["title"], "ada-edited");
    assert_eq!(title_of(1).await.as_deref(), Some("ada-edited"));
}

#[tokio::test]
async fn a_user_cannot_update_another_users_row() {
    let _ = boot().await;
    // ada (user 1) tries to edit grace's post (id 2, author 2).
    let out = gql_as(
        Some("1"),
        r#"mutation { updateOsPost(id: "2", data: { title: "hijacked" }) { title } }"#,
    )
    .await;
    // The mutation matches zero rows → null result, no error (leaks nothing
    // about whether the row exists).
    assert!(
        out["data"]["updateOsPost"].is_null(),
        "cross-owner update must affect nothing: {out}"
    );
    assert_eq!(
        title_of(2).await.as_deref(),
        Some("grace-post"),
        "grace's row must be untouched"
    );
}

#[tokio::test]
async fn a_user_cannot_delete_another_users_row() {
    let _ = boot().await;
    let out = gql_as(Some("1"), r#"mutation { deleteOsPost(id: "2") }"#).await;
    assert_eq!(
        out["data"]["deleteOsPost"], false,
        "cross-owner delete must report false: {out}"
    );
    assert!(
        title_of(2).await.is_some(),
        "grace's row must still exist after a cross-owner delete"
    );
}

#[tokio::test]
async fn an_anonymous_caller_cannot_mutate_an_owned_model() {
    let _ = boot().await;
    let out = gql_as(
        None,
        r#"mutation { updateOsPost(id: "1", data: { title: "anon" }) { title } }"#,
    )
    .await;
    assert!(
        out["data"]["updateOsPost"].is_null(),
        "an anonymous caller has no 'self' to own a row: {out}"
    );
    assert_eq!(
        title_of(1).await.as_deref(),
        Some("ada-post"),
        "the row must be untouched by an anonymous mutation"
    );
}
