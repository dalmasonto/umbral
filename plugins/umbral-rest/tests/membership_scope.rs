//! Membership scoping — `scope_async` + `ScopeDecision::RestrictIn`.
//!
//! The pattern this exists for: one account, many clubs, joined like groups
//! (Kikosi's real flow). A user sees rows from *every* club they belong to, and
//! from no other. That is NOT multi-tenancy — see
//! `docs/specs/row-level-tenancy.md` for why the tenancy tools are the wrong
//! shape for it — and the existing scope hook could not express it:
//! `ScopeDecision::Restrict` is equality-only and ANDed (`club_id = 1 AND
//! club_id = 2` matches nothing), and the hook was sync, so it could not run the
//! membership query in the first place.
//!
//! The assertion that matters most is `no_membership_sees_nothing`: a user who
//! has joined nothing must see NOTHING. The opposite default — treating an empty
//! scope as "unconstrained" — turns "you joined no clubs" into "you see every
//! club", which is the exact data leak this hook exists to prevent.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{
    AllowAny, FnAuthentication, Identity, ResourceConfig, RestPlugin, ScopeDecision,
};

/// A club a user can join.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "club")]
pub struct Club {
    pub id: i64,
    pub name: String,
}

/// The join model — a user belongs to many clubs, a club has many users.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "membership")]
pub struct Membership {
    pub id: i64,
    pub user_id: i64,
    pub club_id: i64,
}

/// The club-owned data being protected.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fixture")]
pub struct Fixture {
    pub id: i64,
    pub club_id: i64,
    pub title: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

fn test_lock() -> &'static tokio::sync::Mutex<()> {
    static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &TEST_LOCK
}

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("membership_scope.sqlite");
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
            let user_id: i64 = headers.get("x-user")?.to_str().ok()?.parse().ok()?;
            Some(Identity::user(user_id))
        });

        // The membership scope: which clubs has this caller joined? That is a
        // DB query, which is exactly why the hook has to be async.
        // AllowAny (writes included) so the create tests isolate gaps4 #37:
        // with the permission gate wide open, the SCOPE alone must refuse a
        // create into a club the caller has not joined.
        let rest = RestPlugin::default().authenticate(auth).resource(
            ResourceConfig::new("fixture")
                .permission(AllowAny)
                .bulk()
                .scope_async(|identity| async move {
                let Some(id) = identity else {
                    return ScopeDecision::None; // anonymous: nothing
                };
                let Ok(user_id) = id.user_id.parse::<i64>() else {
                    return ScopeDecision::None; // fail closed, never All
                };
                let mine = Membership::objects()
                    .filter(membership::USER_ID.eq(user_id))
                    .fetch()
                    .await
                    .unwrap_or_default(); // a failed lookup sees nothing, not everything
                ScopeDecision::RestrictIn(
                    "club_id".into(),
                    mine.iter().map(|m| m.club_id.to_string()).collect(),
                )
            }),
        );

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Club>()
            .model::<Membership>()
            .model::<Fixture>()
            .plugin(rest)
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();
        // Two clubs. User 7 is in BOTH 1 and 2 (the case `Restrict` cannot
        // express). User 8 is in club 3 only. User 42 has joined nothing.
        sqlx::query("INSERT INTO club (name) VALUES ('web3clubs'), ('club_x'), ('other')")
            .execute(&pool)
            .await
            .expect("seed clubs");
        sqlx::query("INSERT INTO membership (user_id, club_id) VALUES (7,1),(7,2),(8,3)")
            .execute(&pool)
            .await
            .expect("seed memberships");
        sqlx::query(
            "INSERT INTO fixture (club_id, title) VALUES (1,'w3-match'),(2,'x-match'),(3,'other-match')",
        )
        .execute(&pool)
        .await
        .expect("seed fixtures");

        app.into_router()
    })
    .await
}

async fn get(path: &str, user: Option<i64>) -> (StatusCode, Value) {
    let app = boot().await.clone();
    let mut req = Request::builder().uri(path).method("GET");
    if let Some(u) = user {
        req = req.header("x-user", u.to_string());
    }
    let res = app
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .expect("request");
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn titles(body: &Value) -> Vec<String> {
    body["results"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|r| r["title"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The headline: a user in TWO clubs sees rows from BOTH — the case
/// `ScopeDecision::Restrict` structurally cannot express (its equalities are
/// ANDed, so `club_id = 1 AND club_id = 2` matches nothing).
#[tokio::test]
async fn a_user_sees_every_club_they_belong_to() {
    let _g = test_lock().lock().await;
    let (status, body) = get("/api/fixture/", Some(7)).await;
    assert_eq!(status, StatusCode::OK);
    let mut got = titles(&body);
    got.sort();
    assert_eq!(
        got,
        vec!["w3-match".to_string(), "x-match".to_string()],
        "user 7 belongs to clubs 1 AND 2, so must see both clubs' fixtures; got:\n{body}",
    );
}

/// ...and nothing from a club they have not joined.
#[tokio::test]
async fn a_user_sees_nothing_from_clubs_they_did_not_join() {
    let _g = test_lock().lock().await;
    let (_, body) = get("/api/fixture/", Some(8)).await;
    assert_eq!(
        titles(&body),
        vec!["other-match".to_string()],
        "user 8 is only in club 3; got:\n{body}",
    );
}

/// A row in someone else's club is **404, not 403** — an out-of-scope row must be
/// indistinguishable from a row that doesn't exist, or the status code itself is
/// an existence oracle ("403 means that fixture is real").
#[tokio::test]
async fn an_out_of_scope_row_is_404_not_403() {
    let _g = test_lock().lock().await;
    // Fixture 3 belongs to club 3; user 7 is in clubs 1 and 2.
    let (status, _) = get("/api/fixture/3", Some(7)).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an out-of-scope row must 404, never 403 — 403 would confirm it exists",
    );
    // ...and the same row IS visible to a member of club 3.
    let (status, _) = get("/api/fixture/3", Some(8)).await;
    assert_eq!(status, StatusCode::OK, "user 8 is in club 3");
}

/// **The one that matters.** A user who has joined NO club sees NO rows.
///
/// The tempting implementation — "an empty scope adds no constraint" — would
/// turn "you joined nothing" into "you see everything". Empty membership must
/// fail closed.
#[tokio::test]
async fn no_membership_sees_nothing() {
    let _g = test_lock().lock().await;
    let (status, body) = get("/api/fixture/", Some(42)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        titles(&body).is_empty(),
        "user 42 has joined no clubs and must see NO fixtures — an empty \
         membership list must never mean 'unconstrained'; got:\n{body}",
    );
    let (status, _) = get("/api/fixture/1", Some(42)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "and no row by id either");
}

/// Anonymous callers get nothing.
#[tokio::test]
async fn anonymous_sees_nothing() {
    let _g = test_lock().lock().await;
    let (_, body) = get("/api/fixture/", None).await;
    assert!(
        titles(&body).is_empty(),
        "anonymous must see no fixtures; got:\n{body}"
    );
}

async fn send_json(
    method: &str,
    path: &str,
    user: Option<i64>,
    body: Value,
) -> (StatusCode, Value) {
    let app = boot().await.clone();
    let mut req = Request::builder()
        .uri(path)
        .method(method)
        .header("content-type", "application/json");
    if let Some(u) = user {
        req = req.header("x-user", u.to_string());
    }
    let res = app
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .expect("request");
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn count_titled(title: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM fixture WHERE title = ?")
        .bind(title)
        .fetch_one(&umbral::db::pool())
        .await
        .expect("count")
}

/// gaps4 #37, the allow half: a member CAN create into a club they joined.
/// (The row is removed again so the read-scope tests keep their exact lists.)
#[tokio::test]
async fn a_member_can_create_into_their_own_club() {
    let _g = test_lock().lock().await;
    let (status, body) = send_json(
        "POST",
        "/api/fixture/",
        Some(7),
        serde_json::json!({ "club_id": 1, "title": "created-in-scope" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "user 7 is in club 1, so the scoped create must succeed; got:\n{body}"
    );
    assert_eq!(count_titled("created-in-scope").await, 1);
    sqlx::query("DELETE FROM fixture WHERE title = 'created-in-scope'")
        .execute(&umbral::db::pool())
        .await
        .expect("cleanup");
}

/// gaps4 #37, the deny half — the hole this closes: the permission gate is
/// wide open (AllowAny), the caller is authenticated, and every READ of club
/// 1 already returns nothing for them — yet before the fix this POST planted
/// a row into club 1's scope anyway. The denial is a 404 (same shape as a
/// nonexistent club — no oracle), and no row lands.
#[tokio::test]
async fn a_create_into_a_foreign_club_is_refused() {
    let _g = test_lock().lock().await;
    // User 8 is in club 3 only.
    let (status, body) = send_json(
        "POST",
        "/api/fixture/",
        Some(8),
        serde_json::json!({ "club_id": 1, "title": "planted-cross-scope" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "user 8 is NOT in club 1 — the create must be refused, 404 not 403; got:\n{body}"
    );
    assert_eq!(
        count_titled("planted-cross-scope").await,
        0,
        "no row may land in the foreign scope"
    );

    // A club that does not exist at all gets the IDENTICAL refusal.
    let (status_missing, _) = send_json(
        "POST",
        "/api/fixture/",
        Some(8),
        serde_json::json!({ "club_id": 999, "title": "planted-cross-scope" }),
    )
    .await;
    assert_eq!(
        status_missing, status,
        "foreign club and nonexistent club must be indistinguishable"
    );
}

/// Empty membership and anonymous both refuse every create (the write-side
/// twin of `no_membership_sees_nothing`), and a body that omits the scoped
/// column can never be in scope.
#[tokio::test]
async fn empty_scope_and_missing_column_refuse_creates() {
    let _g = test_lock().lock().await;
    for user in [Some(42), None] {
        let (status, body) = send_json(
            "POST",
            "/api/fixture/",
            user,
            serde_json::json!({ "club_id": 3, "title": "never-lands" }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "user {user:?} has no scope to create into; got:\n{body}"
        );
    }
    // User 7 IS scoped (clubs 1, 2) but omits the scoped column entirely.
    let (status, _) = send_json(
        "POST",
        "/api/fixture/",
        Some(7),
        serde_json::json!({ "title": "no-club" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an absent scoped column cannot be in the allowed set"
    );
    assert_eq!(
        count_titled("never-lands").await + count_titled("no-club").await,
        0
    );
}

/// gaps4 #37, same class: the BULK write paths honor the scope like their
/// single-object twins. An out-of-scope row is "no row" to a bulk update,
/// and a bulk delete of it deletes nothing.
#[tokio::test]
async fn bulk_writes_honor_the_scope() {
    let _g = test_lock().lock().await;
    // Fixture 1 is club 1's ('w3-match'); user 8 is in club 3 only.
    let (status, body) = send_json(
        "PATCH",
        "/api/fixture/",
        Some(8),
        serde_json::json!([{ "id": 1, "title": "hijacked" }]),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an out-of-scope pk must read as 'no row' to bulk update; got:\n{body}"
    );
    assert_eq!(count_titled("hijacked").await, 0);

    let (status, _) = send_json(
        "DELETE",
        "/api/fixture/",
        Some(8),
        serde_json::json!({ "ids": [1] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "bulk delete skips out-of-scope ids exactly like nonexistent ids"
    );
    assert_eq!(
        count_titled("w3-match").await,
        1,
        "the out-of-scope row must survive the foreign bulk delete"
    );
}
