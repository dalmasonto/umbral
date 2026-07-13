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

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let p = umbral::db::pool();
        for ddl in [
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

/// **`private` is a READ policy. A write still lands.**
///
/// A caller who may write the resource may SET `cost` — and still cannot READ it back. That is
/// the honest reading of "private": the value went in, the API just will not show it to you.
/// The attribute for "must not be settable from an untrusted body" is `#[umbral(privileged)]`,
/// which is a different question with a different answer.
#[tokio::test]
async fn an_anonymous_writer_can_set_a_private_column_but_never_read_it() {
    // An anonymous PATCH naming `cost` DOES change `cost` ...
    let echoed = req(
        "PATCH",
        "/api/pu_product/1",
        None,
        Some(r#"{"name":"Widget","cost":"0.01"}"#),
    )
    .await;

    // ... but the response it gets back still does not contain it.
    assert!(
        echoed.get("cost").is_none(),
        "the write echo leaked a private column to an anonymous caller: {echoed}"
    );

    // The value really landed — read it back through the one caller allowed to see it.
    let staff = req("GET", "/api/pu_product/1", Some("staff"), None).await;
    assert_eq!(
        staff["cost"], "0.01",
        "the write must actually have landed: {staff}"
    );
}

/// The create that used to fail with a lie.
///
/// `cost` is NOT NULL. It used to be stripped from the body before the write, so the row was
/// invalid and the client got `cost: ["This field is required."]` — for a field it had
/// demonstrably just sent. The data was safe; the message was false (gaps3 #75).
///
/// Now the write lands: `private` never governed writes, it only looked like it did.
#[tokio::test]
async fn an_anonymous_create_can_set_the_required_private_column() {
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
        StatusCode::CREATED,
        "a NOT NULL private column is settable, so the row is valid"
    );

    let echoed: serde_json::Value = {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    };
    assert!(
        echoed.get("cost").is_none(),
        "the create echo must still hide the private column: {echoed}"
    );

    // The row exists, and the private value really is on it.
    let staff = req("GET", "/api/pu_product/", Some("staff"), None).await;
    let created = staff["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Sneaky")
        .expect("the row was created");
    assert_eq!(
        created["cost"], "0.01",
        "the private column the anonymous caller sent must have landed: {created}"
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
