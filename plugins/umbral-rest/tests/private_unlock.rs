//! `ResourceConfig::allow_private_if` — the read-path unlock for `#[umbral(private)]`.
//!
//! Until this existed, `private` had no unlock reachable from any API: the ORM offered
//! `DynQuerySet::allow_private`, and no plugin called it. Over REST and GraphQL, a `private`
//! field therefore behaved *exactly* like a `secret` one — hidden, permanently. A two-tier
//! policy with one usable tier.
//!
//! Driven through the real router with real rows, because the interesting failures live in
//! the plumbing: the field being fetched but then stripped by the response filter, the field
//! being readable but silently writable, the spec describing a shape the API never returns.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral::auth::{Authentication, Identity};
use umbral::orm::Model;
use umbral_rest::{ResourceConfig, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(table = "pu_product")]
pub struct PuProduct {
    pub id: i64,
    pub name: String,
    pub price: String,
    /// Staff see it. Nobody else does. And nobody else can set it either.
    #[umbral(private)]
    pub cost: String,
    /// Private with NO unlock configured — so it stays invisible to everyone, which is what
    /// `private` means on its own.
    #[umbral(private)]
    pub supplier_notes: Option<String>,
}

/// Authenticates by a header, so a test can be staff or anonymous at will.
/// `X-Test-User: staff` → a staff identity. Absent → anonymous.
#[derive(Clone)]
struct HeaderAuth;

#[async_trait::async_trait]
impl Authentication for HeaderAuth {
    async fn authenticate(&self, headers: &umbral::web::HeaderMap) -> Option<Identity> {
        let who = headers.get("x-test-user")?.to_str().ok()?;
        Some(Identity {
            user_id: "1".to_string(),
            is_staff: who == "staff",
            is_superuser: false,
            extras: Default::default(),
        })
    }
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> axum::Router {
    BOOT.get_or_init(|| async {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("pu.sqlite");
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
            .model::<PuProduct>()
            .plugin(
                RestPlugin::default()
                    .authenticate(HeaderAuth)
                    .default_permission(umbral_rest::permission::AllowAny)
                    .resource(
                        ResourceConfig::new("pu_product")
                            // staff, and only staff, may see (and set) the wholesale cost
                            .allow_private_if("cost", |id| id.is_some_and(|i| i.is_staff)),
                    ),
            )
            .build()
            .expect("App::build");

        let p = umbral::db::pool();
        for ddl in [
            "CREATE TABLE pu_product (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, \
             price TEXT NOT NULL, cost TEXT NOT NULL, supplier_notes TEXT)",
            // Row 1 is READ-ONLY for these tests; row 2 is the one the write test mutates.
            // Sharing a mutable row across tests that run in parallel is a race, and the
            // failure looks exactly like a bug in the feature under test.
            "INSERT INTO pu_product (id, name, price, cost, supplier_notes) VALUES \
             (1, 'Widget', '19.99', '4.20', 'acme corp, net 30'), \
             (2, 'Gadget', '9.99', '2.50', NULL)",
        ] {
            sqlx::query(ddl).execute(&p).await.expect("ddl");
        }
        app.into_router()
    })
    .await
    .clone()
}

async fn req(method: &str, uri: &str, who: Option<&str>, body: Option<&str>) -> serde_json::Value {
    let body = body.map(|b| b.to_string());
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(w) = who {
        b = b.header("x-test-user", w);
    }
    if body.is_some() {
        b = b.header("content-type", "application/json");
    }
    let res = boot()
        .await
        .oneshot(
            b.body(body.map(Body::from).unwrap_or_else(Body::empty))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    assert_ne!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{method} {uri} blew up"
    );
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    assert!(status.is_success(), "{method} {uri} -> {status}: {body}");
    body
}

/// The gate closed: an anonymous caller sees neither private column.
#[tokio::test]
async fn an_anonymous_reader_sees_no_private_column() {
    let out = req("GET", "/api/pu_product/1", None, None).await;
    assert_eq!(out["name"], "Widget", "{out}");
    assert!(out.get("cost").is_none(), "cost leaked to anonymous: {out}");
    assert!(
        out.get("supplier_notes").is_none(),
        "supplier_notes leaked: {out}"
    );
}

/// The gate OPENS. This is the half that did not exist: before `allow_private_if`, no caller
/// on any API could ever see a private column, which made `private` a synonym for `secret`.
#[tokio::test]
async fn staff_see_the_unlocked_column_and_only_that_one() {
    let out = req("GET", "/api/pu_product/1", Some("staff"), None).await;
    assert_eq!(
        out["cost"], "4.20",
        "the unlock must actually unlock: {out}"
    );

    // ...and unlocks nothing else. `supplier_notes` is private with NO unlock configured, so
    // it stays invisible even to staff. An unlock that widens past what it names is how a
    // staff endpoint quietly starts serving everything.
    assert!(
        out.get("supplier_notes").is_none(),
        "an unlock must not widen beyond the field it names: {out}"
    );
}

/// A non-staff *authenticated* caller is still denied — the closure decides, not the mere
/// presence of a session.
#[tokio::test]
async fn an_ordinary_logged_in_user_is_still_denied() {
    let out = req("GET", "/api/pu_product/1", Some("customer"), None).await;
    assert_eq!(out["name"], "Widget", "{out}");
    assert!(
        out.get("cost").is_none(),
        "cost leaked to a customer: {out}"
    );
}

/// The list endpoint honours the unlock too — not just retrieve-by-id.
///
/// Easy to get wrong: wire the unlock into `retrieve`, ship it, and the list endpoint quietly
/// keeps serving the redacted shape (or, worse, the unredacted one).
#[tokio::test]
async fn the_list_endpoint_honours_the_unlock_in_both_directions() {
    let anon = req("GET", "/api/pu_product/", None, None).await;
    let row = &anon["results"][0];
    assert!(
        row.get("cost").is_none(),
        "list leaked cost to anon: {anon}"
    );

    let staff = req("GET", "/api/pu_product/", Some("staff"), None).await;
    let row = &staff["results"][0];
    assert_eq!(row["cost"], "4.20", "list must unlock for staff: {staff}");
}

/// **A field only staff may READ is not one an anonymous caller gets to SET.**
///
/// Without this, marking a column `private` would hide it from every response while leaving
/// it wide open to `PATCH` — a worse position than not marking it at all, because it *looks*
/// protected.
#[tokio::test]
async fn an_anonymous_writer_cannot_set_a_private_column() {
    // An anonymous PATCH naming `cost` must not change `cost`.
    let _ = req(
        "PATCH",
        "/api/pu_product/1",
        None,
        Some(r#"{"name":"Widget","cost":"0.01"}"#),
    )
    .await;

    let staff = req("GET", "/api/pu_product/1", Some("staff"), None).await;
    assert_ne!(
        staff["cost"], "0.01",
        "an anonymous caller wrote a private column: {staff}"
    );
    assert_eq!(staff["cost"], "4.20", "cost must be untouched: {staff}");
}

/// What an anonymous *create* looks like when a private column is NOT NULL.
///
/// The caller's `cost` is stripped before the write, so the column has no value and the
/// validator rejects the row: `cost: ["This field is required."]`. The data is safe — which is
/// the point — but the message is misleading, because the client DID send `cost` and is being
/// told it is missing. The honest answer would be "you are not allowed to set `cost`".
///
/// Asserted here so the behaviour is pinned rather than discovered, and logged as gaps3 #74.
#[tokio::test]
async fn an_anonymous_create_is_rejected_when_the_private_column_is_required() {
    let body = serde_json::json!({ "name": "Sneaky", "price": "1.00", "cost": "0.01" }).to_string();
    let res = boot()
        .await
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/pu_product/")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "the stripped column leaves the row invalid"
    );

    // And nothing was created.
    let staff = req("GET", "/api/pu_product/", Some("staff"), None).await;
    assert!(
        !staff["results"].to_string().contains("Sneaky"),
        "a rejected create must not leave a row behind: {staff}"
    );
}

/// Staff CAN write it, because the unlock governs both directions: the same closure that
/// reveals `cost` is what lets `cost` be changed.
#[tokio::test]
async fn staff_can_write_the_unlocked_column() {
    // Row 2 — its own row, because this test mutates and the others assert.
    let out = req(
        "PATCH",
        "/api/pu_product/2",
        Some("staff"),
        Some(r#"{"cost":"5.55"}"#),
    )
    .await;
    assert_ne!(out, serde_json::Value::Null, "patch produced no body");

    let staff = req("GET", "/api/pu_product/2", Some("staff"), None).await;
    assert_eq!(staff["cost"], "5.55", "staff write did not land: {staff}");
}
