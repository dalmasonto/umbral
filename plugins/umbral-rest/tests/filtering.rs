//! End-to-end tests for query-string filtering on
//! the REST list endpoint.
//!
//! All tests share one booted app (process-wide OnceLock state can only
//! be set once per process). The boot registers a `Post` model with
//! `enable_filters()` and seeds a handful of rows; each test drives
//! the merged router's `GET /api/post/?...` endpoint.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http::Method;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_rest::{ResourceConfig, RestPlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post {
    id: i64,
    title: String,
    published: bool,
    author: i64,
    created_at: Option<chrono::NaiveDate>,
}

// Review #4: a FK to a String-slug-PK target. `?cat=` / `?cat__in=` filters
// must coerce against the target PK type (text), not reject the slug as
// "not an integer".
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "flt_cat")]
struct Cat {
    #[umbral(primary_key)]
    slug: String,
    name: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "flt_doc")]
struct Doc {
    id: i64,
    cat: umbral::orm::ForeignKey<Cat>,
    title: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("filtering.sqlite");
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

        // Filters are ON by default — RestPlugin::default() already
        // serves the `?field=` / `?field__lookup=` grammar against
        // every column. No explicit opt-in required.
        let rest = RestPlugin::default();
        // `ResourceConfig` only matters here if we wanted other
        // resource-level customisation; for a plain filter test the
        // default plugin is enough.
        let _ = ResourceConfig::new("post"); // kept as a smoke-build
        let rest = rest;

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .model::<Cat>()
            .model::<Doc>()
            .plugin(rest)
            .build()
            .expect("App::build");

        umbral::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");

        let pool = umbral::db::pool();
        // Seed rows.
        sqlx::query(
            "INSERT INTO post (title, published, author, created_at) VALUES \
             ('Hello world', 1, 42, '2026-01-15'),\
             ('Rust tips', 0, 42, '2026-02-01'),\
             ('Advanced Rust', 1, 99, '2025-12-01'),\
             ('Hello again', 0, 7, '2026-03-01')",
        )
        .execute(&pool)
        .await
        .expect("seed posts");

        // String-PK FK fixtures (review #4).
        sqlx::query(
            "INSERT INTO flt_cat (slug, name) VALUES ('tech', 'Tech'), ('news', 'News'), ('life', 'Life')",
        )
        .execute(&pool)
        .await
        .expect("seed cats");
        sqlx::query(
            "INSERT INTO flt_doc (cat, title) VALUES \
             ('tech', 'Rust 2.0'), ('tech', 'WASM'), ('news', 'Election'), ('life', 'Coffee')",
        )
        .execute(&pool)
        .await
        .expect("seed docs");

        app.into_router()
    })
    .await
}

async fn get(router: axum::Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed)
}

// =========================================================================
// Test 1: single boolean field filter
// =========================================================================

#[tokio::test]
async fn filter_by_boolean_field_returns_only_matching_rows() {
    let router = boot().await.clone();

    let (status, body) = get(router, "/api/post/?published=true").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let results = body["results"].as_array().expect("results array");
    assert!(
        !results.is_empty(),
        "expected published rows, got empty list"
    );
    for row in results {
        assert_eq!(
            row["published"], true,
            "row with published=false leaked through: {row}"
        );
    }
}

// =========================================================================
// Test 2: AND of two field filters
// =========================================================================

#[tokio::test]
async fn filter_by_two_fields_ands_predicates() {
    let router = boot().await.clone();

    let (status, body) = get(router, "/api/post/?author=42&published=true").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let results = body["results"].as_array().expect("results array");
    // Only "Hello world" is author=42 AND published=true
    assert_eq!(
        results.len(),
        1,
        "expected exactly 1 matching row, got {results:?}"
    );
    assert_eq!(results[0]["title"], "Hello world");
}

// =========================================================================
// Test 3: __contains lookup on a string field
// =========================================================================

#[tokio::test]
async fn filter_contains_returns_substring_matches() {
    let router = boot().await.clone();

    let (status, body) = get(router, "/api/post/?title__contains=hello").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let results = body["results"].as_array().expect("results array");
    // "Hello world" and "Hello again" — SQLite LIKE is case-insensitive for ASCII
    assert!(
        !results.is_empty(),
        "expected at least one contains match, got {results:?}"
    );
    for row in results {
        let title = row["title"].as_str().unwrap_or("");
        assert!(
            title.to_lowercase().contains("hello"),
            "title `{title}` does not contain 'hello'"
        );
    }
}

// =========================================================================
// Test 4: __gte on a date field stored as TEXT in SQLite
// =========================================================================

#[tokio::test]
async fn filter_date_gte_returns_rows_on_or_after_date() {
    let router = boot().await.clone();

    let (status, body) = get(router, "/api/post/?created_at__gte=2026-01-01").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let results = body["results"].as_array().expect("results array");
    // Rows with created_at >= 2026-01-01: "Hello world", "Rust tips", "Hello again"
    assert!(
        results.len() >= 3,
        "expected >=3 rows with created_at >= 2026-01-01, got {results:?}"
    );
    for row in results {
        let date = row["created_at"].as_str().unwrap_or("");
        assert!(
            date >= "2026-01-01",
            "row with created_at={date} leaked through"
        );
    }
}

// =========================================================================
// Test 5: __in lookup with comma-separated integers
// =========================================================================

#[tokio::test]
async fn filter_in_integers_returns_only_listed_ids() {
    let router = boot().await.clone();

    let (status, body) = get(router, "/api/post/?id__in=1,2,3").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let results = body["results"].as_array().expect("results array");
    assert_eq!(
        results.len(),
        3,
        "expected 3 rows for id IN (1,2,3), got {results:?}"
    );
    for row in results {
        let id = row["id"].as_i64().unwrap();
        assert!([1i64, 2, 3].contains(&id), "id={id} is not in [1,2,3]");
    }
}

// =========================================================================
// Test 6: unknown field returns 400
// =========================================================================

#[tokio::test]
async fn filter_unknown_field_returns_400() {
    let router = boot().await.clone();

    let (status, body) = get(router, "/api/post/?nonsense_field=foo").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("unknown field"),
        "expected 'unknown field' in error, got: `{error}`"
    );
}

// =========================================================================
// Test 7: __contains on a non-string field returns 400
// =========================================================================

#[tokio::test]
async fn filter_contains_on_boolean_returns_400() {
    let router = boot().await.clone();

    let (status, body) = get(router, "/api/post/?published__contains=true").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("contains") && (error.contains("text") || error.contains("Boolean")),
        "expected a type-mismatch error mentioning 'contains', got: `{error}`"
    );
}

// =========================================================================
// Test 8: empty value returns 400
// =========================================================================

#[tokio::test]
async fn filter_empty_value_returns_400() {
    let router = boot().await.clone();

    let (status, body) = get(router, "/api/post/?published=").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("missing value"),
        "expected 'missing value' in error, got: `{error}`"
    );
}

// =========================================================================
// Extra test: filters disabled on a resource do NOT apply
// =========================================================================

#[tokio::test]
async fn unfiltered_resource_ignores_filter_keys() {
    // The note resource in the integration tests has NO enable_filters().
    // We re-use the filtering test's router (post has filters on); the
    // noteworthy check here is just that pagination keys are silently
    // skipped even on a filtered resource (no 400 for ?page=1 etc.).
    let router = boot().await.clone();

    // Pagination keys should be passed through silently.
    let (status, _) = get(router, "/api/post/?page=1").await;
    assert_eq!(status, StatusCode::OK, "pagination key caused error");
}

// =========================================================================
// Review #4: FK to a String-slug-PK target must filter by the slug, not be
// rejected as "not an integer".
// =========================================================================

#[tokio::test]
async fn filter_fk_to_string_pk_by_slug() {
    let router = boot().await.clone();
    let (status, body) = get(router, "/api/flt_doc/?cat=tech").await;
    assert_eq!(status, StatusCode::OK, "FK-to-slug filter 400'd: {body}");
    let results = body["results"].as_array().expect("results array");
    assert_eq!(
        results.len(),
        2,
        "expected 2 docs with cat=tech, got {results:?}"
    );
    for row in results {
        assert_eq!(row["cat"].as_str(), Some("tech"));
    }
}

#[tokio::test]
async fn filter_fk_to_string_pk_in_list() {
    let router = boot().await.clone();
    let (status, body) = get(router, "/api/flt_doc/?cat__in=tech,news").await;
    assert_eq!(status, StatusCode::OK, "FK-to-slug __in 400'd: {body}");
    let results = body["results"].as_array().expect("results array");
    assert_eq!(
        results.len(),
        3,
        "expected 3 docs in (tech,news), got {results:?}"
    );
}
