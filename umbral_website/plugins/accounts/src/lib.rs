//! AccountsPlugin — the website's account surface: signup, login,
//! logout, and a profile page that lists OAuth connections.
//!
//! Password auth uses `umbral-auth` (`create_user` / `authenticate`);
//! social login + connect is provided by `umbral-oauth` (the buttons just
//! link to `/oauth/<provider>/login` and `/connect`). Sessions are
//! established with `umbral-sessions`.

pub mod models;

pub use models::{
    GitHubAccount, GitHubAccountStatus, TrustGateCheck, TrustGateKind, TrustGateStatus,
    WebsiteProfile,
};

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use umbral::migrate::ModelMeta;
use umbral::plugin::{AppContext, Plugin, PluginError};
use umbral::templates::context;
use umbral::web::{
    Form, HeaderMap, Html, IntoResponse, Query, Redirect, Response, Router, StatusCode, get, post,
};
use umbral_auth::{AuthUser, authenticate, create_user, current_session_user_id};
use umbral_oauth::models::{SocialAccount, social_account};
use umbral_sessions::{clear_cookie_header, cookie_from_headers, destroy_session, login_user_id};

#[derive(Debug, Default, Clone)]
pub struct AccountsPlugin;

impl Plugin for AccountsPlugin {
    fn name(&self) -> &'static str {
        "accounts"
    }

    /// `accounts_git_hub_account.user` is a FK into `auth_user`, so the auth
    /// plugin's migrations must run first.
    ///
    /// Without this the toposort in `App::build()` sees no dependencies, every
    /// plugin has in-degree 0, and Kahn's tie-break (a `BTreeSet`) resolves the
    /// order alphabetically — putting "accounts" before "auth". Against an
    /// existing database nothing happens, because `auth_user` is already there;
    /// against a FRESH one, `CREATE TABLE accounts_git_hub_account` dies with
    /// `relation "auth_user" does not exist`. That is exactly what happened on
    /// the first umbralrs.dev deploy.
    fn dependencies(&self) -> &'static [&'static str] {
        &["auth"]
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![
            ModelMeta::for_::<models::WebsiteProfile>(),
            ModelMeta::for_::<models::GitHubAccount>(),
            ModelMeta::for_::<models::TrustGateCheck>(),
        ]
    }

    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }

    fn routes(&self) -> Router {
        Router::new()
            .route("/login", get(login_page).post(do_login))
            .route("/signup", get(signup_page).post(do_signup))
            .route("/account", get(account_page))
            .route("/logout", post(do_logout))
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

fn internal_error(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Only allow site-relative redirect targets (no open redirect): a path
/// starting with a single `/`. Falls back to `/dashboard`.
fn safe_next(raw: Option<&str>) -> String {
    match raw {
        Some(p) if p.starts_with('/') && !p.starts_with("//") => p.to_string(),
        _ => "/dashboard".to_string(),
    }
}

#[derive(Deserialize)]
struct AuthQuery {
    error: Option<String>,
    next: Option<String>,
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

async fn login_page(headers: HeaderMap, Query(q): Query<AuthQuery>) -> Response {
    // Already logged in → don't show an auth page; bounce to the app.
    if current_session_user_id(&headers).await.is_some() {
        return Redirect::to("/dashboard").into_response();
    }
    let providers = umbral_oauth::available_providers();
    match umbral::templates::render(
        "accounts/login.html",
        &context! {
            error => q.error.is_some(),
            next => safe_next(q.next.as_deref()),
            show_google => providers.contains(&"google"),
            show_github => providers.contains(&"github"),
            any_oauth => !providers.is_empty(),
        },
    ) {
        Ok(body) => Html(body).into_response(),
        Err(e) => internal_error(e).into_response(),
    }
}

async fn do_login(headers: HeaderMap, Form(form): Form<HashMap<String, String>>) -> Response {
    let username = form.get("username").map(String::as_str).unwrap_or("").trim();
    let password = form.get("password").map(String::as_str).unwrap_or("");
    let next = safe_next(form.get("next").map(String::as_str));

    match authenticate::<AuthUser>(username, password).await {
        Ok(user) => establish_session(&headers, user.id, &next).await,
        Err(_) => Redirect::to("/login?error=1").into_response(),
    }
}

// ---------------------------------------------------------------------------
// Signup
// ---------------------------------------------------------------------------

async fn signup_page(headers: HeaderMap, Query(q): Query<AuthQuery>) -> Response {
    if current_session_user_id(&headers).await.is_some() {
        return Redirect::to("/dashboard").into_response();
    }
    // Map the error CODE to a fixed message so nothing user-controlled is
    // reflected into the page.
    let message = match q.error.as_deref() {
        Some("taken") => Some("That username or email is already taken."),
        Some("invalid") => Some("Please fill every field (password at least 8 characters)."),
        _ => None,
    };
    let providers = umbral_oauth::available_providers();
    match umbral::templates::render(
        "accounts/signup.html",
        &context! {
            error => message,
            show_google => providers.contains(&"google"),
            show_github => providers.contains(&"github"),
            any_oauth => !providers.is_empty(),
        },
    ) {
        Ok(body) => Html(body).into_response(),
        Err(e) => internal_error(e).into_response(),
    }
}

async fn do_signup(headers: HeaderMap, Form(form): Form<HashMap<String, String>>) -> Response {
    let username = form.get("username").map(String::as_str).unwrap_or("").trim();
    let email = form.get("email").map(String::as_str).unwrap_or("").trim();
    let password = form.get("password").map(String::as_str).unwrap_or("");

    if username.len() < 2 || !email.contains('@') || password.len() < 8 {
        return Redirect::to("/signup?error=invalid").into_response();
    }

    match create_user(username, email, password).await {
        Ok(user) => establish_session(&headers, user.id, "/dashboard").await,
        Err(_) => Redirect::to("/signup?error=taken").into_response(),
    }
}

/// Log `user_id` in (rotates the session token, sets the cookie) and
/// redirect to `next`.
async fn establish_session(headers: &HeaderMap, user_id: i64, next: &str) -> Response {
    let mut response = Redirect::to(next).into_response();
    if login_user_id(headers, response.headers_mut(), Some(user_id.to_string()))
        .await
        .is_err()
    {
        return internal_error("could not establish session").into_response();
    }
    response
}

// ---------------------------------------------------------------------------
// Profile / account
// ---------------------------------------------------------------------------

/// Per-provider connection state for the profile page.
#[derive(Serialize)]
struct LinkVm {
    provider: String,
    label: String,
    email: String,
    connected: bool,
}

async fn account_page(headers: HeaderMap) -> Response {
    let Some(user_id) = current_session_user_id(&headers).await else {
        return Redirect::to("/login?next=/account").into_response();
    };

    let accounts = SocialAccount::objects()
        .filter(social_account::USER.eq(user_id))
        .fetch()
        .await
        .unwrap_or_default();

    // The providers offered on the page, in display order.
    let providers = [("google", "Google"), ("github", "GitHub")];
    let links: Vec<LinkVm> = providers
        .iter()
        .map(|(key, label)| {
            let account = accounts.iter().find(|a| a.provider == *key);
            LinkVm {
                provider: (*key).to_string(),
                label: (*label).to_string(),
                email: account
                    .and_then(|a| a.provider_email.clone())
                    .unwrap_or_default(),
                connected: account.is_some(),
            }
        })
        .collect();

    // `user` is injected into the template context globally by
    // AuthPlugin::with_user_in_templates(), so only `links` is passed.
    match umbral::templates::render("accounts/account.html", &context! { links => links }) {
        Ok(body) => Html(body).into_response(),
        Err(e) => internal_error(e).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Logout
// ---------------------------------------------------------------------------

async fn do_logout(headers: HeaderMap) -> Response {
    if let Some(token) = cookie_from_headers(&headers) {
        let _ = destroy_session(&token).await;
    }
    let mut response = Redirect::to("/").into_response();
    if let Ok(value) = http::HeaderValue::from_str(&clear_cookie_header()) {
        response
            .headers_mut()
            .insert(http::header::SET_COOKIE, value);
    }
    response
}
