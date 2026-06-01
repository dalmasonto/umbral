//! umbra-admin — auto-generated CRUD admin for umbra models.
//!
//! Drop-in admin interface for any umbra project. Register the
//! [`AdminPlugin`] on `App::builder()` and every model the
//! migration registry knows about gets:
//!
//! - A list view at `/admin/<table>/` with all rows in a table
//! - A detail view at `/admin/<table>/<id>` with every field
//! - A create form at `/admin/<table>/new`
//! - An edit form at `/admin/<table>/<id>/edit`
//! - A delete action at `POST /admin/<table>/<id>/delete`
//!
//! Plus a registered-models index at `/admin/`.
//!
//! ## Customizing per-model display
//!
//! Register an [`AdminConfig`] for a model to control list columns, filter
//! facets, search, ordering, bulk actions, and readonly fields. See
//! [`AdminPlugin::register`] and the [`config`] module.
//!
//! ## Auth
//!
//! Every admin route requires HTTP Basic Auth against
//! [`umbra_auth::authenticate`]. The user has to be `is_staff = 1`
//! (matching Django's `auth_user.is_staff` gate). A 401 returns the
//! browser's basic-auth prompt; a non-staff user gets 403.
//!
//! ## Templates
//!
//! Five `include_str!`-embedded Jinja templates live in
//! `templates/`. The admin owns its own minijinja `Environment` so
//! it can render without registering into the framework's
//! `OnceLock`-protected global engine. `admin/base.html` is the
//! shared chrome; the other four extend it.
//!
//! ## Form widgets
//!
//! Inputs dispatch per [`SqlType`]:
//!
//! | SqlType | Input |
//! |---|---|
//! | `SmallInt`, `Integer`, `BigInt` | `<input type="number">` |
//! | `Real`, `Double` | `<input type="number" step="any">` |
//! | `Boolean` | `<input type="checkbox">` |
//! | `Text`, `Uuid` | `<input type="text">` |
//! | `Date` | `<input type="date">` |
//! | `Time` | `<input type="time">` |
//! | `Timestamptz` | `<input type="datetime-local">` |
//!
//! Nullable fields skip the `required` attribute.

pub mod config;

pub use config::{Action, AdminConfig, AdminContext};

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use base64::Engine;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use minijinja::{Environment, context};
use serde::Serialize;
use serde_json::Value;
use sqlx::{Row, SqlitePool};
use umbra::migrate::{Column, ModelMeta};
use umbra::orm::SqlType;
use umbra::prelude::*;
use umbra::web::{
    HeaderMap, Html, IntoResponse, Json, Path, Redirect, Response, StatusCode, header, post,
};
use uuid::Uuid;

// =========================================================================
// Plugin struct — now holds per-model AdminConfigs.
// =========================================================================

/// The plugin. Mounts every admin route under `/admin`.
///
/// Use [`AdminPlugin::register`] to attach an [`AdminConfig`] before
/// passing the plugin to `App::builder().plugin(...)`.
///
/// ```ignore
/// use umbra_admin::{AdminPlugin, AdminConfig, Action};
///
/// let admin = AdminPlugin::default()
///     .register(
///         AdminConfig::new("post")
///             .list_display(&["title", "author", "published_at"])
///             .list_filter(&["published"])
///             .search_fields(&["title", "body"])
///             .ordering(&["-published_at"])
///             .readonly_fields(&["created_at"])
///             .actions(vec![Action::delete_selected()]),
///     );
///
/// App::builder()
///     .plugin(AuthPlugin::default())
///     .plugin(admin)
///     .build()?;
/// ```
#[derive(Debug, Default, Clone)]
pub struct AdminPlugin {
    configs: Vec<AdminConfig>,
}

impl AdminPlugin {
    /// Register an [`AdminConfig`] for one model. Chainable.
    ///
    /// If two configs are registered for the same table the last one wins
    /// (same semantics as Django's `site.register` overwriting on duplicate).
    pub fn register(mut self, config: AdminConfig) -> Self {
        // Remove any prior config for the same table so the last one wins.
        self.configs.retain(|c| c.table != config.table);
        self.configs.push(config);
        self
    }
}

/// Shared state injected into every route via [`axum::extract::State`].
///
/// `Arc` makes the clone cheap; the configs are immutable after `build()`.
#[derive(Clone, Debug)]
struct AdminState {
    configs: Arc<Vec<AdminConfig>>,
}

impl AdminState {
    fn config_for(&self, table: &str) -> Option<&AdminConfig> {
        self.configs.iter().find(|c| c.table == table)
    }
}

impl Plugin for AdminPlugin {
    fn name(&self) -> &'static str {
        "admin"
    }

    fn dependencies(&self) -> &'static [&'static str] {
        // Auth is required: the admin gates every route through
        // umbra_auth::authenticate. App::build's topological sort
        // ensures auth loads first.
        &["auth"]
    }

    fn routes(&self) -> Router {
        let state = AdminState {
            configs: Arc::new(self.configs.clone()),
        };
        Router::new()
            .route("/admin", get(index))
            .route("/admin/", get(index))
            .route("/admin/{table}/", get(list))
            .route("/admin/{table}/new", get(new_form).post(create))
            .route("/admin/{table}/action", post(run_action))
            .route("/admin/{table}/{id}", get(detail))
            .route("/admin/{table}/{id}/edit", get(edit_form).post(update))
            .route("/admin/{table}/{id}/delete", post(delete))
            .with_state(state)
    }
}

// =========================================================================
// Template environment. One Environment, built once at first use via
// OnceLock + an init function that registers every include_str! source.
// =========================================================================

static ENGINE: std::sync::OnceLock<Environment<'static>> = std::sync::OnceLock::new();

fn engine() -> &'static Environment<'static> {
    ENGINE.get_or_init(|| {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
        env.add_template("admin/base.html", include_str!("../templates/base.html"))
            .expect("admin/base.html parses");
        env.add_template("admin/index.html", include_str!("../templates/index.html"))
            .expect("admin/index.html parses");
        env.add_template("admin/list.html", include_str!("../templates/list.html"))
            .expect("admin/list.html parses");
        env.add_template(
            "admin/detail.html",
            include_str!("../templates/detail.html"),
        )
        .expect("admin/detail.html parses");
        env.add_template("admin/form.html", include_str!("../templates/form.html"))
            .expect("admin/form.html parses");
        env
    })
}

fn render(name: &str, ctx: minijinja::Value) -> Result<Html<String>, AdminError> {
    let tmpl = engine()
        .get_template(name)
        .map_err(|e| AdminError::Render(e.to_string()))?;
    let body = tmpl
        .render(ctx)
        .map_err(|e| AdminError::Render(e.to_string()))?;
    Ok(Html(body))
}

// =========================================================================
// Auth gate. Every admin handler calls require_staff() before doing
// any work. Returns either the authenticated user's name or an
// IntoResponse that 401s with the WWW-Authenticate prompt.
// =========================================================================

async fn require_staff(headers: &HeaderMap) -> Result<String, Response> {
    let creds = extract_basic_auth(headers).ok_or_else(challenge)?;
    let user = umbra_auth::authenticate::<umbra_auth::AuthUser>(&creds.username, &creds.password)
        .await
        .map_err(|_| challenge())?;
    if !user.is_staff {
        return Err((StatusCode::FORBIDDEN, "umbra-admin: not a staff user").into_response());
    }
    Ok(user.username)
}

fn challenge() -> Response {
    let mut resp = (
        StatusCode::UNAUTHORIZED,
        "umbra-admin: authentication required",
    )
        .into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        "Basic realm=\"umbra admin\"".parse().unwrap(),
    );
    resp
}

struct BasicCreds {
    username: String,
    password: String,
}

fn extract_basic_auth(headers: &HeaderMap) -> Option<BasicCreds> {
    let header = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = header.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    Some(BasicCreds {
        username: username.to_string(),
        password: password.to_string(),
    })
}

// =========================================================================
// Errors. Mapped to Response via IntoResponse so every handler can use
// `?` and get a sensible HTTP code.
// =========================================================================

#[derive(Debug)]
enum AdminError {
    NotFound(String),
    Render(String),
    Sqlx(sqlx::Error),
    BadInput(String),
}

impl From<sqlx::Error> for AdminError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        match self {
            AdminError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
            AdminError::Render(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
            AdminError::Sqlx(e) => {
                // sqlx errors can include the offending SQL fragment,
                // constraint name, or (on connection failures) parts
                // of the DSN. The admin is staff-only, but a
                // compromised staff credential shouldn't gain a free
                // SQL-reflection oracle on top of normal access. Log
                // the full error server-side; return a fixed string.
                tracing::error!(error = %e, "admin: database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
            }
            AdminError::BadInput(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        }
    }
}

// =========================================================================
// Model discovery. Walks the migration registry and filters out the
// `umbra_migrations` tracking table and any internal SQLite tables.
// =========================================================================

#[derive(Debug, Clone, Serialize)]
struct ModelEntry {
    plugin: String,
    name: String,
    table: String,
}

fn discover_models() -> Vec<(String, ModelMeta)> {
    let mut out: Vec<(String, ModelMeta)> = Vec::new();
    for plugin in umbra::migrate::registered_plugins() {
        for model in umbra::migrate::models_for_plugin(&plugin) {
            out.push((plugin.clone(), model));
        }
    }
    out
}

fn find_model(table: &str) -> Option<(String, ModelMeta)> {
    discover_models()
        .into_iter()
        .find(|(_, m)| m.table == table)
}

fn pk_column(model: &ModelMeta) -> Option<&Column> {
    model.fields.iter().find(|c| c.primary_key)
}

// =========================================================================
// Handlers.
// =========================================================================

async fn index(headers: HeaderMap) -> Response {
    let who = match require_staff(&headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let entries: Vec<ModelEntry> = discover_models()
        .into_iter()
        .map(|(plugin, m)| ModelEntry {
            plugin,
            name: m.name,
            table: m.table,
        })
        .collect();
    match render("admin/index.html", context!(user => who, models => entries)) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

// ---- list ------------------------------------------------------------------

/// Template-facing representation of one filter facet.
#[derive(Debug, Clone, Serialize)]
struct FilterFacet {
    field: String,
    values: Vec<String>,
}

async fn list(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let who = match require_staff(&headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };

    let cfg = state.config_for(&table);

    // Determine displayed columns (list_display or all).
    let display_cols: Vec<String> = if let Some(c) = cfg
        && !c.list_display.is_empty()
    {
        c.list_display.clone()
    } else {
        model.fields.iter().map(|f| f.name.clone()).collect()
    };

    // Determine ordering.
    let order_clause = build_order_clause(cfg, pk);

    // Search term from ?q=...
    let search_term = params.get("q").filter(|s| !s.is_empty()).cloned();

    // Active filter from ?filter_<field>=<value>
    let active_filter: Option<(String, String)> = params.iter().find_map(|(k, v)| {
        k.strip_prefix("filter_")
            .map(|field| (field.to_string(), v.clone()))
    });

    let pool = umbra::db::pool();
    let rows = match fetch_rows_filtered(
        &pool,
        &model,
        None,
        &display_cols,
        &order_clause,
        search_term.as_deref(),
        cfg,
        active_filter
            .as_ref()
            .map(|(f, v)| (f.as_str(), v.as_str())),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    // Build filter facets (distinct values per list_filter field).
    let mut facets: Vec<FilterFacet> = Vec::new();
    if let Some(c) = cfg {
        for field in &c.list_filter {
            let values: Vec<String> = fetch_distinct_values(&pool, &model.table, field)
                .await
                .unwrap_or_default();
            facets.push(FilterFacet {
                field: field.clone(),
                values,
            });
        }
    }

    // Actions list for template.
    let action_names: Vec<serde_json::Value> = cfg
        .map(|c| {
            c.actions
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "name": a.name,
                        "label": a.label,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let has_search = cfg.is_some_and(|c| !c.search_fields.is_empty());
    let search_val = search_term.unwrap_or_default();

    match render(
        "admin/list.html",
        context!(
            user => who,
            model => model_for_template_cols(&model, &display_cols),
            rows => rows,
            pk => pk.name.clone(),
            facets => facets,
            actions => action_names,
            has_search => has_search,
            search_val => search_val,
            active_filter => active_filter.map(|(f, v)| format!("{f}={v}")).unwrap_or_default(),
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

fn build_order_clause(cfg: Option<&AdminConfig>, pk: &Column) -> String {
    let ordering = cfg.map(|c| c.ordering.as_slice()).unwrap_or(&[]);
    if ordering.is_empty() {
        return format!("\"{}\" ASC", q(&pk.name));
    }
    ordering
        .iter()
        .map(|s| {
            if let Some(col) = s.strip_prefix('-') {
                format!("\"{}\" DESC", q(col))
            } else {
                format!("\"{}\" ASC", q(s))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Fetch distinct non-null values for a column (used by list_filter facets).
async fn fetch_distinct_values(
    pool: &SqlitePool,
    table: &str,
    field: &str,
) -> Result<Vec<String>, AdminError> {
    let sql = format!(
        "SELECT DISTINCT \"{}\" FROM \"{}\" WHERE \"{}\" IS NOT NULL ORDER BY 1 LIMIT 100",
        q(field),
        q(table),
        q(field)
    );
    let rows = sqlx::query(&sql).fetch_all(pool).await?;
    let mut out = Vec::new();
    for row in rows {
        // Try common types. Best-effort; the column might be any SqlType.
        if let Ok(v) = row.try_get::<String, _>(0) {
            out.push(v);
        } else if let Ok(v) = row.try_get::<i64, _>(0) {
            out.push(v.to_string());
        } else if let Ok(v) = row.try_get::<bool, _>(0) {
            out.push(if v { "true" } else { "false" }.to_string());
        }
    }
    Ok(out)
}

// ---- run_action ------------------------------------------------------------

async fn run_action(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    body: String,
) -> Response {
    let who = match require_staff(&headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let form: HashMap<String, String> = match serde_urlencoded::from_str(&body) {
        Ok(m) => m,
        Err(e) => return AdminError::BadInput(e.to_string()).into_response(),
    };
    let action_name = match form.get("action") {
        Some(n) => n.clone(),
        None => return AdminError::BadInput("missing 'action' field".to_string()).into_response(),
    };

    // Parse selected PKs.
    let selected_ids: Vec<i64> = form
        .iter()
        .filter(|(k, _)| k.as_str() == "selected")
        .filter_map(|(_, v)| v.parse::<i64>().ok())
        .collect();

    // Look up the action in the registered config.
    let cfg = state.config_for(&table);
    let action = cfg.and_then(|c| c.actions.iter().find(|a| a.name == action_name));
    let Some(action) = action else {
        return AdminError::NotFound(format!("no action `{action_name}` for table `{table}`"))
            .into_response();
    };

    let ctx = AdminContext {
        username: who,
        table: table.clone(),
    };
    let handler = Arc::clone(&action.handler);
    let flash = match handler(selected_ids, ctx).await {
        Ok(msg) => msg,
        Err(e) => {
            tracing::error!(error = %e, "admin: action `{action_name}` failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
    };

    // Redirect back to the list with a flash message in the query param.
    let location = format!("/admin/{table}/?flash={}", urlencoding_simple(&flash));
    Redirect::to(&location).into_response()
}

/// Minimal percent-encode for a flash message value (spaces and special chars).
fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---- detail / new_form / create / edit_form / update / delete -------------

async fn detail(headers: HeaderMap, Path((table, id)): Path<(String, String)>) -> Response {
    let who = match require_staff(&headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    let pool = umbra::db::pool();
    let all_cols: Vec<String> = model.fields.iter().map(|f| f.name.clone()).collect();
    let rows = match fetch_rows_filtered(
        &pool,
        &model,
        Some((&pk.name, &id)),
        &all_cols,
        "",
        None,
        None,
        None,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let Some(row) = rows.into_iter().next() else {
        return AdminError::NotFound(format!("no row with {} = {}", pk.name, id)).into_response();
    };
    match render(
        "admin/detail.html",
        context!(user => who, model => model_for_template(&model), row => row, pk => pk.name.clone()),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

async fn new_form(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
) -> Response {
    let who = match require_staff(&headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let cfg = state.config_for(&table);
    let fields = form_fields_for(&model, None, cfg);
    match render(
        "admin/form.html",
        context!(
            user => who,
            model => model_for_template(&model),
            fields => fields,
            verb => "Create",
            action => format!("/admin/{}/new", model.table),
            error => "",
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

async fn create(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path(table): Path<String>,
    body: String,
) -> Response {
    let who = match require_staff(&headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let form: HashMap<String, String> = match serde_urlencoded::from_str(&body) {
        Ok(m) => m,
        Err(e) => return AdminError::BadInput(e.to_string()).into_response(),
    };
    let cfg = state.config_for(&table);
    let pool = umbra::db::pool();
    match insert_row(&pool, &model, &form, cfg).await {
        Ok(_) => Redirect::to(&format!("/admin/{}/", model.table)).into_response(),
        Err(e) => {
            let fields = form_fields_for(&model, Some(&form), cfg);
            match render(
                "admin/form.html",
                context!(
                    user => who,
                    model => model_for_template(&model),
                    fields => fields,
                    verb => "Create",
                    action => format!("/admin/{}/new", model.table),
                    // Log the full error server-side; surface a
                    // generic message to the browser. Debug formatting
                    // of an AdminError can leak query fragments and
                    // constraint names — autoescape protects against
                    // XSS but the information disclosure is still
                    // worth avoiding for staff-facing flows.
                    error => sanitise_form_error(&e),
                ),
            ) {
                Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
                Err(e2) => e2.into_response(),
            }
        }
    }
}

async fn edit_form(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let who = match require_staff(&headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    let pool = umbra::db::pool();
    let all_cols: Vec<String> = model.fields.iter().map(|f| f.name.clone()).collect();
    let rows = match fetch_rows_filtered(
        &pool,
        &model,
        Some((&pk.name, &id)),
        &all_cols,
        "",
        None,
        None,
        None,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let Some(row) = rows.into_iter().next() else {
        return AdminError::NotFound(format!("no row with {} = {}", pk.name, id)).into_response();
    };
    let row_strings: HashMap<String, String> =
        row.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let cfg = state.config_for(&table);
    let fields = form_fields_for(&model, Some(&row_strings), cfg);
    match render(
        "admin/form.html",
        context!(
            user => who,
            model => model_for_template(&model),
            fields => fields,
            verb => "Edit",
            action => format!("/admin/{}/{}/edit", model.table, id),
            row => row,
            pk => pk.name.clone(),
            error => "",
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

async fn update(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
    body: String,
) -> Response {
    let who = match require_staff(&headers).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    let form: HashMap<String, String> = match serde_urlencoded::from_str(&body) {
        Ok(m) => m,
        Err(e) => return AdminError::BadInput(e.to_string()).into_response(),
    };
    let pool = umbra::db::pool();
    let cfg = state.config_for(&table);
    match update_row(&pool, &model, pk, &id, &form, cfg).await {
        Ok(_) => Redirect::to(&format!("/admin/{}/{}", model.table, id)).into_response(),
        Err(e) => {
            let fields = form_fields_for(&model, Some(&form), cfg);
            match render(
                "admin/form.html",
                context!(
                    user => who,
                    model => model_for_template(&model),
                    fields => fields,
                    verb => "Edit",
                    action => format!("/admin/{}/{}/edit", model.table, id),
                    // Log the full error server-side; surface a
                    // generic message to the browser. Debug formatting
                    // of an AdminError can leak query fragments and
                    // constraint names — autoescape protects against
                    // XSS but the information disclosure is still
                    // worth avoiding for staff-facing flows.
                    error => sanitise_form_error(&e),
                ),
            ) {
                Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
                Err(e2) => e2.into_response(),
            }
        }
    }
}

async fn delete(headers: HeaderMap, Path((table, id)): Path<(String, String)>) -> Response {
    if let Err(r) = require_staff(&headers).await {
        return r;
    }
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let Some(pk) = pk_column(&model) else {
        return AdminError::Render(format!("model `{table}` has no primary key")).into_response();
    };
    let pool = umbra::db::pool();
    let sql = format!(
        "DELETE FROM \"{}\" WHERE \"{}\" = ?",
        q(&model.table),
        q(&pk.name)
    );
    match sqlx::query(&sql).bind(&id).execute(&pool).await {
        Ok(_) => Redirect::to(&format!("/admin/{}/", model.table)).into_response(),
        Err(e) => AdminError::Sqlx(e).into_response(),
    }
}

/// Double-quote-escape a SQL identifier. See umbra-rest's
/// identically-named helper for the rationale.
fn q(name: &str) -> String {
    name.replace('"', "\"\"")
}

/// Convert an `AdminError` to a short user-facing message for the
/// form-re-render path. The full error is also logged via
/// `tracing::error!` so operators can debug.
///
/// `Sqlx` errors specifically are stripped to a generic "database
/// error" — Debug-formatting them leaks query fragments and
/// constraint names. The other variants are safe to surface (they
/// carry user-authored input like "field X required").
fn sanitise_form_error(e: &AdminError) -> String {
    match e {
        AdminError::Sqlx(sqlx_err) => {
            tracing::error!(error = %sqlx_err, "admin: form submission database error");
            "database error".to_string()
        }
        AdminError::NotFound(msg) | AdminError::Render(msg) | AdminError::BadInput(msg) => {
            msg.clone()
        }
    }
}

// =========================================================================
// Row marshalling. Read rows out as `Vec<HashMap<String, String>>` for
// the template; write rows back via per-column SqlType dispatch.
// =========================================================================

/// Fetch rows with optional search, filter, and ordering.
///
/// Arguments:
/// - `where_pk`: primary-key equality filter for detail/edit views.
/// - `display_cols`: the subset of columns to SELECT (list_display).
/// - `order_clause`: pre-built `ORDER BY ...` fragment (empty string = omit).
/// - `search_term`: value from `?q=` matched via LIKE against search_fields.
/// - `cfg`: the registered `AdminConfig` for this table (for search_fields).
/// - `active_filter`: `(field, value)` from a list_filter facet click.
#[allow(clippy::too_many_arguments)]
async fn fetch_rows_filtered(
    pool: &SqlitePool,
    model: &ModelMeta,
    where_pk: Option<(&str, &str)>,
    display_cols: &[String],
    order_clause: &str,
    search_term: Option<&str>,
    cfg: Option<&AdminConfig>,
    active_filter: Option<(&str, &str)>,
) -> Result<Vec<HashMap<String, String>>, AdminError> {
    // Build the SELECT column list from display_cols (validated against model fields).
    let valid_names: std::collections::HashSet<&str> =
        model.fields.iter().map(|c| c.name.as_str()).collect();
    let columns = display_cols
        .iter()
        .filter(|n| valid_names.contains(n.as_str()))
        .map(|n| format!("\"{}\"", n))
        .collect::<Vec<_>>()
        .join(", ");
    let columns = if columns.is_empty() {
        // Fallback: all columns.
        model
            .fields
            .iter()
            .map(|c| format!("\"{}\"", c.name))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        columns
    };

    let mut conditions: Vec<String> = Vec::new();
    let mut bind_strings: Vec<String> = Vec::new();

    // PK equality (detail / edit).
    if let Some((col, _val)) = where_pk {
        conditions.push(format!("\"{}\" = ?", q(col)));
        bind_strings.push(where_pk.unwrap().1.to_string());
    }

    // Search: LIKE %term% across search_fields, ORed together.
    if let Some(term) = search_term
        && let Some(c) = cfg
        && !c.search_fields.is_empty()
    {
        let like_clauses: Vec<String> = c
            .search_fields
            .iter()
            .filter(|f| valid_names.contains(f.as_str()))
            .map(|f| format!("\"{}\" LIKE ?", q(f)))
            .collect();
        if !like_clauses.is_empty() {
            conditions.push(format!("({})", like_clauses.join(" OR ")));
            let like_val = format!("%{term}%");
            for _ in 0..like_clauses.len() {
                bind_strings.push(like_val.clone());
            }
        }
    }

    // Active filter from facet click.
    if let Some((field, value)) = active_filter {
        if valid_names.contains(field) {
            conditions.push(format!("\"{}\" = ?", q(field)));
            bind_strings.push(value.to_string());
        }
    }

    let where_sql = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };

    let order_sql = if order_clause.is_empty() || where_pk.is_some() {
        String::new()
    } else {
        format!(" ORDER BY {order_clause}")
    };

    let limit_sql = if where_pk.is_some() {
        " LIMIT 1"
    } else {
        " LIMIT 200"
    };

    let sql = format!(
        "SELECT {columns} FROM \"{}\"{where_sql}{order_sql}{limit_sql}",
        q(&model.table)
    );

    let mut qb = sqlx::query(&sql);
    for val in &bind_strings {
        qb = qb.bind(val.clone());
    }

    let rows = qb.fetch_all(pool).await?;
    let mut out: Vec<HashMap<String, String>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut entry: HashMap<String, String> = HashMap::new();
        for col_name in display_cols {
            if let Some(col) = model.fields.iter().find(|c| &c.name == col_name) {
                entry.insert(col.name.clone(), column_to_string(&row, col)?);
            }
        }
        out.push(entry);
    }
    Ok(out)
}

fn column_to_string(row: &sqlx::sqlite::SqliteRow, col: &Column) -> Result<String, AdminError> {
    let name = col.name.as_str();
    if col.nullable {
        return Ok(match col.ty {
            SqlType::SmallInt | SqlType::Integer => row
                .try_get::<Option<i32>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::BigInt => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Real => row
                .try_get::<Option<f32>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Double => row
                .try_get::<Option<f64>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Boolean => row
                .try_get::<Option<bool>, _>(name)?
                .map_or(String::new(), |v| {
                    if v { "true" } else { "false" }.to_string()
                }),
            SqlType::Text => row.try_get::<Option<String>, _>(name)?.unwrap_or_default(),
            SqlType::Date => row
                .try_get::<Option<NaiveDate>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Time => row
                .try_get::<Option<NaiveTime>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Timestamptz => row
                .try_get::<Option<DateTime<Utc>>, _>(name)?
                .map_or(String::new(), |v| v.to_rfc3339()),
            SqlType::Uuid => row
                .try_get::<Option<Uuid>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            // Json columns render in the admin as a compact JSON
            // string so the operator can read and edit in the textarea
            // widget. Pretty-printing would be friendlier visually but
            // breaks the textarea's single-row width in the list view;
            // a richer JSON editor is a future admin upgrade.
            SqlType::Json => row
                .try_get::<Option<Value>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Array(_) => panic_array_unsupported(&col.name),
            SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
                panic_pg_only_unsupported(&col.name)
            }
            // ForeignKey renders as i64 — same as BigInt.
            SqlType::ForeignKey => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
        });
    }
    Ok(match col.ty {
        SqlType::SmallInt | SqlType::Integer => row.try_get::<i32, _>(name)?.to_string(),
        SqlType::BigInt => row.try_get::<i64, _>(name)?.to_string(),
        SqlType::Real => row.try_get::<f32, _>(name)?.to_string(),
        SqlType::Double => row.try_get::<f64, _>(name)?.to_string(),
        SqlType::Boolean => if row.try_get::<bool, _>(name)? {
            "true"
        } else {
            "false"
        }
        .to_string(),
        SqlType::Text => row.try_get::<String, _>(name)?,
        SqlType::Date => row.try_get::<NaiveDate, _>(name)?.to_string(),
        SqlType::Time => row.try_get::<NaiveTime, _>(name)?.to_string(),
        SqlType::Timestamptz => row.try_get::<DateTime<Utc>, _>(name)?.to_rfc3339(),
        SqlType::Uuid => row.try_get::<Uuid, _>(name)?.to_string(),
        SqlType::Json => row.try_get::<Value, _>(name)?.to_string(),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
        // ForeignKey renders as i64 — same as BigInt.
        SqlType::ForeignKey => row.try_get::<i64, _>(name)?.to_string(),
    })
}

/// Boot-path-bypassed sentinel for Array fields. The admin plugin runs
/// against SqlitePool today; field.backend should have failed boot.
fn panic_array_unsupported(column: &str) -> ! {
    panic!(
        "umbra-admin: column `{column}` is a Postgres-only Array; the \
         field.backend system check should have failed boot. A \
         Postgres-aware admin upgrade is a Phase 4 follow-on."
    )
}

/// Phase 4.4 sentinel for Inet/Cidr/MacAddr.
fn panic_pg_only_unsupported(column: &str) -> ! {
    panic!(
        "umbra-admin: column `{column}` is a Postgres-only network type \
         (Inet/Cidr/MacAddr); the field.backend system check should \
         have failed boot."
    )
}

async fn insert_row(
    pool: &SqlitePool,
    model: &ModelMeta,
    form: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
) -> Result<(), AdminError> {
    // Skip the PK column if it's an integer (SQLite assigns via
    // AUTOINCREMENT). For string/uuid PKs the form has to supply
    // the value. The form might supply it for integers too; in
    // that case let it through.
    // Also skip readonly fields — they can't be submitted or changed.
    let readonly: std::collections::HashSet<&str> = cfg
        .map(|c| c.readonly_fields.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    let writable: Vec<&Column> = model
        .fields
        .iter()
        .filter(|c| {
            !(readonly.contains(c.name.as_str())
                || (c.primary_key
                    && matches!(c.ty, SqlType::Integer | SqlType::BigInt | SqlType::SmallInt)
                    && form.get(&c.name).is_none_or(|v| v.is_empty())))
        })
        .collect();
    let names = writable
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = writable.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "INSERT INTO \"{}\" ({names}) VALUES ({placeholders})",
        q(&model.table)
    );
    let mut q = sqlx::query(&sql);
    for col in &writable {
        q = bind_form_value(q, col, form)?;
    }
    q.execute(pool).await?;
    Ok(())
}

async fn update_row(
    pool: &SqlitePool,
    model: &ModelMeta,
    pk: &Column,
    pk_value: &str,
    form: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
) -> Result<(), AdminError> {
    // Skip readonly fields in updates.
    let readonly: std::collections::HashSet<&str> = cfg
        .map(|c| c.readonly_fields.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    let writable: Vec<&Column> = model
        .fields
        .iter()
        .filter(|c| !c.primary_key && !readonly.contains(c.name.as_str()))
        .collect();
    let setters = writable
        .iter()
        .map(|c| format!("\"{}\" = ?", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE \"{}\" SET {setters} WHERE \"{}\" = ?",
        q(&model.table),
        q(&pk.name)
    );
    let mut q = sqlx::query(&sql);
    for col in &writable {
        q = bind_form_value(q, col, form)?;
    }
    q = q.bind(pk_value.to_string());
    q.execute(pool).await?;
    Ok(())
}

fn bind_form_value<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    col: &Column,
    form: &HashMap<String, String>,
) -> Result<sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>, AdminError> {
    let raw = form.get(&col.name).cloned().unwrap_or_default();
    // Empty + nullable → NULL. Empty + boolean → false (HTML form
    // omits the checkbox when unchecked). Empty + non-nullable
    // non-boolean → reject.
    if raw.is_empty() {
        return Ok(match col.ty {
            SqlType::Boolean => q.bind(false),
            _ if col.nullable => bind_null(q, col),
            _ => {
                return Err(AdminError::BadInput(format!(
                    "field `{}` is required",
                    col.name
                )));
            }
        });
    }
    Ok(match col.ty {
        SqlType::SmallInt | SqlType::Integer => q.bind(
            raw.parse::<i32>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::BigInt => q.bind(
            raw.parse::<i64>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Real => q.bind(
            raw.parse::<f32>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Double => q.bind(
            raw.parse::<f64>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Boolean => q.bind(matches!(raw.as_str(), "true" | "on" | "1")),
        SqlType::Text => q.bind(raw),
        SqlType::Date => q.bind(
            raw.parse::<NaiveDate>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Time => q.bind(
            raw.parse::<NaiveTime>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Timestamptz => {
            // HTML's `datetime-local` emits `2026-05-30T17:00` with
            // no timezone. Assume UTC.
            let s = if raw.contains(':') && !raw.contains('+') && !raw.ends_with('Z') {
                format!("{raw}:00Z")
            } else {
                raw.clone()
            };
            let parsed = DateTime::parse_from_rfc3339(&s)
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?;
            q.bind(parsed.with_timezone(&Utc))
        }
        SqlType::Uuid => q.bind(
            Uuid::parse_str(&raw)
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        // The admin textarea returned the JSON document as a string.
        // Parse it back to a serde_json::Value so the binder stores
        // structured JSON rather than the literal text.
        SqlType::Json => q.bind(
            serde_json::from_str::<Value>(&raw)
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
        // ForeignKey fields store i64 PKs — parse and bind the same as BigInt.
        SqlType::ForeignKey => q.bind(
            raw.parse::<i64>()
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
    })
}

fn bind_null<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    col: &Column,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    match col.ty {
        SqlType::SmallInt | SqlType::Integer => q.bind(None::<i32>),
        SqlType::BigInt => q.bind(None::<i64>),
        SqlType::Real => q.bind(None::<f32>),
        SqlType::Double => q.bind(None::<f64>),
        SqlType::Boolean => q.bind(None::<bool>),
        SqlType::Text => q.bind(None::<String>),
        SqlType::Date => q.bind(None::<NaiveDate>),
        SqlType::Time => q.bind(None::<NaiveTime>),
        SqlType::Timestamptz => q.bind(None::<DateTime<Utc>>),
        SqlType::Uuid => q.bind(None::<Uuid>),
        SqlType::Json => q.bind(None::<Value>),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
        // ForeignKey stores i64 — bind as nullable i64.
        SqlType::ForeignKey => q.bind(None::<i64>),
    }
}

// =========================================================================
// Template helpers.
// =========================================================================

#[derive(Debug, Clone, Serialize)]
struct FormField {
    name: String,
    kind: &'static str,
    value: String,
    nullable: bool,
    readonly: bool,
}

fn form_fields_for(
    model: &ModelMeta,
    prefill: Option<&HashMap<String, String>>,
    cfg: Option<&AdminConfig>,
) -> Vec<FormField> {
    let readonly_set: std::collections::HashSet<&str> = cfg
        .map(|c| c.readonly_fields.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();

    model
        .fields
        .iter()
        .filter(|c| !c.primary_key) // PK isn't editable in the form
        .map(|c| FormField {
            name: c.name.clone(),
            kind: input_kind(c.ty),
            value: prefill
                .and_then(|m| m.get(&c.name))
                .cloned()
                .unwrap_or_default(),
            nullable: c.nullable,
            readonly: readonly_set.contains(c.name.as_str()),
        })
        .collect()
}

fn input_kind(ty: SqlType) -> &'static str {
    match ty {
        SqlType::SmallInt
        | SqlType::Integer
        | SqlType::BigInt
        | SqlType::Real
        | SqlType::Double => "number",
        SqlType::Boolean => "bool",
        SqlType::Text | SqlType::Uuid => "text",
        SqlType::Date => "date",
        SqlType::Time => "time",
        SqlType::Timestamptz => "datetime-local",
        // JSON columns render as a textarea — same widget the admin
        // already uses for long-form Text overrides could pick when
        // they land. The form template keys on this string; the
        // "textarea" value is new and the template needs the matching
        // branch landed alongside.
        SqlType::Json => "textarea",
        // Array fields are Postgres-only; the admin form path runs on
        // SqlitePool today and the field.backend system check fires at
        // boot. Return a placeholder widget name; the template path
        // never sees this since boot failed first.
        SqlType::Array(_) => "textarea",
        // Phase 4.4 network types — same SQLite-gated story. Widget
        // is "text" since the values render as strings.
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr => "text",
        // Phase 4.3 full-text — textarea since tsvector content is
        // typically multi-line lexeme output.
        SqlType::FullText => "textarea",
        // ForeignKey renders as a numeric input — same as BigInt.
        SqlType::ForeignKey => "number",
    }
}

#[derive(Debug, Clone, Serialize)]
struct ModelView {
    name: String,
    table: String,
    fields: Vec<ColumnView>,
}

#[derive(Debug, Clone, Serialize)]
struct ColumnView {
    name: String,
    nullable: bool,
    primary_key: bool,
}

/// Full model view with all fields (used by detail).
fn model_for_template(model: &ModelMeta) -> ModelView {
    ModelView {
        name: model.name.clone(),
        table: model.table.clone(),
        fields: model
            .fields
            .iter()
            .map(|c| ColumnView {
                name: c.name.clone(),
                nullable: c.nullable,
                primary_key: c.primary_key,
            })
            .collect(),
    }
}

/// Filtered model view for list (only display_cols shown).
fn model_for_template_cols(model: &ModelMeta, display_cols: &[String]) -> ModelView {
    let valid: std::collections::HashSet<&str> =
        model.fields.iter().map(|c| c.name.as_str()).collect();
    let fields: Vec<ColumnView> = display_cols
        .iter()
        .filter(|n| valid.contains(n.as_str()))
        .map(|n| {
            let col = model.fields.iter().find(|c| &c.name == n).unwrap();
            ColumnView {
                name: col.name.clone(),
                nullable: col.nullable,
                primary_key: col.primary_key,
            }
        })
        .collect();
    ModelView {
        name: model.name.clone(),
        table: model.table.clone(),
        fields,
    }
}

// Quiet unused-import lints in case axum's `Json` isn't referenced
// after a future refactor. Keeping the import line stable.
#[allow(dead_code)]
fn _unused_json_marker() -> Option<Json<()>> {
    None
}
