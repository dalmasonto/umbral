//! gaps4 #79 — object scopes were action-BLIND: `.owned_by(...)`/`.scope(...)`
//! ANDed the SAME `ScopeDecision` into every built-in CRUD action, so the
//! moment a resource wanted owner-only WRITES it also lost public READS
//! (anonymous list → empty page, retrieve → 404). `ResourceConfig::owned_by_for_writes`
//! (built on the new `scope_writes`/`scope_writes_async`) is the opt-in fix:
//! `list`/`retrieve` stay unconstrained while `create`/`update`/`delete` (+
//! bulk) are scoped to the owner. `.owned_by(...)` itself is UNCHANGED — it
//! still scopes every action, proven here too (mirrors `object_scope.rs`).

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral::auth::{FnAuthentication, Identity};
use umbral_rest::{AllowAny, ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post {
    id: i64,
    title: String,
    #[umbral(string)]
    author_id: String,
}

// `umbral::Settings::init` is a process-global `OnceLock` (see
// `crates/umbral-core/src/settings.rs`), so a single test BINARY can only
// call `App::builder().build()` ONCE — both the new write-only-scope
// resource ("post") and the pre-existing `.owned_by(...)` regression check
// ("doc") share this one app/router instead of each booting their own.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Doc {
    id: i64,
    title: String,
    #[umbral(string)]
    owner_id: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("owner_or_read_only_scope.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new().busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        // Auth: the `x-user` header IS the identity (a test stand-in for a real
        // session/bearer resolver). AllowAny isolates the test to SCOPING, not
        // action-level permission.
        let auth = FnAuthentication::new(|headers: umbral::web::HeaderMap| async move {
            headers
                .get("x-user")
                .and_then(|v| v.to_str().ok())
                .map(Identity::user)
        });
        // Public read, owner-only write: the new opt-in.
        let post_resource = ResourceConfig::new("post")
            .permission(AllowAny)
            .owned_by_for_writes("author_id");
        // Regression: plain `.owned_by(...)` must still scope EVERY action,
        // reads included — completely unaffected by `owned_by_for_writes`
        // existing.
        let doc_resource = ResourceConfig::new("doc")
            .permission(AllowAny)
            .owned_by("owner_id");
        let rest = RestPlugin::default()
            .authenticate(auth)
            .resource(post_resource)
            .resource(doc_resource);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .model::<Doc>()
            .plugin(rest)
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();
        sqlx::query("INSERT INTO post (id, title, author_id) VALUES (1, 'alice-post', 'alice'), (2, 'bob-post', 'bob')")
            .execute(&pool)
            .await
            .expect("seed post");
        sqlx::query("INSERT INTO doc (id, title, owner_id) VALUES (1, 'alice-doc', 'alice'), (2, 'bob-doc', 'bob')")
            .execute(&pool)
            .await
            .expect("seed doc");

        app.into_router()
    })
    .await
}

async fn req(
    method: Method,
    uri: &str,
    user: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let router = boot().await.clone();
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        b = b.header("x-user", u);
    }
    let request = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = router.oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn anonymous_list_sees_every_row() {
    let (status, body) = req(Method::GET, "/api/post/", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let results = body
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        results.len(),
        2,
        "an anonymous READ must be UNRESTRICTED by a write-only owner scope: {body}"
    );
}

#[tokio::test]
async fn anonymous_retrieve_of_any_row_is_ok() {
    let (own, _) = req(Method::GET, "/api/post/1", None, None).await;
    assert_eq!(own, StatusCode::OK, "anonymous can read alice's post");
    let (other, _) = req(Method::GET, "/api/post/2", None, None).await;
    assert_eq!(other, StatusCode::OK, "anonymous can read bob's post too");
}

#[tokio::test]
async fn non_owner_update_is_denied() {
    let (s, _) = req(
        Method::PATCH,
        "/api/post/2",
        Some("alice"),
        Some(json!({ "title": "hijacked" })),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "alice must NOT update bob's post (write scope)"
    );
    // Bob's row is unchanged, and still publicly readable.
    let (s2, body) = req(Method::GET, "/api/post/2", None, None).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(body["title"], "bob-post", "bob's row must be unchanged");
}

#[tokio::test]
async fn owner_update_is_ok() {
    let (s, body) = req(
        Method::PATCH,
        "/api/post/1",
        Some("alice"),
        Some(json!({ "title": "alice-post-edited" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "alice may update her own post: {body}");
    assert_eq!(body["title"], "alice-post-edited");
}

#[tokio::test]
async fn anonymous_write_is_denied() {
    let (create, _) = req(
        Method::POST,
        "/api/post/",
        None,
        Some(json!({ "title": "anon-post", "author_id": "ghost" })),
    )
    .await;
    assert_eq!(
        create,
        StatusCode::NOT_FOUND,
        "anonymous create is refused by the write scope (no identity to own the row)"
    );

    let (delete, _) = req(Method::DELETE, "/api/post/1", None, None).await;
    assert_eq!(
        delete,
        StatusCode::NOT_FOUND,
        "anonymous delete is refused by the write scope"
    );
    // Confirm it's still there for everyone to read.
    let (s2, _) = req(Method::GET, "/api/post/1", None, None).await;
    assert_eq!(s2, StatusCode::OK, "the row must survive the denied delete");
}

#[tokio::test]
async fn non_owner_delete_is_denied_owner_delete_is_ok() {
    let (s, _) = req(Method::DELETE, "/api/post/2", Some("alice"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "alice must NOT delete bob's post");
    let (s2, _) = req(Method::DELETE, "/api/post/2", Some("bob"), None).await;
    assert_eq!(s2, StatusCode::NO_CONTENT, "bob may delete his own post");
}

// ---------------------------------------------------------------------------
// Regression: the EXISTING `.owned_by(...)` (scopes every action, including
// reads) must be completely unaffected by `owned_by_for_writes` existing —
// verified here against the "doc" resource sharing the same app (see `boot`).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn owned_by_still_scopes_anonymous_reads_to_nothing() {
    // No regression from #79: `.owned_by(...)` alone still means anonymous
    // sees NO rows on list and gets 404 on retrieve — the pre-existing
    // behavior this gap explicitly must not change.
    let (status, body) = req(Method::GET, "/api/doc/", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let results = body
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        results.is_empty(),
        "owned_by must still scope anonymous LIST to nothing: {body}"
    );

    let (detail, _) = req(Method::GET, "/api/doc/1", None, None).await;
    assert_eq!(
        detail,
        StatusCode::NOT_FOUND,
        "owned_by must still 404 anonymous RETRIEVE"
    );
}

#[tokio::test]
async fn owned_by_still_scopes_non_owner_reads() {
    let (detail, _) = req(Method::GET, "/api/doc/2", Some("alice"), None).await;
    assert_eq!(
        detail,
        StatusCode::NOT_FOUND,
        "owned_by must still 404 a non-owner's RETRIEVE (unchanged by #79)"
    );
}
