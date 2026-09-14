//! gaps4 #78 — declarative relation-path object scope. `ResourceConfig::owned_via`
//! restricts EVERY built-in CRUD action to the rows reachable from the caller
//! through a forward FK/O2O chain — ownership TWO hops away (`gig.developer.user
//! == caller`) — with NO hand-written `scope_async` resolver. It's the write-side
//! face of gaps4 #76 (the `__`-traversal filter): both resolve the same
//! Django-style `__` path via `umbral::orm::build_dynamic_relation`.
//!
//! Model shape: `Gig --FK--> Developer`, and `Developer.user` (a plain string
//! column, not itself a further relation) names the owning caller. So
//! `.owned_via("developer", "user")` scopes `gig` to
//! `gig.developer_id IN (SELECT id FROM developer WHERE user = <caller>)`.

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
use umbral::orm::ForeignKey;
use umbral_rest::{AllowAny, ResourceConfig, RestPlugin};

// `Clone` is required: `Developer` is `Gig.developer`'s FK target, and the
// heavy-relations epic's cache-aware to-one accessor clones the cached row
// on a `select_related` hit — every `#[derive(Model)]` struct is expected to
// derive `Clone` for exactly this reason.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Developer {
    id: i64,
    #[umbral(string)]
    user: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Gig {
    id: i64,
    title: String,
    developer: ForeignKey<Developer>,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("owned_via_scope.sqlite");
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

        // Auth: `x-user` IS the identity; `x-superuser: 1` promotes it. A test
        // stand-in for a real session/bearer resolver — `AllowAny` isolates the
        // test to SCOPING, not permission gating.
        let auth = FnAuthentication::new(|headers: umbral::web::HeaderMap| async move {
            let user = headers.get("x-user").and_then(|v| v.to_str().ok())?;
            let is_super = headers.get("x-superuser").is_some();
            Some(Identity::user(user).with_superuser(is_super))
        });
        let resource = ResourceConfig::new("gig")
            .permission(AllowAny)
            .owned_via("developer", "user");
        let rest = RestPlugin::default().authenticate(auth).resource(resource);

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Developer>()
            .model::<Gig>()
            .plugin(rest)
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();
        sqlx::query("INSERT INTO developer (id, user) VALUES (1, 'alice'), (2, 'bob')")
            .execute(&pool)
            .await
            .expect("seed developers");
        sqlx::query(
            "INSERT INTO gig (id, title, developer) VALUES \
             (1, 'alice-gig', 1), (2, 'bob-gig', 2)",
        )
        .execute(&pool)
        .await
        .expect("seed gigs");

        app.into_router()
    })
    .await
}

async fn req(
    method: Method,
    uri: &str,
    user: Option<&str>,
    superuser: bool,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let router = boot().await.clone();
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(u) = user {
        b = b.header("x-user", u);
    }
    if superuser {
        b = b.header("x-superuser", "1");
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
async fn list_returns_only_the_callers_rows_through_the_relation() {
    let (status, body) = req(Method::GET, "/api/gig/", Some("alice"), false, None).await;
    assert_eq!(status, StatusCode::OK);
    let results = body
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        results.len(),
        1,
        "alice must see only the gig whose developer.user is her: {body}"
    );
    assert_eq!(results[0]["title"], "alice-gig");
}

#[tokio::test]
async fn retrieve_own_ok_others_404() {
    let (own, _) = req(Method::GET, "/api/gig/1", Some("alice"), false, None).await;
    assert_eq!(own, StatusCode::OK, "alice reads her own gig");
    let (other, _) = req(Method::GET, "/api/gig/2", Some("alice"), false, None).await;
    assert_eq!(
        other,
        StatusCode::NOT_FOUND,
        "alice must NOT read bob's gig (two hops away) — IDOR"
    );
}

#[tokio::test]
async fn update_others_row_is_404() {
    let (s, _) = req(
        Method::PATCH,
        "/api/gig/2",
        Some("alice"),
        false,
        Some(json!({ "title": "hijacked" })),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "alice must NOT update bob's gig");
    let (s2, body) = req(Method::GET, "/api/gig/2", Some("bob"), false, None).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(body["title"], "bob-gig", "bob's row must be unchanged");
}

#[tokio::test]
async fn delete_others_row_is_404() {
    let (s, _) = req(Method::DELETE, "/api/gig/1", Some("bob"), false, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "bob must NOT delete alice's gig");
    let (s2, _) = req(Method::GET, "/api/gig/1", Some("alice"), false, None).await;
    assert_eq!(s2, StatusCode::OK, "alice's gig must still exist");
}

#[tokio::test]
async fn anonymous_is_denied_everything() {
    let (list, body) = req(Method::GET, "/api/gig/", None, false, None).await;
    assert_eq!(list, StatusCode::OK);
    let results = body
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(results.is_empty(), "anonymous sees no rows: {body}");
    let (detail, _) = req(Method::GET, "/api/gig/1", None, false, None).await;
    assert_eq!(
        detail,
        StatusCode::NOT_FOUND,
        "anonymous can't read a row by id"
    );
}

#[tokio::test]
async fn superuser_sees_every_row() {
    let (status, body) = req(Method::GET, "/api/gig/", Some("root"), true, None).await;
    assert_eq!(status, StatusCode::OK);
    let results = body
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(results.len(), 2, "a superuser sees every gig: {body}");
    let (detail, _) = req(Method::GET, "/api/gig/2", Some("root"), true, None).await;
    assert_eq!(detail, StatusCode::OK, "a superuser reaches any row by id");
}

#[tokio::test]
async fn create_is_checked_against_the_same_relation_chain() {
    // alice owns developer #1 (developer.user == "alice") — a create naming
    // her OWN developer succeeds with no hand-written resolver.
    let (ok, body) = req(
        Method::POST,
        "/api/gig/",
        Some("alice"),
        false,
        Some(json!({ "title": "alice-new-gig", "developer": 1 })),
    )
    .await;
    assert_eq!(
        ok,
        StatusCode::CREATED,
        "alice may create a gig under her OWN developer: {body}"
    );

    // developer #2 belongs to bob (developer.user == "bob") — alice naming it
    // must be refused: a create can't bootstrap access to a scope she can't
    // read, the write-side twin of the read-side 404.
    let (denied, body2) = req(
        Method::POST,
        "/api/gig/",
        Some("alice"),
        false,
        Some(json!({ "title": "smuggled-gig", "developer": 2 })),
    )
    .await;
    assert_eq!(
        denied,
        StatusCode::NOT_FOUND,
        "alice must NOT create a gig under bob's developer: {body2}"
    );

    // A create naming a nonexistent developer id is refused the same way (no
    // oracle distinguishing "foreign" from "absent").
    let (bad, _) = req(
        Method::POST,
        "/api/gig/",
        Some("alice"),
        false,
        Some(json!({ "title": "ghost-gig", "developer": 999 })),
    )
    .await;
    assert_eq!(bad, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn anonymous_create_is_denied() {
    let (s, _) = req(
        Method::POST,
        "/api/gig/",
        None,
        false,
        Some(json!({ "title": "anon-gig", "developer": 1 })),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "anonymous can't create anything");
}
