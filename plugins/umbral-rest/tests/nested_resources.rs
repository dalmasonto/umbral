//! Parent-scoped sub-resources — `ResourceConfig::under` (gaps3 #29 item 2).
//!
//! The live consumer had 11 handlers across 3 plugins on the shape
//! `/api/fixture/{id}/{selections,goals,payments,rsvp}`, each hand-writing the same
//! four steps: check the parent exists → 404 → filter children by the parent FK →
//! mutate. This closes it.
//!
//! Most of what follows is not "does it work" but "can it be walked around". A nested
//! resource whose flat route still works, or whose bulk endpoint skips the parent
//! injection, is not scoped — it just has a scoped-looking URL, which is strictly worse
//! than no scoping, because you would trust it.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral::orm::ForeignKey;
use umbral_rest::{ResourceConfig, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fixture")]
pub struct Fixture {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "selection")]
pub struct Selection {
    pub id: i64,
    pub fixture_id: ForeignKey<Fixture>,
    pub player: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .expect("pool");
        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Fixture>()
            .model::<Selection>()
            .plugin(
                RestPlugin::default()
                    .default_permission(umbral_rest::permission::AllowAny)
                    .resource(ResourceConfig::new("fixture"))
                    .resource(
                        ResourceConfig::new("selection")
                            .under("fixture", "fixture_id")
                            .bulk(),
                    ),
            )
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();
        for ddl in [
            "INSERT INTO fixture (id, name) VALUES (1, 'derby'), (2, 'cup final')",
            "INSERT INTO selection (fixture_id, player) VALUES (1, 'ada'), (1, 'grace'), (2, 'linus')",
        ] {
            sqlx::query(ddl).execute(&pool).await.expect("ddl");
        }
        app.into_router()
    })
    .await
}

async fn req(method: &str, uri: &str, body: Option<serde_json::Value>) -> (StatusCode, String) {
    let mut b = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let res = boot()
        .await
        .clone()
        .oneshot(b.body(body).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 256 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// The list is scoped to the parent named in the URL — and only to it.
#[tokio::test]
async fn list_is_scoped_to_the_parent_in_the_url() {
    let (status, body) = req("GET", "/api/fixture/1/selection", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("ada") && body.contains("grace"), "{body}");
    assert!(
        !body.contains("linus"),
        "fixture 2's selection leaked into fixture 1's list: {body}"
    );
}

/// A child collection under a parent that does not exist is a WRONG URL, not an empty
/// one. `200 []` would tell the client it asked a valid question about a real fixture,
/// so a typo'd id, a deleted fixture and a genuinely empty one become indistinguishable
/// — and the bug hides in the case you cannot see.
#[tokio::test]
async fn a_missing_parent_is_404_not_an_empty_list() {
    let (status, body) = req("GET", "/api/fixture/999/selection", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("999"), "the 404 should name the id: {body}");
}

/// Retrieve, update and delete are scoped too — not just list. Selection 3 belongs to
/// fixture 2, so asking for it under fixture 1 must not find it. If only `list` were
/// scoped, the id would be a skeleton key to every other parent's rows.
#[tokio::test]
async fn detail_routes_cannot_reach_another_parents_row() {
    let (status, _) = req("GET", "/api/fixture/1/selection/3", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "retrieve crossed parents");

    let (status, _) = req(
        "PATCH",
        "/api/fixture/1/selection/3",
        Some(serde_json::json!({"player": "pwned"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "update crossed parents");

    let (status, _) = req("DELETE", "/api/fixture/1/selection/3", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "delete crossed parents");

    // ...and it is still there, under its own parent.
    let (status, body) = req("GET", "/api/fixture/2/selection/3", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("linus"), "{body}");
}

/// Create takes the parent id from the URL.
#[tokio::test]
async fn create_injects_the_parent_from_the_url() {
    let (status, body) = req(
        "POST",
        "/api/fixture/2/selection",
        Some(serde_json::json!({"player": "hopper"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.contains("\"fixture_id\":2"), "{body}");

    let (_, listed) = req("GET", "/api/fixture/2/selection", None).await;
    assert!(listed.contains("hopper"), "{listed}");
    let (_, other) = req("GET", "/api/fixture/1/selection", None).await;
    assert!(
        !other.contains("hopper"),
        "the new row landed under the wrong parent: {other}"
    );
}

/// A body that names a DIFFERENT parent than the URL is rejected, not silently
/// overwritten. Silently winning is defensible, but the client that sent it believes
/// something false about what it just created, and a 201 would confirm the belief.
#[tokio::test]
async fn a_body_supplied_parent_id_is_rejected() {
    let (status, body) = req(
        "POST",
        "/api/fixture/2/selection",
        Some(serde_json::json!({"player": "mallory", "fixture_id": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("fixture_id"), "{body}");
}

/// **The bypass test.** Bulk create must inject the parent per item — otherwise
/// `.bulk()` is simply the way around `under()`, and every guarantee above is theatre.
#[tokio::test]
async fn bulk_create_cannot_be_used_to_escape_the_parent() {
    // Bulk under fixture 2 lands under fixture 2...
    let (status, body) = req(
        "POST",
        "/api/fixture/2/selection",
        Some(serde_json::json!([{"player": "bulk_a"}, {"player": "bulk_b"}])),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (_, one) = req("GET", "/api/fixture/1/selection", None).await;
    assert!(
        !one.contains("bulk_a") && !one.contains("bulk_b"),
        "bulk create escaped its parent: {one}"
    );

    // ...and an item that names another parent is refused, exactly as a single create is.
    let (status, body) = req(
        "POST",
        "/api/fixture/2/selection",
        Some(serde_json::json!([{"player": "sneaky", "fixture_id": 1}])),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "bulk must apply the same rule as single create: {body}"
    );
}

/// **The other bypass test.** Declaring a parent REMOVES the flat route. A resource
/// reachable both nested and flat is not scoped — `/api/selection/3` would hand back
/// fixture 2's row to anyone who guessed the id, and the nested URL would be decoration.
#[tokio::test]
async fn declaring_a_parent_removes_the_flat_route() {
    let (status, body) = req("GET", "/api/selection", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        body.contains("fixture"),
        "the 404 should point at the nested URL: {body}"
    );

    let (status, _) = req("GET", "/api/selection/3", None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "flat detail route still live"
    );

    let (status, _) = req(
        "POST",
        "/api/selection",
        Some(serde_json::json!({"player": "x", "fixture_id": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "flat create still live");
}

/// Nesting under a parent the resource never declared is a 404 — the nested route is
/// generic, so it must refuse what it does not own rather than serving it unscoped.
#[tokio::test]
async fn nesting_under_the_wrong_parent_is_404() {
    let (status, _) = req("GET", "/api/selection/1/fixture", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A parent id that cannot even be a `fixture.id` (a BigInt) must deny, never fall
/// through to "unscoped" — which would return every selection in the table.
#[tokio::test]
async fn an_uncoercible_parent_id_denies_rather_than_unscoping() {
    let (status, body) = req("GET", "/api/fixture/not-a-number/selection", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(
        !body.contains("ada"),
        "rows leaked on a bad parent id: {body}"
    );
}
