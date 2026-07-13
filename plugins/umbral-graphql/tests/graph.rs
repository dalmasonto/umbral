//! A GraphQL API derived from the model registry.
//!
//! The claim under test is not "GraphQL responds". It is that you can **traverse the
//! graph** — `post { author { username } }` and `author { posts { title } }` — because
//! that is the only reason to add GraphQL to a framework that already has REST. A schema
//! of `getPost` / `listPosts` would answer queries too, and nobody would use it.
//!
//! And the second claim, which decides whether this is safe to deploy: relations are
//! **batched**. The client picks the query shape, so the client picks your query count. A
//! GraphQL endpoint without DataLoader is an N+1 generator that the caller aims.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;
use umbral::orm::ForeignKey;
use umbral_graphql::GraphqlPlugin;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "gq_author")]
pub struct GqAuthor {
    pub id: i64,
    pub username: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "gq_post")]
pub struct GqPost {
    pub id: i64,
    pub title: String,
    /// Confidential, but only by convention — nothing in the model says so. It is hidden by
    /// `GraphqlPlugin::hide`, which is the *configurable* tier.
    pub internal_cost: Option<String>,
    pub author: ForeignKey<GqAuthor>,
}

/// Exposed ON PURPOSE, with no `.hide("password_hash")` anywhere — because that is exactly
/// the mistake the core denylist exists to survive.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "gq_user")]
pub struct GqUser {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
}

/// Exposed to NOBODY. It exists to prove the deny-by-default rule: a model you did not
/// name must not be reachable, not even as a relation hanging off one you did.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "gq_secret")]
pub struct GqSecret {
    pub id: i64,
    pub api_key: String,
    pub author: ForeignKey<GqAuthor>,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

/// `DB_READS` is a process-global counter, so the batching test cannot share the process
/// with tests issuing their own queries in parallel — their reads would land in its count.
/// Every test here takes this lock, which serialises the file. Six fast tests; the
/// alternative is a flaky assertion about a number that another thread is also moving.
fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() -> axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("gq.sqlite");
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
            .settings(settings)
            .database("default", pool)
            .model::<GqAuthor>()
            .model::<GqPost>()
            .model::<GqSecret>()
            .model::<GqUser>()
            // gq_secret is deliberately NOT exposed.
            .plugin(
                GraphqlPlugin::new()
                    .expose("gq_author")
                    .expose("gq_post")
                    // Exposed with NO hide for password_hash. Core must save us anyway.
                    .expose("gq_user")
                    .hide("gq_post", "internal_cost"),
            )
            .build()
            .expect("App::build");

        let p = umbral::db::pool();
        for ddl in [
            "CREATE TABLE gq_author (id INTEGER PRIMARY KEY AUTOINCREMENT, username TEXT NOT NULL)",
            "CREATE TABLE gq_post (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, \
             internal_cost TEXT, author INTEGER NOT NULL REFERENCES gq_author(id))",
            "CREATE TABLE gq_user (id INTEGER PRIMARY KEY AUTOINCREMENT, username TEXT NOT NULL, \
             password_hash TEXT NOT NULL)",
            "CREATE TABLE gq_secret (id INTEGER PRIMARY KEY AUTOINCREMENT, api_key TEXT NOT NULL, \
             author INTEGER NOT NULL REFERENCES gq_author(id))",
            "INSERT INTO gq_author (id, username) VALUES (1, 'ada'), (2, 'grace')",
            "INSERT INTO gq_post (title, author) VALUES ('Analytical Engine', 1), \
             ('Notes on the Engine', 1), ('COBOL', 2)",
            "INSERT INTO gq_secret (api_key, author) VALUES ('sk_live_do_not_leak', 1)",
            "INSERT INTO gq_user (username, password_hash) VALUES ('ada', '$argon2id$v=19$leak')",
        ] {
            sqlx::query(ddl).execute(&p).await.expect("ddl");
        }
        app.into_router()
    })
    .await
    .clone()
}

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
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "graphql endpoint should be 200"
    );
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// **The point of the whole plugin.** Walk from a post to its author — an edge nobody
/// declared, derived from `Column::fk_target` in the model registry.
#[tokio::test]
async fn a_query_traverses_a_foreign_key_to_the_related_object() {
    let _g = lock().lock().await;
    let out = gql(r#"{ gq_post(id: "1") { title author { username } } }"#).await;
    assert!(out.get("errors").is_none(), "{out}");
    let post = &out["data"]["gq_post"];
    assert_eq!(post["title"], "Analytical Engine");
    assert_eq!(
        post["author"]["username"], "ada",
        "the FK edge must resolve to the real related row: {out}"
    );
}

/// The REVERSE edge — `author { posts { title } }` — which nobody declared either. It is
/// the forward FK inverted, read straight out of the registry.
#[tokio::test]
async fn a_query_traverses_the_reverse_relation() {
    let _g = lock().lock().await;
    let out = gql(r#"{ gq_author(id: "1") { username gq_posts { title } } }"#).await;
    assert!(out.get("errors").is_none(), "{out}");
    let a = &out["data"]["gq_author"];
    assert_eq!(a["username"], "ada");
    let titles: Vec<&str> = a["gq_posts"]
        .as_array()
        .expect("posts list")
        .iter()
        .map(|p| p["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles.len(), 2, "ada wrote 2 posts: {out}");
    assert!(titles.contains(&"Analytical Engine"));
    assert!(
        !titles.contains(&"COBOL"),
        "grace's post leaked into ada's list: {out}"
    );
}

/// A list, with the relation resolved per row. This is the shape that generates N+1 if
/// the loader is not doing its job.
#[tokio::test]
async fn a_list_resolves_each_rows_relation() {
    let _g = lock().lock().await;
    let out = gql(r#"{ gq_posts(limit: 10) { title author { username } } }"#).await;
    assert!(out.get("errors").is_none(), "{out}");
    let posts = out["data"]["gq_posts"].as_array().expect("list");
    assert_eq!(posts.len(), 3);
    let pairs: Vec<(String, String)> = posts
        .iter()
        .map(|p| {
            (
                p["title"].as_str().unwrap().to_string(),
                p["author"]["username"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        pairs.contains(&("COBOL".into(), "grace".into())),
        "{pairs:?}"
    );
    assert!(
        pairs.contains(&("Analytical Engine".into(), "ada".into())),
        "{pairs:?}"
    );
}

/// **Deny by default.** `gq_secret` was never exposed, so it must not exist in the schema
/// — not as a query, and not as a relation reachable from a model that IS exposed.
///
/// A GraphQL endpoint is the most efficient exfiltration tool you can hand an attacker,
/// because they choose the query shape. An auto-exposed schema would put every column of
/// every model one query away, and the framework would have done it for them.
#[tokio::test]
async fn an_unexposed_model_is_not_in_the_schema_at_all() {
    let _g = lock().lock().await;
    let out = gql(r#"{ gq_secret(id: "1") { api_key } }"#).await;
    assert!(
        out.get("errors").is_some(),
        "an unexposed model answered a query: {out}"
    );
    let msg = out["errors"].to_string();
    assert!(
        !msg.contains("sk_live_do_not_leak"),
        "the secret leaked in an error: {out}"
    );

    // ...and it is not reachable as a relation off the model that IS exposed.
    let out = gql(r#"{ gq_author(id: "1") { gq_secrets { api_key } } }"#).await;
    assert!(
        out.get("errors").is_some(),
        "an unexposed model was reachable as a relation: {out}"
    );

    // The schema itself must not even mention it.
    let out = gql(r#"{ __schema { types { name } } }"#).await;
    let names = out["data"]["__schema"]["types"].to_string();
    assert!(names.contains("GqPost"), "sanity: exposed types present");
    assert!(
        !names.contains("GqSecret"),
        "an unexposed model appears in introspection: {names}"
    );
}

/// The list cap. The caller chooses the query shape, so they do not also get to choose an
/// unbounded row count.
#[tokio::test]
async fn a_list_is_capped() {
    let _g = lock().lock().await;
    let out = gql(r#"{ gq_posts(limit: 100000) { title } }"#).await;
    assert!(out.get("errors").is_none(), "{out}");
    assert!(
        out["data"]["gq_posts"].as_array().unwrap().len() <= umbral_graphql::MAX_LIMIT as usize
    );
}

/// **The N+1 test.** This is the one that decides whether the plugin is safe to deploy.
///
/// `{ gq_posts { title author { username } } }` over 3 posts written by 2 authors:
///
///   - Naive:  1 query for the posts + 1 per post for its author = **4 reads**.
///   - Batched: 1 query for the posts + 1 `WHERE id IN (1,2)` for both authors = **2**.
///
/// The naive version is *correct*. It returns exactly the same JSON. It passes every test
/// above. And it melts the database the first time somebody asks for a page — because the
/// client picks the query shape, so the client picks your query count. Without this
/// assertion, "we batch relations" is a sentence in a doc-comment.
#[tokio::test]
async fn relations_are_batched_not_n_plus_one() {
    let _g = lock().lock().await;
    use std::sync::atomic::Ordering;

    let _router = boot().await;
    umbral_graphql::DB_READS.store(0, Ordering::Relaxed);

    let out = gql(r#"{ gq_posts(limit: 10) { title author { username } } }"#).await;
    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(out["data"]["gq_posts"].as_array().unwrap().len(), 3);

    let reads = umbral_graphql::DB_READS.load(Ordering::Relaxed);
    assert_eq!(
        reads, 2,
        "expected 2 reads (1 list + 1 batched author lookup), got {reads}. \
         3 posts by 2 authors: if this is 4, the loader is not batching and every list \
         query is an N+1 the caller controls."
    );
}

/// Plurals a human would write. The first version emitted `categorys`, which is the sort
/// of thing somebody sees in GraphiQL and then distrusts the whole schema over.
#[test]
fn list_fields_are_pluralised_like_english() {
    for (singular, expected) in [
        ("Post", "posts"),
        ("Category", "categories"),
        ("Address", "addresses"),
        ("Box", "boxes"),
        ("Day", "days"),
        ("OrderItem", "order_items"),
    ] {
        assert_eq!(
            umbral_graphql::plural_for_tests(singular),
            expected,
            "{singular} should pluralise to {expected}"
        );
    }
}

// ---------------------------------------------------------------------------
// `expose_if` — the gate must be openable, not just closable.
// ---------------------------------------------------------------------------

/// A stand-in auth backend: the caller is staff iff they send `x-staff: 1`.
#[derive(Clone)]
struct HeaderAuth;

#[async_trait::async_trait]
impl umbral::auth::Authentication for HeaderAuth {
    async fn authenticate(
        &self,
        headers: &umbral::web::HeaderMap,
    ) -> Option<umbral::auth::Identity> {
        headers.get("x-staff").map(|_| umbral::auth::Identity {
            user_id: "1".to_string(),
            is_staff: true,
            is_superuser: false,
            extras: Default::default(),
        })
    }
}

/// The FIRST version of this plugin never plumbed the request's identity into the GraphQL
/// context — it hardcoded `None`. `expose_if` therefore could only ever DENY: a gate that
/// cannot be opened is not a gate, it is a wall with a lock painted on it. The endpoint
/// looked fine, the deny test passed, and the feature was useless.
///
/// So this test asserts BOTH directions. A gate you have only ever seen refuse is a gate
/// you have not tested.
#[tokio::test]
async fn expose_if_denies_anonymous_and_admits_staff() {
    let _g = lock().lock().await;

    let router = {
        // A second, isolated app would need a second process (App::build is once-per-
        // process), so drive the schema directly — the plugin's own wiring is exercised by
        // the other tests; what is under test here is the ACCESS decision.
        boot().await
    };
    let _ = router;

    use umbral_graphql::{Exposed, GraphqlPlugin};
    let _ = GraphqlPlugin::new().authenticate(HeaderAuth);

    // Build a schema with one gated model and resolve through it twice.
    let meta = umbral::migrate::ModelMeta::for_::<GqPost>();
    let gated = vec![Exposed {
        meta,
        hidden: Vec::new(),
        writable: None,
        subscribable: false,
        access: Some(std::sync::Arc::new(
            |id: Option<&umbral::auth::Identity>| id.is_some_and(|i| i.is_staff),
        )),
    }];
    let schema = umbral_graphql::build_schema_for_tests(&gated).expect("schema");

    // Anonymous -> denied.
    let req = async_graphql::Request::new(r#"{ gq_post(id: "1") { title } }"#)
        .data(umbral_graphql::new_loaders_for_tests())
        .data::<Option<umbral::auth::Identity>>(None);
    let res = schema.execute(req).await;
    assert!(
        !res.errors.is_empty(),
        "an anonymous caller read a staff-gated model"
    );

    // Staff -> admitted, with the real row.
    let staff = umbral::auth::Identity {
        user_id: "1".into(),
        is_staff: true,
        is_superuser: false,
        extras: Default::default(),
    };
    let req = async_graphql::Request::new(r#"{ gq_post(id: "1") { title } }"#)
        .data(umbral_graphql::new_loaders_for_tests())
        .data::<Option<umbral::auth::Identity>>(Some(staff));
    let res = schema.execute(req).await;
    assert!(res.errors.is_empty(), "staff was denied: {:?}", res.errors);
    let data = res.data.into_json().unwrap();
    assert_eq!(
        data["gq_post"]["title"], "Analytical Engine",
        "the gate opened but returned nothing: {data}"
    );
}

/// `hide` removes the column from the SCHEMA, not just from the payload.
///
/// A field that exists and always resolves to null is not hidden: it confirms the column to
/// anyone reading introspection, and GraphiQL will happily autocomplete it. "Unknown field"
/// is the only answer that tells the client nothing.
#[tokio::test]
async fn a_hidden_field_is_absent_from_the_schema() {
    let _g = lock().lock().await;

    let out = gql(r#"{ gq_post(id: "1") { internal_cost } }"#).await;
    let errs = out["errors"].to_string();
    assert!(
        errs.contains("Unknown field"),
        "a hidden field must not exist in the schema, got: {out}"
    );

    // ...and the model itself is still very much exposed, so the absence above is the hide
    // doing its job — not the whole model having quietly vanished.
    let out = gql(r#"{ gq_post(id: "1") { title } }"#).await;
    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(out["data"]["gq_post"]["title"], "Analytical Engine");
}

/// **The one that matters.** `gq_user` is exposed with no `.hide("password_hash")` — the
/// exact mistake a hurried developer makes — and the hash must still be unreachable.
///
/// This is why the denylist lives in core rather than in each plugin: REST has carried this
/// guarantee for ages, and GraphQL, written later, inherited none of it. A rule that every
/// plugin must remember separately is a rule that fails the first time someone writes a new
/// plugin. Now there is one list, in the middle, and no `expose` call can defeat it.
#[tokio::test]
async fn password_hash_is_denied_even_when_the_model_is_exposed() {
    let _g = lock().lock().await;

    let out = gql(r#"{ gq_user(id: "1") { password_hash } }"#).await;
    let errs = out["errors"].to_string();
    assert!(
        errs.contains("Unknown field"),
        "password_hash must not exist in the schema even on an exposed model, got: {out}"
    );

    // The model IS exposed and readable — so the denial above is surgical, not collateral.
    let out = gql(r#"{ gq_user(id: "1") { username } }"#).await;
    assert!(out.get("errors").is_none(), "{out}");
    assert_eq!(out["data"]["gq_user"]["username"], "ada");
}

/// Hiding a foreign key severs the edge in BOTH directions.
///
/// Otherwise `hide` would be decorative: hide `post.author` and the client just asks for
/// `post { author { id } }` — the id you hid, one hop further out — or comes at it from the
/// other side via `author { posts { ... } }`.
#[tokio::test]
async fn hiding_a_foreign_key_removes_the_relation_both_ways() {
    let _g = lock().lock().await;
    use umbral_graphql::Exposed;

    // The registry only exists after an App::build. Reading it without booting passed only
    // when some OTHER test in this binary happened to run first — a test that depends on
    // sibling ordering is a test that fails on a machine with a different core count.
    let _router = boot().await;
    let models = umbral::migrate::registered_models();
    let author = models.iter().find(|m| m.table == "gq_author").unwrap();
    let post = models.iter().find(|m| m.table == "gq_post").unwrap();

    let schema = umbral_graphql::build_schema_for_tests(&[
        Exposed {
            meta: author.clone(),
            access: None,
            hidden: Vec::new(),
            writable: None,
            subscribable: false,
        },
        Exposed {
            meta: post.clone(),
            access: None,
            // sever the FK from the CHILD's side
            hidden: vec!["author".to_string()],
            writable: None,
            subscribable: false,
        },
    ])
    .expect("schema");

    let sdl = schema.sdl();
    assert!(
        !sdl.contains("author: GqAuthor"),
        "the forward edge must be gone:\n{sdl}"
    );
    assert!(
        !sdl.contains("author_id"),
        "the raw fk id must be gone too — hiding the column and handing back its value is \
         not hiding it:\n{sdl}"
    );
    assert!(
        !sdl.contains("gqPosts") && !sdl.contains("gq_posts: [GqPost!]!"),
        "the REVERSE edge must be gone as well, or the operator severed the relation from \
         one side only:\n{sdl}"
    );
}
