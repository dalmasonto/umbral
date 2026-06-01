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
//! Register an [`AdminModel`] for a model to control list columns, filter
//! facets, search, ordering, bulk actions, and readonly fields. See
//! [`AdminPlugin::register`] and the [`config`] module.
//!
//! ## Auth
//!
//! Every admin route requires a session-backed staff user. If the
//! session is missing or the user is not staff, the handler redirects
//! to `GET /admin/login?next=<current-url>`. `POST /admin/login` verifies
//! credentials via [`umbra_auth::authenticate`], creates a session via
//! [`umbra_sessions::login`], then redirects to `next`.
//!
//! ## Templates
//!
//! Six `include_str!`-embedded Jinja templates live in `templates/`.
//! The admin owns its own minijinja `Environment`. `admin/base.html`
//! is the shell (sidebar + topbar + content slot); the other five
//! extend it.
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
pub mod registry;

pub use config::{Action, AdminConfig, AdminContext, AdminModel, InlineModel};
pub use registry::{AdminRegistration, AdminRegistry, App as AdminApp};

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use minijinja::{Environment, context};
use serde::Serialize;
use serde_json::Value;
use sqlx::{Row, SqlitePool};
use umbra::migrate::{Column, ModelMeta};
use umbra::orm::SqlType;
use umbra::prelude::*;
use umbra::web::{HeaderMap, Html, IntoResponse, Json, Path, Redirect, Response, StatusCode, post};
use uuid::Uuid;

// =========================================================================
// Plugin struct
// =========================================================================

/// The plugin. Mounts every admin route under `/admin`.
///
/// Use [`AdminPlugin::register`] to attach an [`AdminModel`] before
/// passing the plugin to `App::builder().plugin(...)`.
///
/// ```ignore
/// use umbra_admin::{AdminPlugin, AdminModel, Action};
///
/// let admin = AdminPlugin::default()
///     .register(
///         AdminModel::new("post")
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
    registry: AdminRegistry,
}

impl AdminPlugin {
    /// Register an [`AdminModel`] for one model. Chainable.
    ///
    /// If two configs are registered for the same table the last one wins
    /// (same semantics as Django's `site.register` overwriting on duplicate).
    ///
    /// The plugin name defaults to `"admin"` for models registered before
    /// the plugin is installed into the app. From M7+ plugins will pass
    /// their own name via `Plugin::admin_register` on the registry.
    pub fn register(mut self, model: AdminModel) -> Self {
        self.registry.register("admin", model);
        self
    }

    /// Register an [`AdminModel`] for a specific plugin name.
    ///
    /// This is the method the `Plugin::routes` / `on_ready` pathway uses
    /// when a plugin contributes its own admin registrations. The sidebar
    /// groups models by the `plugin_name` supplied here.
    pub fn register_for(mut self, plugin_name: &str, model: AdminModel) -> Self {
        self.registry.register(plugin_name, model);
        self
    }
}

/// Shared state injected into every route via [`axum::extract::State`].
///
/// `Arc` makes the clone cheap; the registry is immutable after `build()`.
#[derive(Clone, Debug)]
struct AdminState {
    registry: Arc<AdminRegistry>,
}

impl AdminState {
    fn config_for(&self, table: &str) -> Option<&AdminConfig> {
        self.registry.get(table).map(|r| &r.model)
    }
}

impl Plugin for AdminPlugin {
    fn name(&self) -> &'static str {
        "admin"
    }

    fn dependencies(&self) -> &'static [&'static str] {
        // Auth is required: login verifies credentials via umbra-auth.
        // Sessions is required: login creates sessions.
        &["auth", "sessions"]
    }

    fn routes(&self) -> Router {
        let state = AdminState {
            registry: Arc::new(self.registry.clone()),
        };
        Router::new()
            // Login / logout (no auth required)
            .route(
                "/admin/login",
                axum::routing::get(login_get).post(login_post),
            )
            .route("/admin/logout", axum::routing::get(logout_handler))
            // Index + CRUD routes (all require staff session)
            .route("/admin", axum::routing::get(index))
            .route("/admin/", axum::routing::get(index))
            .route("/admin/{table}/", axum::routing::get(list))
            .route(
                "/admin/{table}/new",
                axum::routing::get(new_form).post(create),
            )
            .route("/admin/{table}/action", post(run_action))
            .route("/admin/{table}/{id}", axum::routing::get(detail))
            .route(
                "/admin/{table}/{id}/edit",
                axum::routing::get(edit_form).post(update),
            )
            .route("/admin/{table}/{id}/delete", post(delete))
            .with_state(state)
    }
}

// =========================================================================
// Template environment.
// =========================================================================

static ENGINE: std::sync::OnceLock<Environment<'static>> = std::sync::OnceLock::new();

fn engine() -> &'static Environment<'static> {
    ENGINE.get_or_init(|| {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
        env.add_template(
            "admin/wrapper.html",
            include_str!("../templates/wrapper.html"),
        )
        .expect("admin/wrapper.html parses");
        env.add_template("admin/base.html", include_str!("../templates/base.html"))
            .expect("admin/base.html parses");
        env.add_template("admin/login.html", include_str!("../templates/login.html"))
            .expect("admin/login.html parses");
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
// Sidebar context helpers.
//
// Every handler that renders the authenticated shell calls `sidebar_apps`
// to pass the nav tree into the template.
// =========================================================================

/// Template-facing representation of one sidebar model link.
#[derive(Debug, Clone, Serialize)]
struct SidebarModel {
    table: String,
    label: String,
    icon: String,
}

/// Template-facing group of models for one plugin.
#[derive(Debug, Clone, Serialize)]
struct SidebarApp {
    plugin: String,
    label: String,
    models: Vec<SidebarModel>,
}

fn sidebar_apps(state: &AdminState, user: &umbra_auth::AuthUser) -> Vec<SidebarApp> {
    state
        .registry
        .apps(user)
        .into_iter()
        .map(|app| SidebarApp {
            plugin: app.plugin.clone(),
            label: app.label.clone(),
            models: app
                .models
                .into_iter()
                .map(|r| SidebarModel {
                    table: r.model.table.clone(),
                    label: r.label.clone(),
                    icon: r.icon.clone().unwrap_or_else(|| "database".to_string()),
                })
                .collect(),
        })
        .collect()
}

// =========================================================================
// CSRF helpers for the login form.
//
// umbra-security's CSRF middleware uses double-submit-cookie with the
// `x-csrf-token` header. HTML forms can't set custom headers, so the
// login page needs its own per-session token stored in the session `data`
// map and submitted as a hidden form field.
// =========================================================================

const ADMIN_CSRF_SESSION_KEY: &str = "_umbra_admin_csrf";

/// Issue a CSRF token for the admin login form.
///
/// Generates a fresh token, stores it in the session `data` map,
/// and returns the token for embedding in the login template.
///
/// The session token must be the raw token from the request cookie
/// (used by `umbra_sessions::set_data`).
async fn issue_login_csrf(session_token: &str) -> String {
    let token = umbra_security::generate_token();
    let _ = umbra_sessions::set_data(session_token, ADMIN_CSRF_SESSION_KEY, &token).await;
    token
}

/// Verify the login form CSRF token.
///
/// Returns `true` if the submitted form token matches what we stored
/// in the session. Constant-time comparison via `subtle::ConstantTimeEq`
/// is not needed here because an attacker who can read the session DB
/// already has the token — the protection is purely against CSRF
/// (cross-site forms that can't read the session cookie).
async fn verify_login_csrf(session_token: &str, submitted: &str) -> bool {
    if submitted.is_empty() {
        return false;
    }
    let session = match umbra_sessions::read_session(session_token).await {
        Ok(Some(s)) => s,
        _ => return false,
    };
    match umbra_sessions::get_data::<String>(&session, ADMIN_CSRF_SESSION_KEY) {
        Ok(Some(stored)) => stored == submitted,
        _ => false,
    }
}

// =========================================================================
// Auth gate — session-based.
//
// require_staff looks up the session cookie, reads the session row,
// hydrates the AuthUser, and checks is_staff. On any failure it
// redirects to /admin/login?next=<path> instead of issuing a
// WWW-Authenticate challenge.
// =========================================================================

/// Check that the request carries a valid staff session.
///
/// On success: returns the authenticated [`umbra_auth::AuthUser`].
/// On failure: returns a [`Response`] that redirects to the login page
/// (307 Temporary Redirect with `?next=<requested_path>`).
async fn require_staff(
    headers: &HeaderMap,
    current_path: &str,
) -> Result<umbra_auth::AuthUser, Response> {
    // Encode the `next` parameter: drop double-slash / external URLs.
    let next = sanitise_next(current_path);
    let login_redirect = || {
        let location = format!("/admin/login?next={}", urlencoding_simple(&next));
        Redirect::to(&location).into_response()
    };

    let user = match umbra_sessions::current_user(headers).await {
        Ok(Some(u)) => u,
        _ => return Err(login_redirect()),
    };
    if !user.is_staff {
        return Err((StatusCode::FORBIDDEN, "umbra-admin: not a staff user").into_response());
    }
    Ok(user)
}

// =========================================================================
// Login / Logout handlers.
// =========================================================================

/// `GET /admin/login` — render the login form.
///
/// If the request has no session cookie, a fresh anonymous session is
/// created and a `Set-Cookie` header is added to the response. This
/// ensures there is always a session available to anchor the CSRF token,
/// even when the `SessionsPlugin` auto-layer is disabled (the common
/// case for admin-only deployments that don't want every request to
/// create a session row).
async fn login_get(headers: HeaderMap, Query(params): Query<HashMap<String, String>>) -> Response {
    // If already logged in as staff, redirect straight to /admin/.
    if let Ok(Some(user)) = umbra_sessions::current_user(&headers).await {
        if user.is_staff {
            let next = params
                .get("next")
                .map(|n| sanitise_next(n))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "/admin/".to_string());
            return Redirect::to(&next).into_response();
        }
    }

    let next = params
        .get("next")
        .map(|n| sanitise_next(n))
        .unwrap_or_default();

    // Obtain a session token for the CSRF anchor.
    // If the request already has a valid session cookie, reuse it.
    // Otherwise create a fresh anonymous session so we have somewhere
    // to store the CSRF token.
    let existing_token = umbra_sessions::cookie_from_headers(&headers);

    // Validate the existing cookie if present.
    let valid_existing = if let Some(ref tok) = existing_token {
        umbra_sessions::read_session(tok)
            .await
            .ok()
            .flatten()
            .is_some()
    } else {
        false
    };

    let (session_token, new_cookie) = if valid_existing {
        (existing_token.unwrap(), None)
    } else {
        // Create a fresh anonymous session.
        match umbra_sessions::create_session(None, None).await {
            Ok(raw) => {
                let cookie_str = umbra_sessions::set_cookie_header(&raw, None);
                (raw, Some(cookie_str))
            }
            Err(e) => {
                tracing::error!(error = %e, "admin: login_get: failed to create anonymous session");
                // Fallback: render without CSRF protection. The POST
                // will reject the empty token and redirect back here.
                let html = render(
                    "admin/login.html",
                    context!(csrf_token => "", next => next, error => "", prefill_username => ""),
                );
                return match html {
                    Ok(h) => h.into_response(),
                    Err(e2) => e2.into_response(),
                };
            }
        }
    };

    let csrf_token = issue_login_csrf(&session_token).await;

    let html = match render(
        "admin/login.html",
        context!(
            csrf_token       => csrf_token,
            next             => next,
            error            => "",
            prefill_username => "",
        ),
    ) {
        Ok(h) => h,
        Err(e) => return e.into_response(),
    };

    // If we minted a new session, attach it to the response.
    if let Some(cookie_str) = new_cookie {
        let mut resp = html.into_response();
        if let Ok(value) = cookie_str.parse::<axum::http::HeaderValue>() {
            resp.headers_mut()
                .insert(axum::http::header::SET_COOKIE, value);
        }
        resp
    } else {
        html.into_response()
    }
}

/// `POST /admin/login` — verify credentials, create session, redirect.
async fn login_post(headers: HeaderMap, body: String) -> Response {
    let form: HashMap<String, String> = match serde_urlencoded::from_str(&body) {
        Ok(m) => m,
        Err(_) => return bad_login_response("Invalid form submission.", "", ""),
    };

    let username = form.get("username").map(|s| s.as_str()).unwrap_or("");
    let password = form.get("password").map(|s| s.as_str()).unwrap_or("");
    let next_raw = form.get("next").map(|s| s.as_str()).unwrap_or("");
    let next = sanitise_next(next_raw);
    let submitted_csrf = form.get("csrf_token").map(|s| s.as_str()).unwrap_or("");

    // CSRF check.
    let session_token = umbra_sessions::cookie_from_headers(&headers);
    let csrf_ok = if let Some(ref tok) = session_token {
        verify_login_csrf(tok, submitted_csrf).await
    } else {
        false
    };
    if !csrf_ok {
        // Refresh the csrf token and re-render the form.
        let new_csrf = if let Some(ref tok) = session_token {
            issue_login_csrf(tok).await
        } else {
            String::new()
        };
        return bad_login_response_with_csrf(
            "Your session expired. Please try again.",
            username,
            &next,
            &new_csrf,
        );
    }

    // Authenticate credentials. Same error message regardless of which
    // field is wrong — timing-safe because we call the hash comparison
    // unconditionally when the user exists (umbra_auth handles this).
    let user = match umbra_auth::authenticate::<umbra_auth::AuthUser>(username, password).await {
        Ok(u) => u,
        Err(_) => {
            let new_csrf = if let Some(ref tok) = session_token {
                issue_login_csrf(tok).await
            } else {
                String::new()
            };
            return bad_login_response_with_csrf(
                "The username or password you entered is incorrect.",
                username,
                &next,
                &new_csrf,
            );
        }
    };

    if !user.is_staff {
        let new_csrf = if let Some(ref tok) = session_token {
            issue_login_csrf(tok).await
        } else {
            String::new()
        };
        return bad_login_response_with_csrf(
            "This account does not have admin access.",
            username,
            &next,
            &new_csrf,
        );
    }

    // Login: create session + set cookie.
    let redirect_to = if next.is_empty() {
        "/admin/".to_string()
    } else {
        next.clone()
    };
    let mut response = Redirect::to(&redirect_to).into_response();
    if let Err(e) =
        umbra_sessions::login_with_request(&headers, response.headers_mut(), &user).await
    {
        tracing::error!(error = %e, "admin: login: session creation failed");
        return (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response();
    }
    response
}

/// Render the login template with a generic error banner.
fn bad_login_response(error: &str, prefill_username: &str, next: &str) -> Response {
    bad_login_response_with_csrf(error, prefill_username, next, "")
}

fn bad_login_response_with_csrf(
    error: &str,
    prefill_username: &str,
    next: &str,
    csrf_token: &str,
) -> Response {
    match render(
        "admin/login.html",
        context!(
            csrf_token       => csrf_token,
            next             => next,
            error            => error,
            prefill_username => prefill_username,
        ),
    ) {
        Ok(html) => (StatusCode::UNPROCESSABLE_ENTITY, html).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /admin/logout` — destroy session, redirect to login.
async fn logout_handler(headers: HeaderMap) -> Response {
    let mut response = Redirect::to("/admin/login").into_response();
    let _ = umbra_sessions::logout(&headers, response.headers_mut()).await;
    response
}

// =========================================================================
// Validate the `next` redirect target.
//
// Accept only same-origin relative paths starting with `/admin/` or `/admin`.
// Reject: protocol-relative `//`, absolute `http://`, or anything that
// doesn't start with the admin prefix.
// =========================================================================

fn sanitise_next(raw: &str) -> String {
    let trimmed = raw.trim();
    // Must be a relative path starting with /admin (not // or http://).
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("//") || trimmed.contains("://") {
        return "/admin/".to_string();
    }
    if !trimmed.starts_with("/admin") {
        return "/admin/".to_string();
    }
    trimmed.to_string()
}

// =========================================================================
// Errors.
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
                tracing::error!(error = %e, "admin: database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
            }
            AdminError::BadInput(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        }
    }
}

// =========================================================================
// Model discovery.
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

async fn index(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    let user = match require_staff(&headers, "/admin/").await {
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
    let apps = sidebar_apps(&state, &user);
    match render(
        "admin/index.html",
        context!(
            user         => user.username.clone(),
            models       => entries,
            apps         => apps,
            active_table => "",
            breadcrumbs  => Vec::<serde_json::Value>::new(),
        ),
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}

// ---- list ------------------------------------------------------------------

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
    let path = format!("/admin/{table}/");
    let user = match require_staff(&headers, &path).await {
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

    let display_cols: Vec<String> = if let Some(c) = cfg
        && !c.list_display.is_empty()
    {
        c.list_display.clone()
    } else {
        model.fields.iter().map(|f| f.name.clone()).collect()
    };

    let order_clause = build_order_clause(cfg, pk);
    let search_term = params.get("q").filter(|s| !s.is_empty()).cloned();
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

    let mut facets: Vec<FilterFacet> = Vec::new();
    if let Some(c) = cfg {
        for field in &c.list_filter {
            let values = fetch_distinct_values(&pool, &model.table, field)
                .await
                .unwrap_or_default();
            facets.push(FilterFacet {
                field: field.clone(),
                values,
            });
        }
    }

    let action_names: Vec<serde_json::Value> = cfg
        .map(|c| {
            c.actions
                .iter()
                .map(|a| serde_json::json!({ "name": a.name, "label": a.label }))
                .collect()
        })
        .unwrap_or_default();

    let has_search = cfg.is_some_and(|c| !c.search_fields.is_empty());
    let search_val = search_term.unwrap_or_default();
    let apps = sidebar_apps(&state, &user);
    let breadcrumbs =
        vec![serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") })];

    match render(
        "admin/list.html",
        context!(
            user         => user.username.clone(),
            model        => model_for_template_cols(&model, &display_cols),
            rows         => rows,
            pk           => pk.name.clone(),
            facets       => facets,
            actions      => action_names,
            has_search   => has_search,
            search_val   => search_val,
            active_filter => active_filter.map(|(f, v)| format!("{f}={v}")).unwrap_or_default(),
            apps         => apps,
            active_table => table,
            breadcrumbs  => breadcrumbs,
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
    let path = format!("/admin/{table}/action");
    let who = match require_staff(&headers, &path).await {
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
    let selected_ids: Vec<i64> = form
        .iter()
        .filter(|(k, _)| k.as_str() == "selected")
        .filter_map(|(_, v)| v.parse::<i64>().ok())
        .collect();

    let cfg = state.config_for(&table);
    let action = cfg.and_then(|c| c.actions.iter().find(|a| a.name == action_name));
    let Some(action) = action else {
        return AdminError::NotFound(format!("no action `{action_name}` for table `{table}`"))
            .into_response();
    };

    let ctx = AdminContext {
        username: who.username,
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
    let location = format!("/admin/{table}/?flash={}", urlencoding_simple(&flash));
    Redirect::to(&location).into_response()
}

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

async fn detail(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}");
    let user = match require_staff(&headers, &path).await {
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
    let apps = sidebar_apps(&state, &user);
    let breadcrumbs = vec![
        serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") }),
        serde_json::json!({ "label": format!("#{id}"), "url": format!("/admin/{table}/{id}") }),
    ];
    match render(
        "admin/detail.html",
        context!(
            user         => user.username.clone(),
            model        => model_for_template(&model),
            row          => row,
            pk           => pk.name.clone(),
            apps         => apps,
            active_table => table,
            breadcrumbs  => breadcrumbs,
        ),
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
    let path = format!("/admin/{table}/new");
    let user = match require_staff(&headers, &path).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some((_, model)) = find_model(&table) else {
        return AdminError::NotFound(format!("no model with table `{table}`")).into_response();
    };
    let cfg = state.config_for(&table);
    let fields = form_fields_for(&model, None, cfg);
    let apps = sidebar_apps(&state, &user);
    let breadcrumbs = vec![
        serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") }),
        serde_json::json!({ "label": "Add", "url": format!("/admin/{table}/new") }),
    ];
    match render(
        "admin/form.html",
        context!(
            user         => user.username.clone(),
            model        => model_for_template(&model),
            fields       => fields,
            verb         => "Create",
            action       => format!("/admin/{}/new", model.table),
            error        => "",
            apps         => apps,
            active_table => table,
            breadcrumbs  => breadcrumbs,
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
    let path = format!("/admin/{table}/new");
    let user = match require_staff(&headers, &path).await {
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
            let apps = sidebar_apps(&state, &user);
            let breadcrumbs = vec![
                serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") }),
                serde_json::json!({ "label": "Add", "url": format!("/admin/{table}/new") }),
            ];
            match render(
                "admin/form.html",
                context!(
                    user         => user.username.clone(),
                    model        => model_for_template(&model),
                    fields       => fields,
                    verb         => "Create",
                    action       => format!("/admin/{}/new", model.table),
                    error        => sanitise_form_error(&e),
                    apps         => apps,
                    active_table => table,
                    breadcrumbs  => breadcrumbs,
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
    let path = format!("/admin/{table}/{id}/edit");
    let user = match require_staff(&headers, &path).await {
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
    let apps = sidebar_apps(&state, &user);
    let breadcrumbs = vec![
        serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") }),
        serde_json::json!({ "label": format!("#{id}"), "url": format!("/admin/{table}/{id}") }),
        serde_json::json!({ "label": "Edit", "url": format!("/admin/{table}/{id}/edit") }),
    ];
    match render(
        "admin/form.html",
        context!(
            user         => user.username.clone(),
            model        => model_for_template(&model),
            fields       => fields,
            verb         => "Edit",
            action       => format!("/admin/{}/{}/edit", model.table, id),
            row          => row,
            pk           => pk.name.clone(),
            error        => "",
            apps         => apps,
            active_table => table,
            breadcrumbs  => breadcrumbs,
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
    let path = format!("/admin/{table}/{id}/edit");
    let user = match require_staff(&headers, &path).await {
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
            let apps = sidebar_apps(&state, &user);
            let breadcrumbs = vec![
                serde_json::json!({ "label": model.name.clone(), "url": format!("/admin/{table}/") }),
                serde_json::json!({ "label": format!("#{id}"), "url": format!("/admin/{table}/{id}") }),
                serde_json::json!({ "label": "Edit", "url": format!("/admin/{table}/{id}/edit") }),
            ];
            match render(
                "admin/form.html",
                context!(
                    user         => user.username.clone(),
                    model        => model_for_template(&model),
                    fields       => fields,
                    verb         => "Edit",
                    action       => format!("/admin/{}/{}/edit", model.table, id),
                    error        => sanitise_form_error(&e),
                    apps         => apps,
                    active_table => table,
                    breadcrumbs  => breadcrumbs,
                ),
            ) {
                Ok(html) => (StatusCode::BAD_REQUEST, html).into_response(),
                Err(e2) => e2.into_response(),
            }
        }
    }
}

async fn delete(
    State(_state): State<AdminState>,
    headers: HeaderMap,
    Path((table, id)): Path<(String, String)>,
) -> Response {
    let path = format!("/admin/{table}/{id}/delete");
    if let Err(r) = require_staff(&headers, &path).await {
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

fn q(name: &str) -> String {
    name.replace('"', "\"\"")
}

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
// Row marshalling.
// =========================================================================

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
    let valid_names: std::collections::HashSet<&str> =
        model.fields.iter().map(|c| c.name.as_str()).collect();
    let columns = display_cols
        .iter()
        .filter(|n| valid_names.contains(n.as_str()))
        .map(|n| format!("\"{}\"", n))
        .collect::<Vec<_>>()
        .join(", ");
    let columns = if columns.is_empty() {
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

    if let Some((col, _val)) = where_pk {
        conditions.push(format!("\"{}\" = ?", q(col)));
        bind_strings.push(where_pk.unwrap().1.to_string());
    }

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
            SqlType::Json => row
                .try_get::<Option<Value>, _>(name)?
                .map_or(String::new(), |v| v.to_string()),
            SqlType::Array(_) => panic_array_unsupported(&col.name),
            SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
                panic_pg_only_unsupported(&col.name)
            }
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
        SqlType::ForeignKey => row.try_get::<i64, _>(name)?.to_string(),
    })
}

fn panic_array_unsupported(column: &str) -> ! {
    panic!(
        "umbra-admin: column `{column}` is a Postgres-only Array; the \
         field.backend system check should have failed boot."
    )
}

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
    let mut qb = sqlx::query(&sql);
    for col in &writable {
        qb = bind_form_value(qb, col, form)?;
    }
    qb.execute(pool).await?;
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
    let mut qb = sqlx::query(&sql);
    for col in &writable {
        qb = bind_form_value(qb, col, form)?;
    }
    qb = qb.bind(pk_value.to_string());
    qb.execute(pool).await?;
    Ok(())
}

fn bind_form_value<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    col: &Column,
    form: &HashMap<String, String>,
) -> Result<sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>, AdminError> {
    let raw = form.get(&col.name).cloned().unwrap_or_default();
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
        SqlType::Json => q.bind(
            serde_json::from_str::<Value>(&raw)
                .map_err(|e| AdminError::BadInput(format!("{}: {e}", col.name)))?,
        ),
        SqlType::Array(_) => panic_array_unsupported(&col.name),
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText => {
            panic_pg_only_unsupported(&col.name)
        }
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
        .filter(|c| !c.primary_key)
        .map(|c| {
            let raw = prefill
                .and_then(|m| m.get(&c.name))
                .cloned()
                .unwrap_or_default();
            FormField {
                name: c.name.clone(),
                kind: input_kind(c.ty),
                value: format_for_input(&raw, c.ty),
                nullable: c.nullable,
                readonly: readonly_set.contains(c.name.as_str()),
            }
        })
        .collect()
}

fn format_for_input(raw: &str, ty: SqlType) -> String {
    if raw.is_empty() {
        return String::new();
    }
    match ty {
        SqlType::Timestamptz => match chrono::DateTime::parse_from_rfc3339(raw) {
            Ok(dt) => dt.format("%Y-%m-%dT%H:%M").to_string(),
            Err(_) => raw.to_string(),
        },
        SqlType::Time => {
            if let Some(dot) = raw.find('.') {
                raw[..dot].to_string()
            } else {
                raw.to_string()
            }
        }
        _ => raw.to_string(),
    }
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
        SqlType::Json => "textarea",
        SqlType::Array(_) => "textarea",
        SqlType::Inet | SqlType::Cidr | SqlType::MacAddr => "text",
        SqlType::FullText => "textarea",
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

#[allow(dead_code)]
fn _unused_json_marker() -> Option<Json<()>> {
    None
}

// =========================================================================
// Unit tests (pure logic — no DB needed).
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_for_input_coerces_rfc3339_to_datetime_local() {
        let coerced = format_for_input("2026-05-30T12:00:00+00:00", SqlType::Timestamptz);
        assert_eq!(coerced, "2026-05-30T12:00");
    }

    #[test]
    fn format_for_input_handles_rfc3339_with_offset() {
        let coerced = format_for_input("2026-05-30T17:00:00+05:00", SqlType::Timestamptz);
        assert_eq!(coerced, "2026-05-30T17:00");
    }

    #[test]
    fn format_for_input_empty_stays_empty() {
        assert_eq!(format_for_input("", SqlType::Timestamptz), "");
        assert_eq!(format_for_input("", SqlType::Time), "");
        assert_eq!(format_for_input("", SqlType::Text), "");
    }

    #[test]
    fn format_for_input_passes_through_simple_types() {
        assert_eq!(format_for_input("2026-05-30", SqlType::Date), "2026-05-30");
        assert_eq!(format_for_input("hello", SqlType::Text), "hello");
        assert_eq!(format_for_input("42", SqlType::BigInt), "42");
    }

    #[test]
    fn format_for_input_trims_subsecond_time() {
        assert_eq!(format_for_input("12:34:56.789", SqlType::Time), "12:34:56");
        assert_eq!(format_for_input("12:34:56", SqlType::Time), "12:34:56");
        assert_eq!(format_for_input("12:34", SqlType::Time), "12:34");
    }

    #[test]
    fn format_for_input_passes_through_bad_rfc3339_unchanged() {
        let bad = "not-a-valid-timestamp";
        assert_eq!(format_for_input(bad, SqlType::Timestamptz), bad);
    }

    #[test]
    fn sanitise_next_rejects_external_urls() {
        assert_eq!(sanitise_next("http://evil.com/"), "/admin/");
        assert_eq!(sanitise_next("https://evil.com/"), "/admin/");
        assert_eq!(sanitise_next("//evil.com/"), "/admin/");
    }

    #[test]
    fn sanitise_next_rejects_non_admin_paths() {
        assert_eq!(sanitise_next("/app/dashboard"), "/admin/");
        assert_eq!(sanitise_next("/login"), "/admin/");
    }

    #[test]
    fn sanitise_next_accepts_admin_paths() {
        assert_eq!(sanitise_next("/admin/"), "/admin/");
        assert_eq!(sanitise_next("/admin/note/"), "/admin/note/");
        assert_eq!(sanitise_next("/admin"), "/admin");
    }

    #[test]
    fn sanitise_next_empty_stays_empty() {
        assert_eq!(sanitise_next(""), "");
        assert_eq!(sanitise_next("   "), "");
    }

    #[test]
    fn admin_model_defaults() {
        let m = AdminModel::new("post");
        assert_eq!(m.get_list_per_page(), 25);
        assert!(m.inlines.is_empty());
        assert!(m.label.is_none());
        assert!(m.icon.is_none());
    }

    #[test]
    fn admin_config_alias_compiles() {
        // The type alias must be identical to AdminModel at the Rust level.
        let _: AdminConfig = AdminModel::new("test");
    }
}
