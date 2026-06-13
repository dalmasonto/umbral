//! The OAuth flow routes:
//!
//! - `GET  /oauth/{provider}/login`      — start a social login.
//! - `GET  /oauth/{provider}/connect`    — start a connect (auth required).
//! - `GET  /oauth/{provider}/callback`   — provider redirects back here.
//! - `POST /oauth/{provider}/disconnect` — unlink (auth required).
//!
//! CSRF is handled with a one-time `state` token persisted in the session
//! before the redirect and checked on the callback. Login establishes the
//! session via `umbra_sessions::login_user_id`; connect leaves the
//! existing session alone (the user is already that user).

use axum::Extension;
use axum::extract::{Path, Query};
use serde::{Deserialize, Serialize};
use umbra::templates::context;
use umbra::web::{Html, IntoResponse, Redirect, Response, Router, StatusCode, get, post};
use umbra_auth::current_session_user_id;
use umbra_sessions::{SessionToken, current_session, get_data, login_user_id, set_data};

use crate::OAuthPlugin;
use crate::models::{SocialAccount, social_account};
use crate::policy::resolve_user;

const FLOW_KEY: &str = "oauth_flow";

/// Render a server-error (500) response for an unrecoverable OAuth
/// failure — an unconfigured provider, a provider-communication error,
/// or a failed session write. These are *server-side* problems (the user
/// did nothing wrong), so they get the app's branded 500 page when one is
/// registered (`server_error_template`), falling back to plain text. The
/// underlying error is logged by the caller; the page never leaks it.
fn server_error(log_message: &str) -> Response {
    tracing::warn!("oauth: {log_message}");
    match umbra::templates::render("500.html", &context! {}) {
        Ok(body) => (StatusCode::INTERNAL_SERVER_ERROR, Html(body)).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong completing sign-in. Please try again later.",
        )
            .into_response(),
    }
}

/// The in-flight flow, stored in the session between the authorize
/// redirect and the callback.
#[derive(Serialize, Deserialize)]
struct FlowState {
    /// The CSRF token echoed back in the callback's `state`.
    state: String,
    /// The provider this flow is for (the callback must match).
    provider: String,
    /// `Some(user_id)` for a connect flow (attach to this user);
    /// `None` for a login flow (resolve / create a user).
    connect_user: Option<i64>,
}

/// Build the flow routes, with the plugin config attached as an
/// extension the handlers read.
pub(crate) fn router(plugin: OAuthPlugin) -> Router {
    Router::new()
        .route("/oauth/{provider}/login", get(oauth_login))
        .route("/oauth/{provider}/connect", get(oauth_connect))
        .route("/oauth/{provider}/callback", get(oauth_callback))
        .route("/oauth/{provider}/disconnect", post(oauth_disconnect))
        .layer(Extension(plugin))
}

/// Start a flow: persist a fresh `state` in the session, redirect to the
/// provider. `connect_user` distinguishes login from connect.
async fn begin_flow(
    plugin: &OAuthPlugin,
    token: &str,
    provider: &str,
    connect_user: Option<i64>,
) -> Response {
    let Some(p) = plugin.lookup(provider) else {
        return server_error(&format!(
            "provider `{provider}` is not configured on this server (no credentials set)"
        ));
    };
    let state = uuid::Uuid::new_v4().to_string();
    let flow = FlowState {
        state: state.clone(),
        provider: provider.to_string(),
        connect_user,
    };
    if let Err(e) = set_data(token, FLOW_KEY, &flow).await {
        return server_error(&format!("failed to store flow state: {e}"));
    }
    let url = p.authorize_url(&state, &plugin.redirect_uri(provider));
    Redirect::to(&url).into_response()
}

async fn oauth_login(
    Extension(plugin): Extension<OAuthPlugin>,
    Extension(SessionToken(token)): Extension<SessionToken>,
    Path(provider): Path<String>,
) -> Response {
    begin_flow(&plugin, &token, &provider, None).await
}

async fn oauth_connect(
    Extension(plugin): Extension<OAuthPlugin>,
    Extension(SessionToken(token)): Extension<SessionToken>,
    Path(provider): Path<String>,
    headers: umbra::web::HeaderMap,
) -> Response {
    let Some(user_id) = current_session_user_id(&headers).await else {
        return (
            StatusCode::UNAUTHORIZED,
            "log in before connecting an account",
        )
            .into_response();
    };
    begin_flow(&plugin, &token, &provider, Some(user_id)).await
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn oauth_callback(
    Extension(plugin): Extension<OAuthPlugin>,
    Path(provider): Path<String>,
    Query(query): Query<CallbackQuery>,
    headers: umbra::web::HeaderMap,
) -> Response {
    let Some(p) = plugin.lookup(&provider) else {
        return server_error(&format!(
            "provider `{provider}` is not configured on this server"
        ));
    };

    // The user denied consent (or the provider errored).
    if let Some(err) = query.error {
        tracing::info!("oauth: provider `{provider}` returned error: {err}");
        return Redirect::to(&plugin.login_redirect).into_response();
    }

    // Validate `state` against the value stored in the session — this is
    // the CSRF defense. A missing / mismatched state is rejected.
    let session = current_session(&headers).await.ok().flatten();
    let flow: Option<FlowState> = session
        .as_ref()
        .and_then(|s| get_data(s, FLOW_KEY).ok().flatten());
    let Some(flow) = flow else {
        return (StatusCode::BAD_REQUEST, "no oauth flow in progress").into_response();
    };
    if Some(&flow.state) != query.state.as_ref() || flow.provider != provider {
        return (StatusCode::BAD_REQUEST, "oauth state mismatch").into_response();
    }
    let Some(code) = query.code else {
        return (StatusCode::BAD_REQUEST, "missing authorization code").into_response();
    };

    // Exchange the code and resolve the identity.
    let redirect_uri = plugin.redirect_uri(&provider);
    let tokens = match p.exchange_code(&code, &redirect_uri).await {
        Ok(t) => t,
        Err(e) => return server_error(&format!("token exchange failed for `{provider}`: {e}")),
    };
    let identity = match p.fetch_identity(&tokens).await {
        Ok(i) => i,
        Err(e) => return server_error(&format!("identity fetch failed for `{provider}`: {e}")),
    };

    // Apply the create-or-link policy.
    let user_id = match resolve_user(&provider, &identity, &tokens, flow.connect_user).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("oauth: resolve_user failed for `{provider}`: {e}");
            return (StatusCode::CONFLICT, e.to_string()).into_response();
        }
    };

    let mut response = Redirect::to(&plugin.login_redirect).into_response();
    // Connect flow: the user is already logged in as themselves — leave
    // their session alone. Login flow: establish the session now.
    if flow.connect_user.is_none() {
        if let Err(e) =
            login_user_id(&headers, response.headers_mut(), Some(user_id.to_string())).await
        {
            return server_error(&format!("failed to establish session: {e}"));
        }
    }
    response
}

async fn oauth_disconnect(
    Path(provider): Path<String>,
    headers: umbra::web::HeaderMap,
) -> Response {
    let Some(user_id) = current_session_user_id(&headers).await else {
        return (StatusCode::UNAUTHORIZED, "not logged in").into_response();
    };
    match SocialAccount::objects()
        .filter(social_account::USER.eq(user_id))
        .filter(social_account::PROVIDER.eq(&provider))
        .delete()
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::warn!("oauth: disconnect failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "disconnect failed").into_response()
        }
    }
}
