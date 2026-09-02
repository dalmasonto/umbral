//! gaps4 #72 — the read-side EXPAND: `?expand=<field>` embeds a declared
//! reverse-FK relation as a full child ARRAY, and expands a declared M2M
//! field from a bare id array to full child objects. Symmetric to
//! `ResourceConfig::nested(...)`'s WRITE-side (POST) behavior, but for GET.
//!
//! Fixture: `Developer` is the parent. `Project` FKs INTO `Developer`
//! (reverse-FK, declared embeddable via `.embed("projects", "project")`).
//! `Developer` also carries an M2M `favorite_software` (declared expandable
//! via `.expand_m2m("favorite_software")`). Both `project.secret_budget`
//! and `software.internal_cost` are hidden columns — the test proves they
//! never leak through the expanded arrays.
//!
//! Query counting (self-contained, mirrors
//! `crates/umbral-core/tests/query_counts.rs`'s tracing-layer technique but
//! scoped to this binary) proves the list-page expand is BATCHED: the SQL
//! statement count for `GET /api/developer/?expand=...` does not grow when
//! the number of developers (and their projects/software) triples.

#![allow(dead_code, private_interfaces)]

use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use tracing::Subscriber;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

use umbral::orm::{ForeignKey, M2M};
use umbral_rest::{AllowAny, ResourceConfig, RestPlugin};

// =========================================================================
// Query-count harness (self-contained copy of the umbral-core technique —
// this is a separate test binary so a private static counter is safe).
// =========================================================================

static QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);
static INIT: Once = Once::new();
static COUNT_LOCK: Mutex<()> = Mutex::const_new(());

struct CountLayer;

struct StmtVisitor(Option<String>);
impl tracing::field::Visit for StmtVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "summary" || field.name() == "db.statement" {
            self.0 = Some(format!("{value:?}"));
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "summary" || field.name() == "db.statement" {
            self.0 = Some(value.to_string());
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for CountLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if !event.metadata().target().starts_with("sqlx::query") {
            return;
        }
        let mut v = StmtVisitor(None);
        event.record(&mut v);
        let stmt = v.0.unwrap_or_default();
        // Connection-setup PRAGMAs fire lazily on first use of a fresh pool
        // connection — not application DML/DQL. Excluding them is what makes
        // the count deterministic regardless of which GET happens to be the
        // one that warms a new connection (mirrors
        // `crates/umbral-core/tests/query_counts.rs`'s `CountLayer`).
        if stmt.trim_start().to_ascii_uppercase().starts_with("PRAGMA") {
            return;
        }
        QUERY_COUNT.fetch_add(1, Ordering::SeqCst);
    }
}

fn install_counter() {
    INIT.call_once(|| {
        tracing_subscriber::registry()
            .with(LevelFilter::TRACE)
            .with(CountLayer)
            .init();
    });
}

async fn count_lock() -> tokio::sync::MutexGuard<'static, ()> {
    install_counter();
    COUNT_LOCK.lock().await
}

fn reset_count() {
    QUERY_COUNT.store(0, Ordering::SeqCst);
}

fn query_count() -> usize {
    QUERY_COUNT.load(Ordering::SeqCst)
}

// =========================================================================
// Models
// =========================================================================

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Software {
    id: i64,
    #[umbral(string)]
    name: String,
    // Hidden via `.hide("software", "internal_cost")` — must never leak
    // through an M2M-expanded `favorite_software` object.
    internal_cost: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Developer {
    id: i64,
    #[umbral(string)]
    name: String,
    #[sqlx(skip)]
    #[serde(skip)]
    favorite_software: M2M<Software>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Project {
    id: i64,
    #[umbral(string)]
    name: String,
    developer: ForeignKey<Developer>,
    // Hidden via `.hide("project", "secret_budget")` — must never leak
    // through a reverse-FK-expanded `projects` array.
    secret_budget: i64,
}

// =========================================================================
// Boot
// =========================================================================

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("expand_read.sqlite");
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

        let rest = RestPlugin::default()
            .default_permission(AllowAny)
            .hide("project", "secret_budget")
            .hide("software", "internal_cost")
            .resource(
                ResourceConfig::for_::<Developer>()
                    .embed("projects", "project")
                    .expand_m2m("favorite_software"),
            );

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Software>()
            .model::<Developer>()
            .model::<Project>()
            .plugin(rest)
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        app.into_router()
    })
    .await
}

async fn get_json(router: axum::Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes).expect("valid json");
    (status, parsed)
}

async fn post_json(router: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes).expect("valid json");
    (status, parsed)
}

/// Seed one developer with `n_projects` projects and the given software ids
/// as favorites. The developer goes through the public REST write path (it
/// has no hidden column, and this exercises the M2M write-through); `Project`
/// carries the hidden `secret_budget` column, and `ResourceConfig::hide` is
/// symmetric (WEB-2: hidden in, hidden out — see `strip_hidden_for_write`),
/// so a hidden, NOT NULL column can't be set through the REST create
/// endpoint at all. Seed it through the typed ORM `Manager` instead — still
/// ORM-only, just below the REST layer rather than through it. Returns the
/// developer's id.
async fn seed_developer(
    router: axum::Router,
    name: &str,
    software_ids: &[i64],
    n_projects: usize,
) -> i64 {
    let (status, dev) = post_json(
        router.clone(),
        "/api/developer/",
        json!({ "name": name, "favorite_software": software_ids }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "seed developer: {dev}");
    let dev_id = dev["id"].as_i64().expect("developer id");
    for i in 0..n_projects {
        Project::objects()
            .create(Project {
                id: 0,
                name: format!("{name}-project-{i}"),
                developer: ForeignKey::new(dev_id),
                secret_budget: 1000 + i as i64,
            })
            .await
            .expect("seed project via ORM");
    }
    dev_id
}

/// `Software.internal_cost` is hidden too — same reasoning as `Project`
/// above, seed via the typed ORM `Manager`, not the REST create endpoint.
async fn seed_software(name: &str, cost: i64) -> i64 {
    let created = Software::objects()
        .create(Software {
            id: 0,
            name: name.to_string(),
            internal_cost: cost,
        })
        .await
        .expect("seed software via ORM");
    created.id
}

// =========================================================================
// 1. No `?expand=` — unchanged shape (no regression).
// =========================================================================

#[tokio::test]
async fn without_expand_response_is_unchanged() {
    let router = boot().await.clone();
    let rust_id = seed_software("rust-1", 10).await;
    let pg_id = seed_software("pg-1", 20).await;
    let dev_id = seed_developer(router.clone(), "ada-1", &[rust_id, pg_id], 2).await;

    let (status, body) = get_json(router, &format!("/api/developer/{dev_id}")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    // M2M still a bare id array — expand wasn't asked for.
    let ids: Vec<i64> = body["favorite_software"]
        .as_array()
        .expect("favorite_software array")
        .iter()
        .map(|v| v.as_i64().expect("bare id"))
        .collect();
    assert_eq!(ids.len(), 2, "bare id array, not expanded objects: {body}");
    // No declared-but-unrequested reverse-FK key at all.
    assert!(
        body.get("projects").is_none(),
        "`projects` must not appear without `?expand=projects`: {body}"
    );
}

// =========================================================================
// 2. Detail `?expand=` — reverse-FK array + M2M expansion, hidden columns
//    stripped from both.
// =========================================================================

#[tokio::test]
async fn expand_embeds_reverse_fk_array_and_expands_m2m_on_retrieve() {
    let router = boot().await.clone();
    let rust_id = seed_software("rust-2", 11).await;
    let pg_id = seed_software("pg-2", 22).await;
    let dev_id = seed_developer(router.clone(), "ada-2", &[rust_id, pg_id], 2).await;

    let (status, body) = get_json(
        router,
        &format!("/api/developer/{dev_id}?expand=projects,favorite_software"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // Reverse-FK: `projects` is now an array of full child objects.
    let projects = body["projects"].as_array().expect("projects array");
    assert_eq!(projects.len(), 2, "both projects embedded: {body}");
    for p in projects {
        assert!(
            p.get("secret_budget").is_none(),
            "DATA LEAK: hidden `secret_budget` leaked through expanded `projects`: {p:?}"
        );
        assert!(
            p.get("name").is_some(),
            "non-hidden field should survive: {p:?}"
        );
        assert_eq!(
            p.get("developer").and_then(|v| v.as_i64()),
            Some(dev_id),
            "expanded child keeps its own FK column"
        );
    }

    // M2M: `favorite_software` is now full objects, not bare ids.
    let software = body["favorite_software"]
        .as_array()
        .expect("favorite_software array");
    assert_eq!(software.len(), 2, "both software expanded: {body}");
    let names: Vec<&str> = software
        .iter()
        .map(|s| s["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"rust-2"));
    assert!(names.contains(&"pg-2"));
    for s in software {
        assert!(
            s.get("internal_cost").is_none(),
            "DATA LEAK: hidden `internal_cost` leaked through expanded `favorite_software`: {s:?}"
        );
    }
}

// =========================================================================
// 3. Undeclared `?expand=` name -> 400, loud not silent.
// =========================================================================

#[tokio::test]
async fn undeclared_expand_name_is_bad_input() {
    let router = boot().await.clone();
    let dev_id = seed_developer(router.clone(), "ada-3", &[], 0).await;
    let (status, body) = get_json(router, &format!("/api/developer/{dev_id}?expand=nope")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}

// =========================================================================
// 4. LIST `?expand=` — correctness per-row (no cross-developer bleed) AND
//    batching (flat query count as the row count triples).
// =========================================================================

#[tokio::test]
async fn list_expand_batches_and_scopes_children_per_row() {
    let _g = count_lock().await;
    let router = boot().await.clone();

    // `BOOT` (and its underlying table) is shared across every `#[tokio::test]`
    // in this binary, so scope the LIST query to a name prefix unique to this
    // test — otherwise rows other tests seeded would inflate `results.len()`
    // and break the exact-count assertions below (it would NOT, however,
    // affect the query-count proof: that invariant holds regardless of how
    // many unrelated rows exist, which is the whole point).
    let list_uri = "/api/developer/?expand=projects,favorite_software&name__startswith=batchdev-";

    // First page: 2 developers, one project + one favorite each.
    let sw_a = seed_software("batchsw-a", 1).await;
    let sw_b = seed_software("batchsw-b", 2).await;
    let dev1 = seed_developer(router.clone(), "batchdev-1", &[sw_a], 1).await;
    let dev2 = seed_developer(router.clone(), "batchdev-2", &[sw_b], 1).await;

    reset_count();
    let (status, body) = get_json(router.clone(), list_uri).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let first_count = query_count();
    assert!(
        first_count > 0,
        "the harness must observe at least one statement"
    );

    let results = body["results"].as_array().expect("results array");
    assert_eq!(
        results.len(),
        2,
        "only the 2 seeded-so-far developers: {body}"
    );
    let by_id = |rows: &[Value], id: i64| -> Value {
        rows.iter()
            .find(|r| r["id"].as_i64() == Some(id))
            .unwrap_or_else(|| panic!("developer {id} in results: {rows:?}"))
            .clone()
    };
    // Each developer's `projects` are ITS OWN, not the other's (proves the
    // batch grouping keys correctly rather than fanning every child to
    // every parent).
    let r1 = by_id(results, dev1);
    let r1_projects = r1["projects"].as_array().expect("projects");
    assert_eq!(r1_projects.len(), 1);
    assert!(
        r1_projects[0]["name"]
            .as_str()
            .unwrap()
            .starts_with("batchdev-1")
    );
    let r2 = by_id(results, dev2);
    let r2_projects = r2["projects"].as_array().expect("projects");
    assert_eq!(r2_projects.len(), 1);
    assert!(
        r2_projects[0]["name"]
            .as_str()
            .unwrap()
            .starts_with("batchdev-2")
    );

    // Now triple the row count: 4 more developers (6 total), each with
    // their own project + favorite software.
    for i in 0..4 {
        let sw = seed_software(&format!("batchsw-extra-{i}"), 3 + i).await;
        seed_developer(router.clone(), &format!("batchdev-extra-{i}"), &[sw], 1).await;
    }

    reset_count();
    let (status, body2) = get_json(router.clone(), list_uri).await;
    assert_eq!(status, StatusCode::OK, "body: {body2}");
    let second_count = query_count();
    let results2 = body2["results"].as_array().expect("results array");
    assert_eq!(
        results2.len(),
        6,
        "all six scoped developers present: {body2}"
    );

    assert_eq!(
        first_count, second_count,
        "expand must batch — the SQL statement count ({first_count} -> {second_count}) must \
         stay flat as the developer/project/software row count triples, proving no N+1"
    );
}
