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
        // The default block-list lives in umbra-rest and is consulted
        // via `umbra_rest::is_exposed(table)` at spec-build time, so
        // we don't duplicate it here. Our own opt-out is purely the
        // `extra_exclude` list — for cases like "served by REST but
        // I don't want it in the public spec."
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

/// Public read of the configured spec URL — the path the JSON
/// document is served at after `App::build()` runs. Returns
/// `None` when OpenApiPlugin isn't installed (the OnceLock
/// hasn't been populated by `Plugin::routes()` yet); returns
/// `Some("/openapi/openapi.json")` for the default mount and
/// `Some("/api/docs/openapi.json")` when the user calls
/// `OpenApiPlugin::default().at("/api/docs")`.
///
/// The playground plugin reads this at HTML-render time to inject
/// the URL into the shell page as a JS global, so a re-mounted
/// spec is auto-discovered by the SPA without the user having to
/// also configure the playground.
pub fn spec_url() -> Option<String> {
    CONFIG.get().map(|cfg| cfg.spec_url())
}

impl Plugin for OpenApiPlugin {
    fn name(&self) -> &'static str {
        "openapi"
    }

    fn dependencies(&self) -> &'static [&'static str] {
        &["rest"]
    }

    fn routes(&self) -> Router {
        let _ = CONFIG.set(self.clone());
        // Publish the spec URL to the core registry so cross-plugin
        // consumers (umbra-playground's SPA fetches it from the
        // browser) can discover the configured mount without
        // hardcoding `/openapi/openapi.json`.
        umbra::routes::init_openapi_spec_url(self.spec_url());
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

    // Playground-openapi-gaps #2: precompute every (table →
    // schema_name) mapping so FK columns can emit
    // `x-umbra-fk-ref` pointing at the target schema's JSON
    // pointer. The pointer shape `#/components/schemas/<Target>`
    // is what generated clients follow to navigate from `Post.author`
    // to the `User` schema. Done in a separate walk first so the
    // map is complete by the time column_schema runs on FK fields.
    let mut table_to_schema: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for plugin in umbra::migrate::registered_plugins() {
        for model in umbra::migrate::models_for_plugin(&plugin) {
            table_to_schema.insert(model.table.clone(), pascal_case(&model.name));
        }
    }

    for plugin in umbra::migrate::registered_plugins() {
        for model in umbra::migrate::models_for_plugin(&plugin) {
            // The spec describes what REST actually serves, so defer
            // to RestPlugin's allow/block decision first. This means
            // `RestPlugin::default().include_only(["article"])`
            // automatically restricts the spec to `article` without
            // the user having to repeat the configuration on
            // OpenApiPlugin. The OpenAPI plugin's own `.exclude(...)`
            // list still applies AFTER as an additional filter for
            // tables the user wants served-but-not-documented.
            if !umbra_rest::is_exposed(&model.table) {
                continue;
            }
            if !cfg.is_exposed(&model.table) {
                continue;
            }
            let schema_name = pascal_case(&model.name);
            schemas.insert(schema_name.clone(), model_schema(&model, &table_to_schema));
            // Advertise every filterable column × lookup AND the
            // `?search=` free-text parameter (when enabled) as
            // discoverable query parameters on the GET list
            // operation. The playground (and any spec consumer) can
            // then drive a real filter UI off the spec instead of
            // guessing.
            let mut list_params = Vec::new();
            // Pagination always documented — the REST plugin
            // accepts `?page` / `?page_size` on every list endpoint
            // regardless of resource config.
            list_params.extend(pagination_parameters());
            if umbra_rest::search_enabled_for(&model.table) {
                list_params.push(search_parameter());
            }
            // `?fields=` sparse fieldset (BUG-81) is always
            // available — independent of search / filter opt-out.
            list_params.push(fields_parameter(&model));
            // `?include=fk1,fk2` — only emit when the model actually
            // has FK columns; otherwise the param has nothing to
            // expand and the playground multi-select would render
            // empty.
            if model.fields.iter().any(|c| c.fk_target.is_some()) {
                list_params.push(include_parameter(&model));
            }
            if umbra_rest::filters_enabled_for(&model.table) {
                list_params.extend(filter_parameters(&model));
            }
            paths.insert(
                format!("/api/{}/", model.table),
                collection_paths(&model.table, &schema_name, &list_params),
            );
            // Retrieve respects both `?fields=` and `?include=` — same
            // shape as list. Build the params slice dynamically so the
            // FK-less models don't get a vestigial `?include=` entry.
            let mut item_params = vec![fields_parameter(&model)];
            if model.fields.iter().any(|c| c.fk_target.is_some()) {
                item_params.push(include_parameter(&model));
            }
            paths.insert(
                format!("/api/{}/{{id}}", model.table),
                item_paths(&model.table, &schema_name, &item_params),
            );
        }
    }

    // BUG-20: every plugin's `Plugin::openapi_paths()` contribution
    // gets merged into the spec. Auto-CRUD paths above land first
    // (so a plugin can shadow a model's path with a custom Path Item
    // if it wants); plugin contributions land on top, last-write-
    // wins for duplicate URLs.
    if let Some(entries) = umbra::routes::registered_openapi_paths() {
        for (path, item) in entries {
            paths.insert(path.clone(), item.clone());
        }
    }

    let mut info = Map::new();
    info.insert("title".into(), Value::String(cfg.title.clone()));
    info.insert("version".into(), Value::String(cfg.version.clone()));
    if let Some(desc) = &cfg.description {
        info.insert("description".into(), Value::String(desc.clone()));
    }

    // Playground-openapi-gaps #4: read the configured auth chain's
    // securitySchemes and emit a `components.securitySchemes` block
    // + a global `security` array referencing each. The global
    // security is an OR (any one scheme satisfies the request),
    // matching `ChainAuthentication([Session, Bearer])`'s actual
    // runtime behaviour.
    let mut security_schemes = Map::new();
    let mut security: Vec<Value> = Vec::new();
    for (name, scheme) in umbra_rest::registered_security_schemes() {
        security.push(json!({ name.clone(): [] }));
        security_schemes.insert(name, scheme);
    }
    let mut components = Map::new();
    components.insert("schemas".into(), Value::Object(schemas));
    if !security_schemes.is_empty() {
        components.insert("securitySchemes".into(), Value::Object(security_schemes));
    }

    let mut document = Map::new();
    document.insert("openapi".into(), Value::String("3.0.3".into()));
    document.insert("info".into(), Value::Object(info));
    document.insert("paths".into(), Value::Object(paths));
    document.insert("components".into(), Value::Object(components));
    if !security.is_empty() {
        document.insert("security".into(), Value::Array(security));
    }
    Value::Object(document)
}

fn model_schema(
    model: &ModelMeta,
    table_to_schema: &std::collections::HashMap<String, String>,
) -> Value {
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    for col in &model.fields {
        properties.insert(
            col.name.clone(),
            column_schema_with_refs(col, table_to_schema),
        );
        // PK is auto-generated by SQLite on POST.
        // Non-nullable non-PK columns are what the client MUST
        // supply — except when the framework supplies a default
        // itself. `auto_now` / `auto_now_add` stamp `Utc::now()`
        // when the body omits the value, and `noform` columns
        // are stripped from the body before write. None of
        // those should appear in `required`; making them so
        // would force clients to ship server-managed timestamps
        // and password hashes on every POST.
        if !col.nullable && !col.primary_key && !col.auto_now && !col.auto_now_add && !col.noform {
            required.push(Value::String(col.name.clone()));
        }
    }
    // M2M relations live on the parent's `m2m_relations` channel
    // (not on `fields`, because they have no column on the parent
    // table). Surface them as `array of integer` with a vendor
    // extension naming the child schema so playground / generated
    // clients can render a tag-picker. Not marked required —
    // M2M slots are always optional on write.
    for rel in &model.m2m_relations {
        let target_schema = table_to_schema
            .get(&rel.target_table)
            .cloned()
            .unwrap_or_else(|| pascal_case(&rel.target_name));
        let mut prop = serde_json::Map::new();
        prop.insert("type".into(), Value::String("array".into()));
        prop.insert(
            "items".into(),
            json!({ "type": "integer", "format": "int64" }),
        );
        prop.insert(
            "description".into(),
            Value::String(format!(
                "Many-to-many relation to {}. Send an array of child ids on \
                 create / update; the framework writes the junction table.",
                target_schema,
            )),
        );
        // Vendor extensions: aware clients (playground) can render
        // a multi-select chip picker pointed at the child schema.
        prop.insert("x-umbra-m2m".into(), Value::Bool(true));
        prop.insert(
            "x-umbra-m2m-target".into(),
            Value::String(target_schema.clone()),
        );
        prop.insert(
            "x-umbra-m2m-target-table".into(),
            Value::String(rel.target_table.clone()),
        );
        if table_to_schema.contains_key(&rel.target_table) {
            prop.insert(
                "x-umbra-m2m-target-ref".into(),
                Value::String(format!("#/components/schemas/{target_schema}")),
            );
        }
        properties.insert(rel.field_name.clone(), Value::Object(prop));
    }
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String("object".into()));
    obj.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        obj.insert("required".into(), Value::Array(required));
    }
    Value::Object(obj)
}

/// Wrap [`column_schema`] with the schema-name-aware FK ref. The
/// inner function stays backwards-compatible (no map arg) for the
/// test cases that exercise `column_schema(&col)` directly.
fn column_schema_with_refs(
    col: &Column,
    table_to_schema: &std::collections::HashMap<String, String>,
) -> Value {
    let mut value = column_schema(col);
    // Playground-openapi-gaps #2: emit `x-umbra-fk-ref` as a JSON
    // pointer to the target schema. Generated clients that follow
    // vendor extensions can navigate from a `Post.author` (integer)
    // to the `User` schema. OpenAPI 3.0's strict `$ref` rule
    // ("siblings of $ref must be ignored") rules out putting this on
    // the value as a real `$ref`, which is why this lives as a
    // vendor extension. The Swagger UI playground already special-
    // cases umbra's `x-umbra-*` extensions; openapi-generator
    // / orval can do the same.
    if let Some(target_table) = &col.fk_target {
        if let Some(schema_name) = table_to_schema.get(target_table) {
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "x-umbra-fk-ref".into(),
                    Value::String(format!("#/components/schemas/{schema_name}")),
                );
            }
        }
    }
    value
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
    // `#[umbra(help = "...")]` lands as the OpenAPI standard
    // `description` so Swagger UI / generated clients pick it up.
    // Closes playground-openapi-gaps item 5.
    if !col.help.is_empty() {
        obj.insert("description".into(), Value::String(col.help.clone()));
    }
    // `#[umbra(example = "...")]` lands as the OpenAPI standard
    // `example` so Swagger UI pre-fills request bodies with a
    // useful sample. Closes playground-openapi-gaps item 6.
    if !col.example.is_empty() {
        obj.insert("example".into(), Value::String(col.example.clone()));
    }
    // IMP-3: `#[umbra(min = N)]` / `#[umbra(max = N)]` →
    // OpenAPI `minimum` / `maximum`. Both are standard 3.0 keys.
    if let Some(min) = col.min {
        obj.insert(
            "minimum".into(),
            Value::Number(serde_json::Number::from(min)),
        );
    }
    if let Some(max) = col.max {
        obj.insert(
            "maximum".into(),
            Value::Number(serde_json::Number::from(max)),
        );
    }
    // BUG-11/12/13: `Slug` / `Email` / `Url` wrappers lower to
    // standard OpenAPI markers so generated clients and Swagger UI
    // render the right widget.
    if let Some(fmt) = col.text_format.as_deref() {
        match fmt {
            "email" => {
                obj.insert("format".into(), Value::String("email".into()));
            }
            "url" => {
                obj.insert("format".into(), Value::String("uri".into()));
            }
            "slug" => {
                // No built-in OpenAPI format for slug; use the
                // `pattern` keyword (standard 3.0) to constrain
                // accepted values. Mirrors the macro-side regex.
                obj.insert("pattern".into(), Value::String("^[A-Za-z0-9_-]+$".into()));
            }
            _ => {}
        }
    }
    // Standard OpenAPI: closed-set values become `enum`. Skipped for
    // multichoice (a CSV-encoded subset) because each request value is
    // a comma-separated string of the choices, not one choice — clients
    // need richer guidance than a flat enum can provide. We still emit
    // the underlying choices via `x-umbra-choices` below.
    if !col.choices.is_empty() && !col.is_multichoice {
        obj.insert(
            "enum".into(),
            Value::Array(col.choices.iter().cloned().map(Value::String).collect()),
        );
    }
    if col.max_length > 0 {
        obj.insert(
            "maxLength".into(),
            Value::Number(serde_json::Number::from(col.max_length)),
        );
    }
    if !col.default.is_empty() {
        // OpenAPI `default` is typed as the property's type, but the
        // Column carries it as a string (it's a SQL literal). Emitting
        // as a string is the conservative choice — Swagger UI shows it
        // as a hint, and clients that care can re-parse.
        obj.insert("default".into(), Value::String(col.default.clone()));
    }
    if col.is_multichoice {
        obj.insert("x-umbra-multichoice".into(), Value::Bool(true));
        obj.insert(
            "x-umbra-choices".into(),
            Value::Array(col.choices.iter().cloned().map(Value::String).collect()),
        );
    }
    if !col.choice_labels.is_empty() {
        obj.insert(
            "x-umbra-choice-labels".into(),
            Value::Array(
                col.choice_labels
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if let Some(target) = &col.fk_target {
        obj.insert("x-umbra-fk-target".into(), Value::String(target.clone()));
    }
    // Playground-openapi-gaps #2: the schema-pointer flavour of
    // `x-umbra-fk-target` lives on the wrapper `column_schema_with_refs`
    // because it needs the table→schema name map.
    if col.is_string_repr {
        obj.insert("x-umbra-string-repr".into(), Value::Bool(true));
    }
    // `noedit` is intentionally NOT mapped to `readOnly`. The two
    // concepts are different: `noedit` is an admin EDIT-form hint
    // ("show this field disabled when the user clicks the row"),
    // while OpenAPI's `readOnly` means "never accept this field in
    // ANY request body" — including POST. The conflation hid
    // required `noedit` fields from the playground autofill on
    // CREATE, which is exactly the wrong direction.
    //
    // The real "API never accepts" semantic is `noform` (the field
    // is never shown on any admin form AND the REST plugin drops
    // it from request bodies before write). That maps cleanly to
    // OpenAPI `readOnly`.
    // `auto_now` / `auto_now_add` are server-populated: the ORM
    // stamps `Utc::now()` when the body omits the value. Surface
    // them as vendor extensions so an aware client (the playground)
    // can show a "the server fills this in" hint and skip the
    // field on autofill / form prefill. Not mapped to `readOnly`
    // because the client CAN still send an explicit value — the
    // framework respects it. `required` is already dropped at
    // `model_schema`'s pass for the same reason.
    if col.auto_now_add {
        obj.insert("x-umbra-auto-now-add".into(), Value::Bool(true));
    }
    if col.auto_now {
        obj.insert("x-umbra-auto-now".into(), Value::Bool(true));
    }
    if col.noform {
        obj.insert("readOnly".into(), Value::Bool(true));
        // Vendor extension so clients aware of the umbra surface
        // (the playground in particular) can distinguish "API
        // doesn't accept this" from "admin won't let you edit it"
        // without having to re-derive the rule from the column
        // metadata.
        obj.insert("x-umbra-noform".into(), Value::Bool(true));
    }
    // `noedit` becomes a pure vendor extension. Aware clients can
    // surface it in their edit UI (the playground could, e.g.,
    // grey the field on PUT/PATCH but not POST) without it
    // contaminating the request-body contract.
    if col.noedit {
        obj.insert("x-umbra-noedit".into(), Value::Bool(true));
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
        // BUG-10: NUMERIC. OpenAPI represents arbitrary-precision
        // decimals as `string` with `format: decimal` per the
        // 3.1 spec convention; clients that round-trip through
        // f64 lose precision, so the canonical wire shape is the
        // string representation.
        SqlType::Decimal => ("string", Some("decimal")),
    }
}

/// Build the OpenAPI `?search=` parameter object. One slot shared
/// across every searchable column on the resource — the REST list
/// handler ORs `icontains` predicates on Text columns and `eq`
/// predicates on numeric / FK / Boolean columns whose type matches
/// the parsed term shape.
///
/// Vendor extension `x-umbra-search: true` flags this parameter for
/// aware clients (the playground in particular surfaces it as a
/// dedicated search box rather than treating it as a generic filter
/// chip).
fn search_parameter() -> Value {
    json!({
        "name": "search",
        "in": "query",
        "required": false,
        "description": "Free-text search across every searchable column. \
                        Text columns match via case-insensitive substring; \
                        numeric / FK / Boolean columns match exactly when \
                        the term parses as that type. Multiple matches are \
                        ORed.",
        "schema": { "type": "string" },
        "x-umbra-search": true,
    })
}

/// BUG-81: the `?fields=col1,col2` sparse-fieldset parameter. Lives
/// on every list AND retrieve endpoint — when set, the response row
/// drops every key not in the requested list. Unknown column names
/// are silently ignored; an empty value falls back to the full row.
///
/// The `x-umbra-fields` vendor extension lists every column the
/// model exposes so the playground can render a multi-select
/// instead of a plain text box. Generated clients that ignore the
/// extension still see a `string` parameter with a clear
/// description.
fn fields_parameter(model: &ModelMeta) -> Value {
    let columns: Vec<Value> = model
        .fields
        .iter()
        .map(|c| Value::String(c.name.clone()))
        .collect();
    json!({
        "name": "fields",
        "in": "query",
        "required": false,
        "description": "Comma-separated list of column names to include in the \
                        response. Unknown names are silently dropped; an empty \
                        value falls back to the full row (BUG-81). Composes \
                        with hide / transform / computed — hide always wins, \
                        the rest are returned iff in the list.",
        "schema": { "type": "string" },
        "x-umbra-fields": true,
        "x-umbra-fields-columns": Value::Array(columns),
    })
}

/// `?include=fk1,fk2` — expand the named FK columns into their full
/// related-row objects via the REST plugin's select_related-backed
/// path. Only FK columns are valid (anything else 400s); the
/// playground reads `x-umbra-include-fks` to render a multi-select
/// of the candidate FK names. Mirrors the `fields_parameter` shape
/// so the same UI machinery can drive both.
fn include_parameter(model: &ModelMeta) -> Value {
    let fks: Vec<Value> = model
        .fields
        .iter()
        .filter(|c| c.fk_target.is_some())
        .map(|c| Value::String(c.name.clone()))
        .collect();
    json!({
        "name": "include",
        "in": "query",
        "required": false,
        "description": "Comma-separated list of foreign-key columns to expand \
                        in the response. Each named FK gets replaced with the \
                        full related-row JSON object (one batched IN(...) query \
                        per FK — no N+1). Unknown or non-FK names return a 400. \
                        Example: `?include=user,billing_address`.",
        "schema": { "type": "string" },
        "x-umbra-include": true,
        "x-umbra-include-fks": Value::Array(fks),
    })
}

/// Playground-openapi-gaps #3: the two standard pagination
/// parameters umbra-rest accepts on every list endpoint. Documented
/// here so generated clients and Swagger UI surface them as
/// configurable, instead of leaving callers to discover the shape
/// from the response envelope.
fn pagination_parameters() -> Vec<Value> {
    vec![
        json!({
            "name": "page",
            "in": "query",
            "required": false,
            "description": "1-indexed page number. Defaults to 1 when omitted.",
            "schema": { "type": "integer", "format": "int32", "minimum": 1, "default": 1 },
            "x-umbra-pagination": "page",
        }),
        json!({
            "name": "page_size",
            "in": "query",
            "required": false,
            "description": "Rows per page. Capped at 100. Default 20.",
            "schema": {
                "type": "integer", "format": "int32",
                "minimum": 1, "maximum": 100, "default": 20,
            },
            "x-umbra-pagination": "page_size",
        }),
    ]
}

/// Build the OpenAPI `parameters` entries that document the
/// django-filter-style query-string filters on a list endpoint.
/// One entry per (column, lookup) pair.
///
/// Skips the primary key (filtering on `id` adds no value over the
/// detail URL `/api/<table>/{id}`) and the columns whose type the
/// filter parser can't model (none today, but the helper takes the
/// stance so future opt-outs are a one-line change).
fn filter_parameters(model: &ModelMeta) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for col in &model.fields {
        if col.primary_key {
            continue;
        }
        let lookups = umbra_rest::filtering::applicable_lookups(col);
        for lookup in lookups {
            let name = if lookup == "eq" {
                col.name.clone()
            } else {
                format!("{}__{}", col.name, lookup)
            };
            out.push(filter_parameter(col, lookup, &name));
        }
    }
    out
}

/// One OpenAPI parameter object for a single (column, lookup) pair.
///
/// - `__in` takes a CSV string: schema `type: string` with a
///   description spelling out the format. (A proper `style: form` +
///   `explode: false` array would be more correct OpenAPI but
///   complicates client code.)
/// - `__isnull` takes a boolean.
/// - `__contains` / `__icontains` / `__startswith` take a string
///   regardless of column type.
/// - Range / equality lookups inherit the column's own type.
fn filter_parameter(col: &Column, lookup: &str, name: &str) -> Value {
    let (schema, description) = match lookup {
        "in" => (
            json!({ "type": "string" }),
            format!(
                "Comma-separated `{}` values; matches rows where the column is in the set.",
                col.name,
            ),
        ),
        "isnull" => (
            json!({ "type": "boolean" }),
            format!(
                "`true` matches rows where `{}` IS NULL; `false` matches IS NOT NULL.",
                col.name,
            ),
        ),
        "contains" | "icontains" | "startswith" => {
            let phrase = match lookup {
                "contains" => "case-sensitive substring",
                "icontains" => "case-insensitive substring",
                "startswith" => "case-sensitive prefix",
                _ => unreachable!(),
            };
            (
                json!({ "type": "string" }),
                format!(
                    "Matches rows where `{}` contains the given {phrase}.",
                    col.name
                ),
            )
        }
        // eq, ne, gte, lte, gt, lt — type-aligned with the column.
        _ => {
            let (ty, format) = openapi_type(col.ty);
            let mut schema_obj = Map::new();
            schema_obj.insert("type".into(), Value::String(ty.into()));
            if let Some(f) = format {
                schema_obj.insert("format".into(), Value::String(f.into()));
            }
            let phrase = match lookup {
                "eq" => "equals the value",
                "ne" => "does not equal the value",
                "gte" => "is greater than or equal to the value",
                "lte" => "is less than or equal to the value",
                "gt" => "is greater than the value",
                "lt" => "is less than the value",
                _ => "matches the value",
            };
            (
                Value::Object(schema_obj),
                format!("Matches rows where `{}` {phrase}.", col.name),
            )
        }
    };

    json!({
        "name": name,
        "in": "query",
        "required": false,
        "description": description,
        "schema": schema,
        "x-umbra-filter-field": col.name,
        "x-umbra-filter-lookup": lookup,
    })
}

fn collection_paths(table: &str, schema_name: &str, filter_params: &[Value]) -> Value {
    // The list operation's `parameters` array is omitted entirely
    // when there are no filters (matches the pre-fix spec shape and
    // keeps Swagger UI from rendering an empty Parameters section).
    let mut get_op = Map::new();
    get_op.insert(
        "operationId".into(),
        Value::String(format!("list_{}", table)),
    );
    get_op.insert("tags".into(), json!([table]));
    if !filter_params.is_empty() {
        get_op.insert("parameters".into(), Value::Array(filter_params.to_vec()));
    }
    get_op.insert(
        "responses".into(),
        json!({
            "200": {
                "description": "List of rows",
                "content": {
                    "application/json": {
                        "schema": list_envelope(schema_name)
                    }
                }
            }
        }),
    );

    json!({
        "get": Value::Object(get_op),
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

fn item_paths(table: &str, schema_name: &str, retrieve_query_params: &[Value]) -> Value {
    let id_param = json!({
        "name": "id",
        "in": "path",
        "required": true,
        "schema": { "type": "string" }
    });
    // Build the GET op separately so its query params can be
    // listed alongside the path-level `id_param`. Path-level
    // `parameters` apply to every method on the item URL, so
    // GET-only knobs (like `?fields=`) land on the operation
    // itself instead.
    let mut get_op = Map::new();
    get_op.insert(
        "operationId".into(),
        Value::String(format!("retrieve_{}", table)),
    );
    get_op.insert("tags".into(), json!([table]));
    if !retrieve_query_params.is_empty() {
        get_op.insert(
            "parameters".into(),
            Value::Array(retrieve_query_params.to_vec()),
        );
    }
    get_op.insert(
        "responses".into(),
        json!({
            "200": {
                "description": "Row found",
                "content": {
                    "application/json": {
                        "schema": schema_ref(schema_name)
                    }
                }
            },
            "404": { "description": "Not found" }
        }),
    );
    json!({
        "parameters": [id_param],
        "get": Value::Object(get_op),
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

#[cfg(test)]
mod tests {
    use super::*;
    use umbra::migrate::Column;
    use umbra::orm::SqlType;

    fn base_col(name: &str, ty: SqlType) -> Column {
        Column {
            name: name.into(),
            ty,
            primary_key: false,
            nullable: false,
            fk_target: None,
            noform: false,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: Vec::new(),
            choice_labels: Vec::new(),
            default: String::new(),
            is_multichoice: false,
            unique: false,
            on_delete: ::umbra::orm::FkAction::NoAction,
            on_update: ::umbra::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: ::core::option::Option::None,
            slug_from: ::core::option::Option::None,
        }
    }

    #[test]
    fn choices_render_as_openapi_enum_with_labels_extension() {
        let mut col = base_col("status", SqlType::Text);
        col.choices = vec!["draft".into(), "published".into(), "archived".into()];
        col.choice_labels = vec!["Draft".into(), "Published".into(), "Archived".into()];
        let schema = column_schema(&col);
        assert_eq!(schema["type"], "string");
        assert_eq!(
            schema["enum"],
            serde_json::json!(["draft", "published", "archived"])
        );
        assert_eq!(
            schema["x-umbra-choice-labels"],
            serde_json::json!(["Draft", "Published", "Archived"])
        );
    }

    #[test]
    fn multichoice_skips_enum_and_uses_vendor_extension() {
        let mut col = base_col("tags", SqlType::Text);
        col.choices = vec!["rust".into(), "django".into()];
        col.is_multichoice = true;
        let schema = column_schema(&col);
        assert!(
            schema.get("enum").is_none(),
            "multichoice columns should not declare a flat enum (value is a CSV subset)"
        );
        assert_eq!(schema["x-umbra-multichoice"], true);
        assert_eq!(
            schema["x-umbra-choices"],
            serde_json::json!(["rust", "django"])
        );
    }

    #[test]
    fn max_length_and_default_surface_as_standard_openapi_keys() {
        let mut col = base_col("title", SqlType::Text);
        col.max_length = 50;
        col.default = "untitled".into();
        let schema = column_schema(&col);
        assert_eq!(schema["maxLength"], 50);
        assert_eq!(schema["default"], "untitled");
    }

    #[test]
    fn fk_target_emits_vendor_extension_for_playground_navigation() {
        let mut col = base_col("author_id", SqlType::ForeignKey);
        col.fk_target = Some("auth_user".into());
        let schema = column_schema(&col);
        assert_eq!(schema["type"], "integer");
        assert_eq!(schema["format"], "int64");
        assert_eq!(schema["x-umbra-fk-target"], "auth_user");
    }

    #[test]
    fn noform_renders_as_read_only_and_carries_vendor_extension() {
        // `noform` is the API-readOnly semantic — never accepted in
        // any request body, server fills it in. Maps to OpenAPI
        // `readOnly: true` so Swagger / generated clients honour it
        // on POST and PUT/PATCH alike.
        let mut col = base_col("internal_token", SqlType::Text);
        col.noform = true;
        let schema = column_schema(&col);
        assert_eq!(schema["readOnly"], true);
        assert_eq!(schema["x-umbra-noform"], true);
    }

    #[test]
    fn noedit_does_NOT_render_as_read_only() {
        // Decoupled from API contract: `noedit` is purely an admin
        // EDIT-form hint. The field stays writable in the spec so a
        // required `noedit` field (e.g. `email` you can set at
        // signup but not change later) still gets autofilled on POST
        // by the playground and accepted by the REST plugin on CREATE.
        let mut col = base_col("email", SqlType::Text);
        col.noedit = true;
        let schema = column_schema(&col);
        assert!(
            schema.get("readOnly").is_none(),
            "noedit must NOT contaminate the API request-body contract; \
             got readOnly in schema: {schema:?}"
        );
        // Surface it as a vendor extension so aware clients can
        // still grey the field on PUT/PATCH if they want.
        assert_eq!(schema["x-umbra-noedit"], true);
    }

    #[test]
    fn plain_column_keeps_minimal_schema_no_extensions() {
        let col = base_col("body", SqlType::Text);
        let schema = column_schema(&col);
        let obj = schema.as_object().expect("object");
        assert_eq!(
            obj.len(),
            1,
            "plain column should only have `type`: {obj:?}"
        );
        assert_eq!(schema["type"], "string");
    }

    /// Playground-openapi-gaps item 5: `#[umbra(help = "...")]`
    /// emits as the standard OpenAPI `description` so Swagger UI
    /// and any generated client picks it up. Empty help leaves the
    /// key absent.
    #[test]
    fn help_attribute_flows_to_openapi_description() {
        let mut col = base_col("status", SqlType::Text);
        col.help = "Workflow step. Set by editors on Save.".to_string();
        let schema = column_schema(&col);
        assert_eq!(
            schema["description"], "Workflow step. Set by editors on Save.",
            "help should round-trip to OpenAPI description; got: {schema:?}",
        );
    }

    #[test]
    fn empty_help_omits_description() {
        let col = base_col("body", SqlType::Text);
        let schema = column_schema(&col);
        assert!(
            schema.get("description").is_none(),
            "empty help should omit description; got: {schema:?}",
        );
    }

    /// Playground-openapi-gaps item 6: `#[umbra(example = "...")]`
    /// emits as OpenAPI `example` on the property schema. Empty
    /// leaves the key absent.
    #[test]
    fn example_attribute_flows_to_openapi_example() {
        let mut col = base_col("status", SqlType::Text);
        col.example = "published".to_string();
        let schema = column_schema(&col);
        assert_eq!(
            schema["example"], "published",
            "example should round-trip; got: {schema:?}",
        );
    }

    #[test]
    fn empty_example_omits_example() {
        let col = base_col("body", SqlType::Text);
        let schema = column_schema(&col);
        assert!(
            schema.get("example").is_none(),
            "empty example should omit example key; got: {schema:?}",
        );
    }

    // ----------------------------------------------------------------- //
    // Filter parameter emission                                          //
    // ----------------------------------------------------------------- //

    fn note_model() -> ModelMeta {
        let mut id = base_col("id", SqlType::BigInt);
        id.primary_key = true;
        let mut published_at = base_col("published_at", SqlType::Timestamptz);
        published_at.nullable = true;
        ModelMeta {
            name: "Note".to_string(),
            table: "note".to_string(),
            fields: vec![
                id,
                base_col("title", SqlType::Text),
                base_col("views", SqlType::Integer),
                published_at,
            ],
            display: "Note".to_string(),
            icon: "database".to_string(),
            database: None,
            singleton: false,
            unique_together: Vec::new(),
            indexes: Vec::new(),
            ordering: Vec::new(),
            m2m_relations: Vec::new(),
            soft_delete: false,
        }
    }

    #[test]
    fn filter_parameters_skips_primary_key() {
        let params = filter_parameters(&note_model());
        let names: Vec<&str> = params.iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert!(
            !names.iter().any(|n| *n == "id" || n.starts_with("id__")),
            "PK column should be skipped; got {names:?}",
        );
    }

    #[test]
    fn filter_parameters_eq_uses_bare_column_name_no_suffix() {
        let params = filter_parameters(&note_model());
        let bare_title = params
            .iter()
            .find(|p| p["name"] == "title")
            .expect("title eq parameter should be present");
        assert_eq!(bare_title["x-umbra-filter-lookup"], "eq");
        assert_eq!(bare_title["x-umbra-filter-field"], "title");
        assert_eq!(bare_title["schema"]["type"], "string");
    }

    #[test]
    fn filter_parameters_in_is_string_typed_with_csv_description() {
        let params = filter_parameters(&note_model());
        let title_in = params
            .iter()
            .find(|p| p["name"] == "title__in")
            .expect("title__in parameter should be present");
        assert_eq!(title_in["schema"]["type"], "string");
        assert!(
            title_in["description"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("comma"),
            "__in description should mention the comma-separated format",
        );
    }

    #[test]
    fn filter_parameters_isnull_only_on_nullable_columns() {
        let params = filter_parameters(&note_model());
        let isnull_params: Vec<&str> = params
            .iter()
            .filter_map(|p| p["name"].as_str())
            .filter(|n| n.ends_with("__isnull"))
            .collect();
        assert_eq!(
            isnull_params,
            vec!["published_at__isnull"],
            "isnull lookup should only appear for nullable columns; got {isnull_params:?}",
        );
    }

    #[test]
    fn filter_parameters_range_lookups_only_on_numeric_or_temporal() {
        let params = filter_parameters(&note_model());
        let has_gte = |field: &str| params.iter().any(|p| p["name"] == format!("{field}__gte"));
        assert!(has_gte("views"), "integer column gets gte");
        assert!(has_gte("published_at"), "timestamp column gets gte");
        assert!(
            !has_gte("title"),
            "text column must NOT get gte; got {params:?}",
        );
    }

    #[test]
    fn filter_parameters_string_lookups_only_on_text() {
        let params = filter_parameters(&note_model());
        let has_contains = |field: &str| {
            params
                .iter()
                .any(|p| p["name"] == format!("{field}__contains"))
        };
        assert!(has_contains("title"), "text column gets contains");
        assert!(
            !has_contains("views"),
            "integer column must NOT get contains; got {params:?}",
        );
    }

    #[test]
    fn collection_paths_omits_parameters_array_when_no_filters() {
        let value = collection_paths("note", "Note", &[]);
        let get_op = &value["get"];
        assert!(
            get_op.get("parameters").is_none(),
            "no filters → no parameters key; got {get_op:?}",
        );
    }

    #[test]
    fn collection_paths_includes_parameters_when_filters_present() {
        let filter_params = filter_parameters(&note_model());
        let value = collection_paths("note", "Note", &filter_params);
        let params = value["get"]["parameters"]
            .as_array()
            .expect("parameters array should be present when filters land");
        assert!(!params.is_empty());
        assert!(
            params.iter().all(|p| p["in"] == "query"),
            "every filter parameter is in: query",
        );
    }

    /// BUG-81: the `?fields=` sparse-fieldset parameter is built
    /// with the model's columns listed under the
    /// `x-umbra-fields-columns` vendor extension so the playground
    /// can render a multi-select.
    #[test]
    fn fields_parameter_lists_model_columns() {
        let param = fields_parameter(&note_model());
        assert_eq!(param["name"], "fields");
        assert_eq!(param["in"], "query");
        assert_eq!(param["x-umbra-fields"], true);
        let cols = param["x-umbra-fields-columns"]
            .as_array()
            .expect("x-umbra-fields-columns should be a list");
        let names: Vec<&str> = cols.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"title"));
        assert!(names.contains(&"views"));
        assert!(
            !names.is_empty(),
            "every column should land in the enum so the playground can offer it",
        );
    }

    /// The retrieve op also documents `?fields=` so the playground
    /// renders the same param on GET /resource/{id}.
    #[test]
    fn item_paths_advertises_fields_query_param_on_retrieve() {
        let value = item_paths("note", "Note", &[fields_parameter(&note_model())]);
        let get_params = value["get"]["parameters"]
            .as_array()
            .expect("retrieve op should carry its query parameters");
        assert!(
            get_params.iter().any(|p| p["name"] == "fields"),
            "fields parameter should be on the retrieve op; got {get_params:?}",
        );
    }

    /// Playground-openapi-gaps #2: FK columns gain an
    /// `x-umbra-fk-ref` JSON pointer when the target schema is
    /// known. Generated clients that follow vendor extensions can
    /// navigate Post.author → User.
    #[test]
    fn fk_column_emits_schema_ref_when_target_known() {
        let mut col = base_col("author", SqlType::ForeignKey);
        col.fk_target = Some("auth_user".into());
        let mut map = std::collections::HashMap::new();
        map.insert("auth_user".to_string(), "AuthUser".to_string());
        let schema = column_schema_with_refs(&col, &map);
        assert_eq!(
            schema["x-umbra-fk-target"], "auth_user",
            "the table-name vendor extension stays for backward compat",
        );
        assert_eq!(
            schema["x-umbra-fk-ref"], "#/components/schemas/AuthUser",
            "the JSON pointer to the target schema should be emitted",
        );
    }

    #[test]
    fn fk_column_without_known_target_omits_schema_ref() {
        let mut col = base_col("author", SqlType::ForeignKey);
        col.fk_target = Some("unknown_table".into());
        let map = std::collections::HashMap::new();
        let schema = column_schema_with_refs(&col, &map);
        assert!(
            schema.get("x-umbra-fk-ref").is_none(),
            "unknown FK target → no ref emitted; got: {schema:?}",
        );
    }

    /// M2M relations get a property entry on the model schema
    /// (`array of integer` ids) plus vendor extensions naming the
    /// target schema. Without this the playground / generated
    /// clients have no way to know the model has a many-to-many
    /// slot.
    #[test]
    fn m2m_relation_lands_in_model_schema_with_target_extension() {
        let mut model = note_model();
        model.m2m_relations.push(umbra::migrate::M2MRelation {
            field_name: "tags".to_string(),
            target_table: "tag".to_string(),
            target_name: "Tag".to_string(),
        });
        // table_to_schema mirrors what `model_schemas` builds at
        // spec-emit time; pre-seed with the M2M target so the
        // vendor `x-umbra-m2m-target-ref` JSON pointer is set.
        let mut tts = std::collections::HashMap::new();
        tts.insert("tag".to_string(), "Tag".to_string());
        let schema = model_schema(&model, &tts);
        let tags_prop = &schema["properties"]["tags"];
        assert_eq!(tags_prop["type"], "array");
        assert_eq!(tags_prop["items"]["type"], "integer");
        assert_eq!(tags_prop["x-umbra-m2m"], true);
        assert_eq!(tags_prop["x-umbra-m2m-target"], "Tag");
        assert_eq!(tags_prop["x-umbra-m2m-target-table"], "tag");
        assert_eq!(
            tags_prop["x-umbra-m2m-target-ref"],
            "#/components/schemas/Tag",
        );
        // Not in `required` — M2M slots are always optional.
        let required = schema["required"].as_array();
        if let Some(req) = required {
            assert!(!req.iter().any(|v| v == "tags"));
        }
    }

    /// `auto_now_add` (created_at) and `auto_now` (updated_at)
    /// fields are server-populated — the framework stamps
    /// `Utc::now()` when the body omits them. The OpenAPI
    /// schema must reflect that: the columns drop out of the
    /// `required` array AND gain vendor extensions so the
    /// playground can render them as "server fills this in"
    /// instead of marking them as missing inputs.
    #[test]
    fn auto_now_columns_are_optional_in_the_request_schema() {
        let mut model = note_model();
        let mut created = base_col("created_at", SqlType::Timestamptz);
        created.auto_now_add = true;
        let mut updated = base_col("updated_at", SqlType::Timestamptz);
        updated.auto_now = true;
        model.fields.push(created);
        model.fields.push(updated);

        let schema = model_schema(&model, &std::collections::HashMap::new());

        // Vendor extensions: aware clients flag these as
        // server-populated. Both extensions present, keyed
        // under the right column.
        assert_eq!(
            schema["properties"]["created_at"]["x-umbra-auto-now-add"],
            true
        );
        assert_eq!(schema["properties"]["updated_at"]["x-umbra-auto-now"], true);

        // NOT marked `readOnly` — the client can still send an
        // explicit timestamp if they want. `readOnly` is reserved
        // for `noform` columns the framework drops from bodies.
        assert!(
            schema["properties"]["created_at"].get("readOnly").is_none(),
            "auto_now_add must not be readOnly; got {}",
            schema["properties"]["created_at"],
        );
        assert!(
            schema["properties"]["updated_at"].get("readOnly").is_none(),
            "auto_now must not be readOnly; got {}",
            schema["properties"]["updated_at"],
        );

        // And dropped from `required` so a POST that omits them
        // doesn't 400 with "this field is required."
        let required = schema["required"].as_array().expect("required array");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            !names.contains(&"created_at"),
            "auto_now_add should drop out of required; got {names:?}",
        );
        assert!(
            !names.contains(&"updated_at"),
            "auto_now should drop out of required; got {names:?}",
        );
    }

    /// Playground-openapi-gaps #3: every list endpoint advertises
    /// `page` + `page_size` as query parameters, so generated
    /// clients and the playground know they're tunable.
    #[test]
    fn pagination_parameters_shape_round_trips() {
        let params = pagination_parameters();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0]["name"], "page");
        assert_eq!(params[0]["in"], "query");
        assert_eq!(params[0]["schema"]["type"], "integer");
        assert_eq!(params[0]["schema"]["minimum"], 1);
        assert_eq!(params[0]["schema"]["default"], 1);
        assert_eq!(params[0]["x-umbra-pagination"], "page");
        assert_eq!(params[1]["name"], "page_size");
        assert_eq!(params[1]["schema"]["maximum"], 100);
        assert_eq!(params[1]["x-umbra-pagination"], "page_size");
    }
}
