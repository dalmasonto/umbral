//! End-to-end coverage for umbral-openapi. Boot the App once with
//! RestPlugin + OpenApiPlugin + AuthPlugin (so the default block-list
//! test has something to hide) + a Note model, then hit the two
//! generated routes through axum's `oneshot`.

#![allow(dead_code, private_interfaces)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbral_auth::{AuthPlugin, AuthUser};
use umbral_openapi::OpenApiPlugin;
use umbral_rest::RestPlugin;

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Note {
    id: i64,
    title: String,
    body: String,
    published_at: Option<DateTime<Utc>>,
}

// A model with a sensitive column the REST plugin hides. The generated
// spec must NOT advertise `token` anywhere — it would leak a field the
// API never returns (parity with the runtime response strip).
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Secret {
    id: i64,
    label: String,
    token: String,
}

// Review #4: FK + M2M to a String-slug-PK target must render as `string`
// in the OpenAPI schema, not `integer/int64`.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "oa_cat")]
struct OaCat {
    #[umbral(primary_key)]
    slug: String,
    name: String,
}

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "oa_article")]
struct OaArticle {
    id: i64,
    cat: umbral::orm::ForeignKey<OaCat>,
    // OaArticle's own PK is i64 (default P); the CHILD OaCat is String-PK,
    // so the M2M `items` schema must be `string`.
    #[sqlx(skip)]
    #[serde(skip)]
    related: umbral::orm::M2M<OaCat>,
}

// gaps3 #1: a resource scoped to `views([List, Retrieve])` must omit
// `post`/`put`/`patch`/`delete` from the generated spec — the spec
// describes exactly the operations the API serves.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "oa_readonly")]
struct OaReadonly {
    id: i64,
    name: String,
}

static BOOT: OnceCell<axum::Router> = OnceCell::const_new();

async fn boot() -> &'static axum::Router {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("openapi_integration.sqlite");
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

        let app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Note>()
            .model::<Secret>()
            .model::<OaCat>()
            .model::<OaArticle>()
            .model::<OaReadonly>()
            .plugin(AuthPlugin::<AuthUser>::default())
            .plugin(
                RestPlugin::default().hide("secret", "token").resource(
                    umbral_rest::ResourceConfig::new("oa_readonly")
                        .views([umbral_rest::Action::List, umbral_rest::Action::Retrieve]),
                ),
            )
            .plugin(OpenApiPlugin::default())
            .build()
            .expect("App::build with RestPlugin + OpenApiPlugin");

        // Tables aren't needed for the openapi tests; the plugin reads
        // the registry not the DB. But create note so the rest layer
        // wouldn't choke if a test hits it.
        let pool = umbral::db::pool();
        sqlx::query(
            "CREATE TABLE note (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                title TEXT NOT NULL,\
                body TEXT NOT NULL,\
                published_at TEXT\
             )",
        )
        .execute(&pool)
        .await
        .expect("create note");

        sqlx::query(
            "CREATE TABLE secret (\
                id INTEGER PRIMARY KEY AUTOINCREMENT,\
                label TEXT NOT NULL,\
                token TEXT NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create secret");

        app.into_router()
    })
    .await
}

async fn get_request(router: axum::Router, uri: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

// =========================================================================
// 1. Valid OpenAPI 3.0 envelope.
// =========================================================================

/// audit_2 plugin-observability #9 — the Swagger UI page, served with the
/// default (pinned unpkg) asset base, must carry SRI `integrity` hashes on both
/// the CSS and the JS so a tampered CDN response is refused by the browser.
#[tokio::test]
async fn swagger_ui_default_assets_carry_sri_integrity() {
    let router = boot().await.clone();
    let (status, body) = get_request(router, "/openapi/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("swagger-ui.css\" integrity=\"sha384-"),
        "the CSS <link> must carry an SRI integrity hash;\ngot:\n{body}"
    );
    assert!(
        body.contains("swagger-ui-bundle.js\" integrity=\"sha384-"),
        "the JS <script> must carry an SRI integrity hash;\ngot:\n{body}"
    );
}

#[tokio::test]
async fn openapi_json_serves_a_valid_openapi_3_0_document() {
    let router = boot().await.clone();
    let (status, body) = get_request(router, "/openapi/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).expect("body is JSON");
    let openapi = v["openapi"].as_str().expect("openapi field is a string");
    assert!(
        openapi.starts_with("3.0"),
        "expected an OpenAPI 3.0.x doc, got {openapi}"
    );
    assert!(v["info"].is_object(), "info should be an object");
    assert!(v["paths"].is_object(), "paths should be an object");
    assert!(
        v["components"].is_object(),
        "components should be an object"
    );
}

// =========================================================================
// 2. Models surface as schemas.
// =========================================================================

#[tokio::test]
async fn every_registered_model_appears_in_components_schemas() {
    let router = boot().await.clone();
    let (_, body) = get_request(router, "/openapi/openapi.json").await;
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let schemas = v["components"]["schemas"]
        .as_object()
        .expect("components.schemas is an object");
    assert!(
        schemas.contains_key("Note"),
        "expected Note in schemas; got {:?}",
        schemas.keys().collect::<Vec<_>>()
    );
    // The note schema should have the four columns as properties.
    let note = &schemas["Note"];
    let props = note["properties"].as_object().expect("properties");
    for field in ["id", "title", "body", "published_at"] {
        assert!(props.contains_key(field), "Note missing property `{field}`");
    }
    // published_at is nullable; the schema should say so.
    assert_eq!(
        note["properties"]["published_at"]["nullable"], true,
        "published_at should be nullable"
    );
    // required = non-null non-PK columns. id (PK) and published_at
    // (nullable) are out; title + body must be in.
    let required = note["required"].as_array().expect("required is array");
    let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
    assert!(names.contains(&"title"), "title should be required");
    assert!(names.contains(&"body"), "body should be required");
    assert!(!names.contains(&"id"), "id (PK) should NOT be required");
    assert!(
        !names.contains(&"published_at"),
        "published_at (nullable) should NOT be required"
    );
}

// =========================================================================
// 3. auth_user is hidden by default.
// =========================================================================

#[tokio::test]
async fn default_block_list_keeps_auth_user_out_of_the_spec() {
    let router = boot().await.clone();
    let (_, body) = get_request(router, "/openapi/openapi.json").await;
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let schemas = v["components"]["schemas"].as_object().expect("schemas");
    assert!(
        !schemas.contains_key("AuthUser"),
        "AuthUser should be hidden by the default block-list; got {:?}",
        schemas.keys().collect::<Vec<_>>()
    );
    let paths = v["paths"].as_object().expect("paths");
    assert!(
        !paths.contains_key("/api/auth_user/"),
        "/api/auth_user/ should be absent from paths"
    );
}

// =========================================================================
// 3b. REST-hidden fields are excluded from the schema, the ?fields=
//     picker, and required — parity with the runtime response strip.
// =========================================================================

#[tokio::test]
async fn rest_hidden_field_is_excluded_from_the_model_schema() {
    let router = boot().await.clone();
    let (_, body) = get_request(router, "/openapi/openapi.json").await;
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let secret = &v["components"]["schemas"]["Secret"];
    let props = secret["properties"]
        .as_object()
        .expect("Secret schema properties");

    // The hidden column must not appear as a property...
    assert!(
        !props.contains_key("token"),
        "hidden `token` leaked into the Secret schema properties: {:?}",
        props.keys().collect::<Vec<_>>()
    );
    // ...nor in `required` (token is non-null non-PK, so without the
    // hide filter it WOULD have been required).
    if let Some(required) = secret["required"].as_array() {
        let names: Vec<&str> = required.iter().filter_map(|x| x.as_str()).collect();
        assert!(
            !names.contains(&"token"),
            "hidden `token` leaked into Secret.required: {names:?}"
        );
    }
    // A visible field is still present — proves we didn't over-filter.
    assert!(
        props.contains_key("label"),
        "non-hidden `label` should still be a property; got {:?}",
        props.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn rest_hidden_field_is_excluded_from_the_fields_picker() {
    let router = boot().await.clone();
    let (_, body) = get_request(router, "/openapi/openapi.json").await;
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");

    // The `?fields=` parameter on the Secret list endpoint advertises
    // its columns via `x-umbral-fields-columns`. The hidden `token`
    // must NOT be offered (you can never get it back), but `label`
    // must remain.
    let list_params = v["paths"]["/api/secret/"]["get"]["parameters"]
        .as_array()
        .expect("list params array");
    let fields_param = list_params
        .iter()
        .find(|p| p["name"] == "fields")
        .expect("fields parameter present on /api/secret/ list op");
    let cols: Vec<&str> = fields_param["x-umbral-fields-columns"]
        .as_array()
        .expect("x-umbral-fields-columns array")
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    assert!(
        !cols.contains(&"token"),
        "hidden `token` should not be offered in the ?fields= picker; got {cols:?}"
    );
    assert!(
        cols.contains(&"label"),
        "visible `label` should still be in the ?fields= picker; got {cols:?}"
    );
}

// =========================================================================
// 4. All six REST operations appear with the right shape.
// =========================================================================

#[tokio::test]
async fn every_rest_operation_appears_in_paths() {
    let router = boot().await.clone();
    let (_, body) = get_request(router, "/openapi/openapi.json").await;
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let paths = v["paths"].as_object().expect("paths");

    let collection = paths
        .get("/api/note/")
        .expect("/api/note/ should be present");
    assert!(
        collection["get"]["operationId"].as_str() == Some("list_note"),
        "list_note operationId missing"
    );
    assert!(
        collection["post"]["operationId"].as_str() == Some("create_note"),
        "create_note operationId missing"
    );
    // 201 is the success code on create.
    assert!(
        collection["post"]["responses"]["201"].is_object(),
        "POST should advertise 201"
    );

    let item = paths
        .get("/api/note/{id}")
        .expect("/api/note/{id} should be present");
    let ops = [
        ("get", "retrieve_note", "200"),
        ("put", "update_note", "200"),
        ("patch", "partial_update_note", "200"),
        ("delete", "destroy_note", "204"),
    ];
    for (verb, op_id, success_code) in ops {
        let op = item.get(verb).unwrap_or_else(|| panic!("missing {verb}"));
        assert_eq!(
            op["operationId"].as_str(),
            Some(op_id),
            "{verb}'s operationId should be {op_id}"
        );
        assert!(
            op["responses"][success_code].is_object(),
            "{verb} should advertise {success_code}"
        );
        // GET/PUT/PATCH/DELETE on the item URL all 404 on miss.
        assert!(
            op["responses"]["404"].is_object(),
            "{verb} should advertise 404"
        );
    }
}

// =========================================================================
// 4b. `views([List, Retrieve])` omits write operations from the spec (gaps3 #1).
// =========================================================================

#[tokio::test]
async fn view_scoped_resource_omits_write_operations() {
    let router = boot().await.clone();
    let (_, body) = get_request(router, "/openapi/openapi.json").await;
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let paths = v["paths"].as_object().expect("paths");

    // The collection keeps `get` (List) but drops `post` (Create).
    let collection = paths
        .get("/api/oa_readonly/")
        .expect("read-only collection still present (it serves GET)");
    assert!(
        collection.get("get").is_some(),
        "List is exposed → `get` present: {collection}"
    );
    assert!(
        collection.get("post").is_none(),
        "Create scoped out → `post` must be absent: {collection}"
    );

    // The detail URI keeps `get` (Retrieve) but drops put/patch/delete.
    let item = paths
        .get("/api/oa_readonly/{id}")
        .expect("read-only detail still present (it serves GET)");
    assert!(
        item.get("get").is_some(),
        "Retrieve exposed → `get` present"
    );
    for verb in ["put", "patch", "delete"] {
        assert!(
            item.get(verb).is_none(),
            "{verb} scoped out → must be absent from the detail path item: {item}"
        );
    }
}

// =========================================================================
// 5. Swagger UI page.
// =========================================================================

#[tokio::test]
async fn swagger_ui_html_page_loads_and_references_the_spec_url() {
    let router = boot().await.clone();
    let (status, body) = get_request(router, "/openapi/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("/openapi/openapi.json"),
        "swagger UI should point at /openapi/openapi.json; got\n{body}"
    );
    assert!(
        body.contains("swagger-ui"),
        "expected swagger-ui asset references in body"
    );
}

// =========================================================================
// 6. .at("/api/docs") changes both routes. App::build's globals are
// already populated by the shared OnceCell boot above, so we can't
// boot a second App. Instead inspect the routes the plugin contributes.
// =========================================================================

#[test]
fn base_path_override_changes_both_routes() {
    // Can't reuse the boot's OnceCell here (different base_path) and
    // can't safely boot a second App in the same test binary (the
    // CONFIG OnceLock + the framework's global model registry are
    // both single-set). Inspect the URL helpers the plugin uses to
    // register its routes instead. Equivalent to checking what the
    // Router would receive.
    let with_slash = OpenApiPlugin::new().at("/api/docs/");
    let without_slash = OpenApiPlugin::new().at("/api/docs");
    // Both with-and-without-trailing-slash inputs normalise to the
    // same base.
    assert_eq!(with_slash.spec_url_for_test(), "/api/docs/openapi.json");
    assert_eq!(with_slash.ui_route_for_test(), "/api/docs/");
    assert_eq!(without_slash.spec_url_for_test(), "/api/docs/openapi.json");
    assert_eq!(without_slash.ui_route_for_test(), "/api/docs/");

    let default_plugin = OpenApiPlugin::new();
    assert_eq!(default_plugin.spec_url_for_test(), "/openapi/openapi.json");
    assert_eq!(default_plugin.ui_route_for_test(), "/openapi/");
}

// Test-only accessor: spec_url / ui_route are private. Expose them
// through an extension trait so the assertion above doesn't have to
// fight axum's opaque Router type for path strings.
trait PluginInspect {
    fn spec_url_for_test(&self) -> String;
    fn ui_route_for_test(&self) -> String;
}

impl PluginInspect for OpenApiPlugin {
    fn spec_url_for_test(&self) -> String {
        umbral_openapi::test_spec_url(self)
    }
    fn ui_route_for_test(&self) -> String {
        umbral_openapi::test_ui_route(self)
    }
}

// =========================================================================
// gaps2 #79a: default boot uses NoPagination → no page/page_size params.
// =========================================================================

#[tokio::test]
async fn no_pagination_style_emits_no_pagination_params() {
    let (_, body) = get_request(boot().await.clone(), "/openapi/openapi.json").await;
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let list_params = v["paths"]["/api/note/"]["get"]["parameters"]
        .as_array()
        .expect("list params array on /api/note/");

    let param_names: Vec<&str> = list_params
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();

    assert!(
        !param_names.contains(&"page"),
        "NoPagination should not emit a `page` param; got {param_names:?}"
    );
    assert!(
        !param_names.contains(&"page_size"),
        "NoPagination should not emit a `page_size` param; got {param_names:?}"
    );
}

// =========================================================================
// gaps2 #79b: the spec paths match the real REST base path (default /api).
// =========================================================================

#[tokio::test]
async fn spec_paths_use_registered_rest_base_path() {
    let (_, body) = get_request(boot().await.clone(), "/openapi/openapi.json").await;
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let paths = v["paths"].as_object().expect("paths");

    // The default RestPlugin uses /api — the paths must reflect that.
    assert!(
        paths.contains_key("/api/note/"),
        "/api/note/ must be in paths when REST base is /api; keys: {:?}",
        paths.keys().collect::<Vec<_>>()
    );
    assert!(
        paths.contains_key("/api/note/{id}"),
        "/api/note/{{id}} must be in paths; keys: {:?}",
        paths.keys().collect::<Vec<_>>()
    );
    // No hardcoded fallback path should appear if /api is the live base.
    // (Both would appear if we ever accidentally duplicated with a different base.)
    let non_api: Vec<&str> = paths
        .keys()
        .map(|k| k.as_str())
        .filter(|k| k.starts_with("/note/"))
        .collect();
    assert!(
        non_api.is_empty(),
        "No bare /note/ paths should appear (only /api/note/); got {non_api:?}"
    );
}

// =========================================================================
// Review #4: FK + M2M to a String-slug-PK target render as `string`.
// =========================================================================

#[tokio::test]
async fn fk_and_m2m_to_string_pk_render_as_string_schema() {
    let (status, body) = get_request(boot().await.clone(), "/openapi/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).expect("valid json");
    let props = &v["components"]["schemas"]["OaArticle"]["properties"];

    // FK to a String-slug-PK target → string (was integer/int64).
    assert_eq!(
        props["cat"]["type"], "string",
        "FK to a String-PK target must be `string`; got {}",
        props["cat"]
    );
    // M2M whose CHILD is String-PK → array of string items.
    assert_eq!(
        props["related"]["type"], "array",
        "M2M is an array; got {}",
        props["related"]
    );
    assert_eq!(
        props["related"]["items"]["type"], "string",
        "M2M to a String-PK child → string items; got {}",
        props["related"]
    );
}
