//! umbra-openapi — auto-generated OpenAPI 3.0 schema + Swagger UI.
//!
//! Register [`OpenApiPlugin`] on `App::builder()` alongside
//! `RestPlugin`. The plugin walks the migration registry, drops the
//! tables umbra-rest hides by default, and emits an OpenAPI 3.0
//! document describing every remaining model's REST surface.
//!
//! Default mount point is `/openapi/`:
//!
//! - `GET /openapi/openapi.json` — the JSON spec
//! - `GET /openapi/`             — Swagger UI loaded from unpkg
//!
//! Override via `OpenApiPlugin::new().at("/api/docs")` to put the UI
//! under `/api/docs/` and the JSON under `/api/docs/openapi.json`.
//!
//! ## Scope
//!
//! v1 only describes umbra-rest's auto-generated endpoints. Hand-
//! written routes the user added on the builder are not in scope, and
//! the spec carries no `securitySchemes` entries. Pagination is also
//! deferred because umbra-rest does not paginate yet.

use std::sync::OnceLock;

use serde_json::{Map, Value, json};
use umbra::migrate::{Column, ModelMeta};
use umbra::orm::SqlType;
use umbra::prelude::*;
use umbra::web::{Html, IntoResponse, Json, Response, StatusCode, header};

/// Tables umbra-rest hides by default. Mirrored here so the spec does
/// not advertise endpoints the REST plugin refuses to serve.
const DEFAULT_BLOCKED_TABLES: &[&str] = &["auth_user", "session", "umbra_migrations"];

const SWAGGER_UI_HTML: &str = include_str!("../templates/swagger_ui.html");

/// The OpenAPI plugin.
#[derive(Debug, Clone)]
pub struct OpenApiPlugin {
    base_path: String,
    title: String,
    version: String,
    description: Option<String>,
    extra_exclude: Vec<String>,
}

impl Default for OpenApiPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenApiPlugin {
    pub fn new() -> Self {
        Self {
            base_path: "/openapi".to_string(),
            title: "umbra API".to_string(),
            version: "0.0.1".to_string(),
            description: None,
            extra_exclude: Vec::new(),
        }
    }

    /// Mount the JSON + UI under a different base. Trailing slashes
    /// are normalised so both `.at("/api/docs")` and `.at("/api/docs/")`
    /// register the same routes.
    pub fn at(mut self, path: &str) -> Self {
        let trimmed = path.trim_end_matches('/');
        self.base_path = if trimmed.is_empty() {
            "/".to_string()
        } else {
            trimmed.to_string()
        };
        self
    }

    /// Override `info.title` in the emitted spec.
    pub fn title(mut self, s: impl Into<String>) -> Self {
        self.title = s.into();
        self
    }

    /// Override `info.version` in the emitted spec.
    pub fn version(mut self, s: impl Into<String>) -> Self {
        self.version = s.into();
        self
    }

    /// Set `info.description` in the emitted spec. Optional —
    /// omitted from the JSON when unset. Markdown is permitted (per
    /// OpenAPI 3.0.3); Swagger UI renders it above the operations
    /// list, so this is the place to document API-wide auth, rate
    /// limiting, conventions, etc.
    pub fn description(mut self, s: impl Into<String>) -> Self {
        self.description = Some(s.into());
        self
    }

    /// Add tables to the block-list. The umbra-rest defaults still
    /// apply.
    pub fn exclude<I, S>(mut self, tables: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for t in tables {
            self.extra_exclude.push(t.into());
        }
        self
    }

    fn is_exposed(&self, table: &str) -> bool {
        if DEFAULT_BLOCKED_TABLES.contains(&table) {
            return false;
        }
        !self.extra_exclude.iter().any(|t| t == table)
    }

    fn spec_url(&self) -> String {
        if self.base_path == "/" {
            "/openapi.json".to_string()
        } else {
            format!("{}/openapi.json", self.base_path)
        }
    }

    fn ui_route(&self) -> String {
        if self.base_path == "/" {
            "/".to_string()
        } else {
            format!("{}/", self.base_path)
        }
    }
}

// Configured plugin lives in a OnceLock so the static handlers, which
// can't capture per-instance state through axum, can read the title /
// version / block-list at request time.
static CONFIG: OnceLock<OpenApiPlugin> = OnceLock::new();

impl Plugin for OpenApiPlugin {
    fn name(&self) -> &'static str {
        "openapi"
    }

    fn dependencies(&self) -> &'static [&'static str] {
        &["rest"]
    }

    fn routes(&self) -> Router {
        let _ = CONFIG.set(self.clone());
        let mut router = Router::new()
            .route(&self.spec_url(), get(spec_handler))
            .route(&self.ui_route(), get(swagger_ui_handler));
        // Also register the slash-less form (`/openapi` alongside
        // `/openapi/`) so the trailing-slash gotcha doesn't bite users
        // who haven't opted into the framework-wide
        // `App::builder().slash_redirect(SlashRedirect::Append)`
        // policy. Cheap: same handler, no extra state. Skipped when
        // the base path is `/` (the ui_route is already just `/`,
        // no alternate form to register).
        if self.base_path != "/" {
            router = router.route(&self.base_path, get(swagger_ui_handler));
        }
        router
    }
}

// =========================================================================
// Handlers.
// =========================================================================

async fn spec_handler() -> Response {
    let cfg = CONFIG.get().expect("OpenApiPlugin::routes was called");
    let spec = build_spec(cfg);
    // Json's IntoResponse already sets application/json, but be
    // explicit so a future swap to a String body doesn't drop it.
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        Json(spec),
    )
        .into_response()
}

async fn swagger_ui_handler() -> Response {
    let cfg = CONFIG.get().expect("OpenApiPlugin::routes was called");
    let body = SWAGGER_UI_HTML.replace("{SPEC_URL}", &cfg.spec_url());
    Html(body).into_response()
}

// =========================================================================
// Spec generation. Walk the registry, dispatch each SqlType to an
// OpenAPI type/format, and emit one schema + six operations per
// exposed model.
// =========================================================================

fn build_spec(cfg: &OpenApiPlugin) -> Value {
    let mut schemas = Map::new();
    let mut paths = Map::new();

    for plugin in umbra::migrate::registered_plugins() {
        for model in umbra::migrate::models_for_plugin(&plugin) {
            if !cfg.is_exposed(&model.table) {
                continue;
            }
            let schema_name = pascal_case(&model.name);
            schemas.insert(schema_name.clone(), model_schema(&model));
            paths.insert(
                format!("/api/{}/", model.table),
                collection_paths(&model.table, &schema_name),
            );
            paths.insert(
                format!("/api/{}/{{id}}", model.table),
                item_paths(&model.table, &schema_name),
            );
        }
    }

    let mut info = Map::new();
    info.insert("title".into(), Value::String(cfg.title.clone()));
    info.insert("version".into(), Value::String(cfg.version.clone()));
    if let Some(desc) = &cfg.description {
        info.insert("description".into(), Value::String(desc.clone()));
    }

    json!({
        "openapi": "3.0.3",
        "info": Value::Object(info),
        "paths": Value::Object(paths),
        "components": {
            "schemas": Value::Object(schemas),
        },
    })
}

fn model_schema(model: &ModelMeta) -> Value {
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    for col in &model.fields {
        properties.insert(col.name.clone(), column_schema(col));
        // PK is auto-generated by SQLite on POST. Non-nullable non-PK
        // columns are what the client must supply.
        if !col.nullable && !col.primary_key {
            required.push(Value::String(col.name.clone()));
        }
    }
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String("object".into()));
    obj.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        obj.insert("required".into(), Value::Array(required));
    }
    Value::Object(obj)
}

fn column_schema(col: &Column) -> Value {
    let (ty, format) = openapi_type(col.ty);
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String(ty.into()));
    if let Some(f) = format {
        obj.insert("format".into(), Value::String(f.into()));
    }
    if col.nullable {
        obj.insert("nullable".into(), Value::Bool(true));
    }
    Value::Object(obj)
}

fn openapi_type(ty: SqlType) -> (&'static str, Option<&'static str>) {
    match ty {
        SqlType::SmallInt => ("integer", Some("int32")),
        SqlType::Integer => ("integer", Some("int32")),
        SqlType::BigInt => ("integer", Some("int64")),
        SqlType::Real => ("number", Some("float")),
        SqlType::Double => ("number", Some("double")),
        SqlType::Boolean => ("boolean", None),
        SqlType::Text => ("string", None),
        SqlType::Date => ("string", Some("date")),
        SqlType::Time => ("string", Some("time")),
        SqlType::Timestamptz => ("string", Some("date-time")),
        SqlType::Uuid => ("string", Some("uuid")),
        // OpenAPI represents JSON columns as the catch-all "object". A
        // tighter schema would use `oneOf: [object, array]` to model the
        // full JSON value space, but `object` is the conservative and
        // most-tooling-friendly mapping for a first iteration.
        SqlType::Json => ("object", None),
        // Arrays render as `type: array` with an inferred item type in
        // OpenAPI. The v1 mapping flattens the element to the same
        // "type" string (no nested `items.format`) — enough for tools
        // to validate the request shape, but not the full structural
        // detail. A future pass can recurse into the element type via
        // openapi_type for proper `items: { type, format }` nesting.
        SqlType::Array(_) => ("array", None),
        // Phase 4.4 network address types. INET and CIDR render as
        // OpenAPI `ipv4`/`ipv6` strings (we use the generic "string"
        // shape since umbra doesn't distinguish v4 vs v6 at the type
        // level). MACADDR likewise renders as a string.
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr => ("string", None),
        // Phase 4.3 tsvector — opaque text lexeme vector. Render as
        // plain string in the OpenAPI schema.
        SqlType::FullText => ("string", None),
        // ForeignKey columns expose as integer (i64) in the REST/OpenAPI
        // schema — the raw PK value, not a nested object.
        SqlType::ForeignKey => ("integer", Some("int64")),
        // BLOB / BYTEA. OpenAPI's `string` + `format: byte` means
        // base64-encoded on the wire by convention, but umbra-rest's
        // current wire format is a JSON array of u8. Render as
        // `array` + `format: byte` to keep the schema honest about
        // the shape; clients that need base64 can handle the encoding
        // boundary themselves.
        SqlType::Bytes => ("array", Some("byte")),
    }
}

fn collection_paths(table: &str, schema_name: &str) -> Value {
    json!({
        "get": {
            "operationId": format!("list_{}", table),
            "tags": [table],
            "responses": {
                "200": {
                    "description": "List of rows",
                    "content": {
                        "application/json": {
                            "schema": list_envelope(schema_name)
                        }
                    }
                }
            }
        },
        "post": {
            "operationId": format!("create_{}", table),
            "tags": [table],
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": {
                        "schema": schema_ref(schema_name)
                    }
                }
            },
            "responses": {
                "201": {
                    "description": "Row created",
                    "content": {
                        "application/json": {
                            "schema": schema_ref(schema_name)
                        }
                    }
                },
                "400": { "description": "Invalid input" }
            }
        }
    })
}

fn item_paths(table: &str, schema_name: &str) -> Value {
    let id_param = json!({
        "name": "id",
        "in": "path",
        "required": true,
        "schema": { "type": "string" }
    });
    json!({
        "parameters": [id_param],
        "get": {
            "operationId": format!("retrieve_{}", table),
            "tags": [table],
            "responses": {
                "200": {
                    "description": "Row found",
                    "content": {
                        "application/json": {
                            "schema": schema_ref(schema_name)
                        }
                    }
                },
                "404": { "description": "Not found" }
            }
        },
        "put": {
            "operationId": format!("update_{}", table),
            "tags": [table],
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": {
                        "schema": schema_ref(schema_name)
                    }
                }
            },
            "responses": {
                "200": {
                    "description": "Row updated",
                    "content": {
                        "application/json": {
                            "schema": schema_ref(schema_name)
                        }
                    }
                },
                "404": { "description": "Not found" }
            }
        },
        "patch": {
            "operationId": format!("partial_update_{}", table),
            "tags": [table],
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": {
                        "schema": schema_ref(schema_name)
                    }
                }
            },
            "responses": {
                "200": {
                    "description": "Row partially updated",
                    "content": {
                        "application/json": {
                            "schema": schema_ref(schema_name)
                        }
                    }
                },
                "404": { "description": "Not found" }
            }
        },
        "delete": {
            "operationId": format!("destroy_{}", table),
            "tags": [table],
            "responses": {
                "204": { "description": "Row deleted" },
                "404": { "description": "Not found" }
            }
        }
    })
}

fn schema_ref(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{}", name) })
}

fn list_envelope(schema_name: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "results": {
                "type": "array",
                "items": schema_ref(schema_name)
            },
            "count": { "type": "integer" }
        },
        "required": ["results", "count"]
    })
}

// Test hooks: expose the URL helpers so the integration test can
// assert that `.at("/api/docs")` flows through to the right path
// strings without booting a second App.
#[doc(hidden)]
pub fn test_spec_url(p: &OpenApiPlugin) -> String {
    p.spec_url()
}

#[doc(hidden)]
pub fn test_ui_route(p: &OpenApiPlugin) -> String {
    p.ui_route()
}

/// Crude PascalCase: split on `_` and uppercase the first char of
/// each chunk. Model names already arrive PascalCase from
/// `Model::NAME`, so this only matters when a plugin author passes a
/// snake_case name in the metadata.
fn pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for chunk in s.split('_') {
        let mut chars = chunk.chars();
        if let Some(c) = chars.next() {
            out.extend(c.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}
