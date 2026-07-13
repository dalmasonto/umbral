//! Writes.
//!
//! Every test here drives the real HTTP endpoint against a real database, because the things
//! that go wrong with mutations — a mass-assigned `is_staff`, a CSRF'd write, a DELETE with
//! no WHERE — all live in the plumbing between the request and the row, and a test that calls
//! the resolver directly walks straight past them.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral::orm::{ForeignKey, Model};
use umbral_graphql::GraphqlPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(table = "mu_author")]
pub struct MuAuthor {
    pub id: i64,
    pub username: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(table = "mu_post")]
pub struct MuPost {
    pub id: i64,
    pub title: String,
    pub author: ForeignKey<MuAuthor>,
    /// The mass-assignment trap. A client that can add one field to a mutation body must not
    /// be able to publish its own post — or, in the real world, make itself an admin.
    #[umbral(privileged)]
    #[umbral(default = "false")]
    pub is_published: bool,
    /// Read-only to clients, and not writable either.
    #[umbral(private)]
    pub internal_score: Option<i64>,
}

/// Exposed for READING but never made `mutable`. There must be no way to write it.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(table = "mu_readonly")]
pub struct MuReadonly {
    pub id: i64,
    pub name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() -> axum::Router {
    BOOT.get_or_init(|| async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("mu.sqlite");
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
            .model::<MuAuthor>()
            .model::<MuPost>()
            .model::<MuReadonly>()
            .plugin(
                GraphqlPlugin::new()
                    .expose("mu_author")
                    .expose("mu_post")
                    .expose("mu_readonly") // exposed, NOT mutable
                    .mutable("mu_post"),
            )
            .build()
            .expect("App::build");

        let p = umbral::db::pool();
        for ddl in [
            "CREATE TABLE mu_author (id INTEGER PRIMARY KEY AUTOINCREMENT, username TEXT NOT NULL)",
            "CREATE TABLE mu_post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, \
             author INTEGER NOT NULL REFERENCES mu_author(id), \
             is_published BOOLEAN NOT NULL DEFAULT 0, internal_score INTEGER)",
            "CREATE TABLE mu_readonly (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
            "INSERT INTO mu_author (id, username) VALUES (1, 'ada')",
            "INSERT INTO mu_post (id, title, author) VALUES (1, 'Existing', 1)",
        ] {
            sqlx::query(ddl).execute(&p).await.expect("ddl");
        }
        app.into_router()
    })
    .await
    .clone()
}

/// POST a GraphQL document as JSON — i.e. the way a real client does it.
async fn gql(query: &str) -> serde_json::Value {
    let body = serde_json::json!({ "query": query }).to_string();
    let res = boot()
        .await
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/graphql")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "graphql should answer 200");
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn create_inserts_a_row_and_returns_it() {
    let _g = lock().lock().await;
    let out = gql(r#"mutation { createMuPost(data: { title: "Written by GraphQL", author: "1" }) { id title author { username } } }"#).await;
    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(out["data"]["createMuPost"]["title"], "Written by GraphQL");
    // The relation resolves on the way back out — the created row is a first-class node in
    // the graph, not a detached echo of the request body.
    assert_eq!(out["data"]["createMuPost"]["author"]["username"], "ada");

    // ...and it is actually in the database.
    let out = gql(r#"{ mu_posts(limit: 50) { title } }"#).await;
    let titles: Vec<&str> = out["data"]["mu_posts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"Written by GraphQL"), "{titles:?}");
}

/// **The one that matters.** A `#[umbral(privileged)]` column must not be settable by adding
/// a field to the mutation. In a real app this field is `is_staff`, and the whole game is
/// that a client cannot promote itself.
///
/// It is not enough that the value is ignored — it must not be in the INPUT TYPE at all, or
/// the schema is advertising a field that silently does nothing, which is its own bug.
#[tokio::test]
async fn a_privileged_column_cannot_be_mass_assigned() {
    let _g = lock().lock().await;
    let out = gql(
        r#"mutation { createMuPost(data: { title: "Sneaky", author: "1", is_published: true }) { id } }"#,
    )
    .await;
    let errs = out["errors"].to_string();
    assert!(
        errs.contains("is_published") || errs.contains("Unknown"),
        "a privileged column must not exist on the input type: {out}"
    );

    // And nothing was created as a side effect of the attempt.
    let out = gql(r#"{ mu_posts(limit: 50) { title } }"#).await;
    let titles = out["data"]["mu_posts"].to_string();
    assert!(!titles.contains("Sneaky"), "{titles}");
}

/// Exposing a model for reading must not make it writable. Two opt-ins, not one.
#[tokio::test]
async fn an_exposed_but_not_mutable_model_has_no_mutations() {
    let _g = lock().lock().await;

    // It reads fine...
    let out = gql(r#"{ mu_readonlies(limit: 5) { id } }"#).await;
    assert!(out.get("errors").is_none(), "should be readable: {out}");

    // ...and cannot be written, in any of the three ways.
    for m in [
        r#"mutation { createMuReadonly(data: { name: "x" }) { id } }"#,
        r#"mutation { updateMuReadonly(id: "1", data: { name: "x" }) { id } }"#,
        r#"mutation { deleteMuReadonly(id: "1") }"#,
    ] {
        let out = gql(m).await;
        assert!(
            out["errors"].to_string().contains("Unknown field"),
            "{m} must not exist: {out}"
        );
    }
}

/// A patch changes what it names and leaves the rest alone. Otherwise a client would have to
/// re-send the whole row to edit one field, which is how lost updates happen.
#[tokio::test]
async fn update_patches_only_the_fields_it_names() {
    let _g = lock().lock().await;
    let out = gql(r#"mutation { updateMuPost(id: "1", data: { title: "Renamed" }) { id title author { username } } }"#).await;
    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(out["data"]["updateMuPost"]["title"], "Renamed");
    // `author` was not in the patch and must survive it.
    assert_eq!(out["data"]["updateMuPost"]["author"]["username"], "ada");
}

/// Deleting a row that does not exist deletes NOTHING — it does not delete everything.
///
/// This is gaps3 #56 wearing a different hat: a predicate that cannot be built must become
/// `1=0`, never an absent WHERE clause. The assertion that matters is the row count after.
#[tokio::test]
async fn delete_removes_one_row_and_a_bogus_id_removes_none() {
    let _g = lock().lock().await;

    let before = gql(r#"{ mu_posts(limit: 200) { id } }"#).await["data"]["mu_posts"]
        .as_array()
        .unwrap()
        .len();

    let out = gql(r#"mutation { deleteMuPost(id: "not-a-number") }"#).await;
    assert_eq!(
        out["data"]["deleteMuPost"], false,
        "a bogus id deletes nothing: {out}"
    );
    let after = gql(r#"{ mu_posts(limit: 200) { id } }"#).await["data"]["mu_posts"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(after, before, "a failed delete must not empty the table");

    // A real delete really deletes.
    let created = gql(
        r#"mutation { createMuPost(data: { title: "Doomed", author: "1" }) { id } }"#,
    )
    .await["data"]["createMuPost"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = gql(&format!(r#"mutation {{ deleteMuPost(id: "{created}") }}"#)).await;
    assert_eq!(out["data"]["deleteMuPost"], true, "{out}");

    let after = gql(r#"{ mu_posts(limit: 200) { id } }"#).await["data"]["mu_posts"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(after, before, "back to where we started");
}

/// **CSRF.** `/graphql` sits in `csrf_exempt_paths` — it has to, because GraphQL reads are
/// POSTs. Once mutations exist, that exemption is the hole: a hostile page can submit an HTML
/// `<form>` at the endpoint and the browser attaches the victim's session cookie.
///
/// An HTML form can only send three content types, and none of them is `application/json`. So
/// requiring JSON rejects exactly the set of requests a hostile page is able to forge.
#[tokio::test]
async fn a_form_encoded_post_is_rejected_because_that_is_what_csrf_looks_like() {
    let _g = lock().lock().await;

    for ct in [
        "application/x-www-form-urlencoded",
        "text/plain",
        "multipart/form-data",
    ] {
        let res = boot()
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/graphql")
                    .header("content-type", ct)
                    .body(Body::from(
                        r#"{"query":"mutation { deleteMuPost(id: \"1\") }"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "a {ct} POST is forgeable by a cross-site form and must be refused"
        );
    }

    // The row it tried to delete is still there.
    let out = gql(r#"{ mu_post(id: "1") { id } }"#).await;
    assert!(out["data"]["mu_post"]["id"].is_string(), "{out}");
}
