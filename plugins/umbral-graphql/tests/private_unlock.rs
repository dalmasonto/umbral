//! `GraphqlPlugin::allow_private_if` — the read-path unlock for `#[umbral(private)]`.
//!
//! Without it, a private column is absent from the schema, which makes it indistinguishable
//! from `#[umbral(secret)]` — a two-tier policy with one usable tier. This is the second tier.
//!
//! Note the schema is ONE shape for everybody: the conditionally-visible field exists and is
//! nullable, and who gets a value is decided per request. That is the honest form for GraphQL,
//! where introspection is a single document.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral::auth::{Authentication, Identity};
use umbral::orm::Model;
use umbral_graphql::GraphqlPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(table = "gp_product")]
pub struct GpProduct {
    pub id: i64,
    pub name: String,
    /// NOT NULL in the database — and still nullable in the schema, because a caller without
    /// the unlock receives nothing and "nothing" has to be a legal value.
    #[umbral(private)]
    pub cost: String,
    /// Private with NO unlock: absent from the schema entirely, even for staff.
    #[umbral(private)]
    pub supplier_notes: Option<String>,
}

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
        let path = tmp.path().join("gp.sqlite");
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
            .model::<GpProduct>()
            .plugin(
                GraphqlPlugin::new()
                    .authenticate(HeaderAuth)
                    .expose("gp_product")
                    .mutable("gp_product")
                    .allow_private_if("gp_product", "cost", |id| id.is_some_and(|i| i.is_staff)),
            )
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let p = umbral::db::pool();
        for ddl in [
            "INSERT INTO gp_product (id, name, cost, supplier_notes) VALUES \
             (1, 'Widget', '4.20', 'acme, net 30')",
        ] {
            sqlx::query(ddl).execute(&p).await.expect("ddl");
        }
        app.into_router()
    })
    .await
    .clone()
}

async fn gql(query: &str, who: Option<&str>) -> serde_json::Value {
    let body = serde_json::json!({ "query": query }).to_string();
    let mut b = Request::builder()
        .method("POST")
        .uri("/graphql")
        .header("content-type", "application/json");
    if let Some(w) = who {
        b = b.header("x-test-user", w);
    }
    let res = boot()
        .await
        .oneshot(b.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// The gate closed: the field exists, and an anonymous caller gets nothing in it.
#[tokio::test]
async fn an_anonymous_reader_gets_null_for_the_private_column() {
    let out = gql(r#"{ gp_product(id: "1") { name cost } }"#, None).await;
    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(out["data"]["gp_product"]["name"], "Widget");
    assert!(
        out["data"]["gp_product"]["cost"].is_null(),
        "cost leaked to anonymous: {out}"
    );
}

/// The gate OPENS. This is what did not exist: with no unlock reachable from GraphQL, a
/// `private` column could never be read by anyone, making it a synonym for `secret`.
#[tokio::test]
async fn staff_get_the_unlocked_column() {
    let out = gql(r#"{ gp_product(id: "1") { name cost } }"#, Some("staff")).await;
    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(
        out["data"]["gp_product"]["cost"], "4.20",
        "the unlock must actually unlock: {out}"
    );
}

/// A logged-in NON-staff caller is still denied. The closure decides, not the session.
#[tokio::test]
async fn an_ordinary_user_is_still_denied() {
    let out = gql(r#"{ gp_product(id: "1") { cost } }"#, Some("customer")).await;
    assert!(
        out["data"]["gp_product"]["cost"].is_null(),
        "cost leaked to a customer: {out}"
    );
}

/// A private column with NO unlock stays absent from the schema — for everyone, staff
/// included. An unlock reveals the field it names and nothing else.
#[tokio::test]
async fn a_private_column_with_no_unlock_is_not_in_the_schema_at_all() {
    let out = gql(
        r#"{ gp_product(id: "1") { supplier_notes } }"#,
        Some("staff"),
    )
    .await;
    assert!(
        out["errors"].to_string().contains("Unknown field"),
        "a private column with no unlock must not exist in the schema: {out}"
    );
}

/// The list and the cursor connection honour the unlock too — not just retrieve-by-id.
///
/// Easy to get wrong: wire it into one resolver, ship it, and the other two quietly keep
/// serving the wrong shape. There are four separate read paths through the ORM in this plugin.
#[tokio::test]
async fn every_read_path_honours_the_unlock() {
    let list = gql(r#"{ gp_products(limit: 5) { cost } }"#, Some("staff")).await;
    assert_eq!(
        list["data"]["gp_products"][0]["cost"], "4.20",
        "the list resolver must unlock: {list}"
    );
    let anon = gql(r#"{ gp_products(limit: 5) { cost } }"#, None).await;
    assert!(
        anon["data"]["gp_products"][0]["cost"].is_null(),
        "the list resolver leaked: {anon}"
    );

    let conn = gql(
        r#"{ gp_productsConnection(first: 5) { edges { node { cost } } } }"#,
        Some("staff"),
    )
    .await;
    assert_eq!(
        conn["data"]["gp_productsConnection"]["edges"][0]["node"]["cost"], "4.20",
        "the connection resolver must unlock: {conn}"
    );
    let anon = gql(
        r#"{ gp_productsConnection(first: 5) { edges { node { cost } } } }"#,
        None,
    )
    .await;
    assert!(
        anon["data"]["gp_productsConnection"]["edges"][0]["node"]["cost"].is_null(),
        "the connection resolver leaked: {anon}"
    );
}

/// **`private` is a READ policy. The mutation lands.**
///
/// An anonymous caller can SET `cost` and still cannot READ it back — the field is in the
/// INPUT type but resolves to `null` for them on the way out. GraphQL models this natively: an
/// input field need not exist on the object type.
///
/// The attribute for "must not be settable from an untrusted body" is `#[umbral(privileged)]`,
/// which is a different question. Conflating the two is what produced the misleading "this
/// field is required" on the REST side (gaps3 #75).
#[tokio::test]
async fn an_anonymous_writer_can_set_a_private_column_but_reads_back_null() {
    let out = gql(
        r#"mutation { createGpProduct(data: { name: "Sneaky", cost: "0.01" }) { id cost } }"#,
        None,
    )
    .await;
    assert!(
        out.get("errors").is_none(),
        "a private column is settable by anyone who may write: {out}"
    );
    assert!(
        out["data"]["createGpProduct"]["cost"].is_null(),
        "...but it must read back as null for a caller with no unlock: {out}"
    );

    // The value really landed — read it back through the one caller allowed to see it.
    let staff = gql(r#"{ gp_products(limit: 50) { name cost } }"#, Some("staff")).await;
    let rows = staff["data"]["gp_products"].as_array().unwrap();
    let created = rows
        .iter()
        .find(|r| r["name"] == "Sneaky")
        .expect("the row was created");
    assert_eq!(
        created["cost"], "0.01",
        "the private column the anonymous caller sent must have landed: {created}"
    );
}

/// Staff can write it — the same closure that reveals `cost` is what lets `cost` be set.
#[tokio::test]
async fn staff_can_write_the_unlocked_column() {
    let out = gql(
        r#"mutation { createGpProduct(data: { name: "Gadget", cost: "9.99" }) { name cost } }"#,
        Some("staff"),
    )
    .await;
    assert!(out.get("errors").is_none(), "{out}");
    // The row echoed back after a write is a serialized response like any other — so it is
    // unlocked for this caller, and would be null for an anonymous one.
    assert_eq!(out["data"]["createGpProduct"]["cost"], "9.99", "{out}");
}

/// The sharpest case: a private column with **no unlock at all**.
///
/// It is absent from the output type forever — nobody can read it, not even staff (asserted by
/// `a_private_column_with_no_unlock_is_not_in_the_schema_at_all`). But it is still SETTABLE,
/// because `private` is a read policy. This is the write-only column: a support agent files an
/// `internal_note` that the API will never hand back.
///
/// If this ever regresses to "unwritable", `private` has silently become a write guard again —
/// which is the confusion that produced gaps3 #75.
#[tokio::test]
async fn a_private_column_with_no_unlock_is_still_writable() {
    let out = gql(
        r#"mutation { createGpProduct(data: { name: "WriteOnly", cost: "1.00", supplier_notes: "from acme" }) { id } }"#,
        Some("staff"),
    )
    .await;
    assert!(
        out.get("errors").is_none(),
        "a private column with no unlock must still be settable: {out}"
    );

    // It cannot be read back through the API by anyone — so verify it landed by going around
    // the API, straight to the database. That is the whole point of a write-only column.
    let stored: String =
        sqlx::query_scalar("SELECT supplier_notes FROM gp_product WHERE name = 'WriteOnly'")
            .fetch_one(&umbral::db::pool())
            .await
            .expect("the row exists");
    assert_eq!(
        stored, "from acme",
        "the write-only column must actually have been written"
    );
}
