//! The OAuth flow routes:
//!
//! - `GET  /oauth/{provider}/login`      — start a social login.
//! - `GET  /oauth/{provider}/connect`    — start a connect (auth required).
//! - `GET  /oauth/{provider}/callback`   — provider redirects back here.
//! - `POST /oauth/{provider}/disconnect` — unlink (auth required).
//!
//! CSRF is handled with a one-time `state` token persisted in the session
//! before the redirect and checked on the callback. Login establishes the
//! session via `umbral_sessions::login_user_id`; connect leaves the
//! existing session alone (the user is already that user).

use axum::Extension;
use axum::extract::{Path, Query};
use serde::{Deserialize, Serialize};
use serde_json::json;
use umbral::templates::context;
use umbral::web::{Html, IntoResponse, Json, Redirect, Response, Router, StatusCode, get, post};
use umbral_auth::{AuthToken, AuthUser, auth_user, current_session_user_id};
use umbral_sessions::{
    SessionToken, cookie_from_headers, current_session, get_data, login_user_id, set_data,
};

use crate::OAuthPlugin;
use crate::models::{SocialAccount, social_account};
use crate::policy::resolve_user;
use crate::provider::OAuthError;

const FLOW_KEY: &str = "oauth_flow";

/// Render a server-error (500) response for an unrecoverable OAuth
/// failure — an unconfigured provider, a provider-communication error,
/// or a failed session write. These are *server-side* problems (the user
/// did nothing wrong), so they get the app's branded 500 page when one is
/// registered (`server_error_template`), falling back to plain text. The
/// underlying error is logged by the caller; the page never leaks it.
fn server_error(log_message: &str) -> Response {
    tracing::warn!("oauth: {log_message}");
    match umbral::templates::render("500.html", &context! {}) {
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
    /// `Some(url)` when the flow was started by a SPA with an
    /// allowlisted `?next=`. The callback redirects here instead of
    /// `login_redirect`, and (for a login flow) appends a bearer token
    /// in the URL fragment. `#[serde(default)]` so flows already in the
    /// session from before this field existed still deserialize.
    #[serde(default)]
    return_to: Option<String>,
    /// The PKCE `code_verifier` (RFC 7636) minted with this flow. The
    /// authorize redirect carries only its hash; this secret is replayed
    /// on the token exchange to prove the redeemer is the client that
    /// began the flow, so an intercepted `code` can't be redeemed alone.
    /// `#[serde(default)]` so a flow persisted before PKCE landed still
    /// deserializes (it exchanges with an empty verifier, as it did before).
    #[serde(default)]
    code_verifier: String,
}

/// `?next=<url>` on a login/connect start — the SPA return URL.
#[derive(Deserialize)]
struct StartQuery {
    next: Option<String>,
}

/// Build the flow routes, with the plugin config attached as an
/// extension the handlers read.
pub(crate) fn router(plugin: OAuthPlugin) -> Router {
    Router::new()
        .route("/oauth/providers", get(oauth_providers))
        .route("/oauth/{provider}/login", get(oauth_login))
        .route("/oauth/{provider}/connect", get(oauth_connect))
        .route("/oauth/{provider}/callback", get(oauth_callback))
        .route("/oauth/{provider}/disconnect", post(oauth_disconnect))
        .layer(Extension(plugin))
}

/// Service-discovery: the configured providers and their flow URLs,
/// auto-built from the registered providers. Public — lists provider
/// names only, no secrets. A SPA fetches this to render login buttons
/// and learn the URLs to navigate to.
async fn oauth_providers(Extension(plugin): Extension<OAuthPlugin>) -> Response {
    let providers: Vec<_> = plugin
        .provider_links()
        .into_iter()
        .map(|l| {
            json!({
                "key": l.key,
                "label": l.label,
                "login":    { "path": l.login,    "url": plugin.absolute(&l.login) },
                "connect":  { "path": l.connect,  "url": plugin.absolute(&l.connect) },
                "callback": { "path": l.callback, "url": plugin.absolute(&l.callback) },
            })
        })
        .collect();
    Json(json!({ "providers": providers })).into_response()
}

/// Validate a `?next=` against the plugin's return-URL allowlist.
/// `None` (no `next`) passes as `None`. A present-but-disallowed `next`
/// is rejected with `400` — never a silent fallback, so an attacker
/// can't redirect a minted token to an arbitrary origin.
fn validate_next(
    plugin: &OAuthPlugin,
    next: Option<String>,
) -> Result<Option<String>, Box<Response>> {
    match next {
        None => Ok(None),
        Some(url) if plugin.is_allowed_return(&url) => Ok(Some(url)),
        Some(_) => Err(Box::new(
            (StatusCode::BAD_REQUEST, "next is not an allowed return URL").into_response(),
        )),
    }
}

/// Map a `resolve_user` failure to a **client-safe** `(status, message)`.
///
/// The message is always a fixed string — it NEVER includes the error's own
/// text. An `OAuthError::Database` renders the raw DB error (table / column /
/// constraint names) via `Display`; echoing that into the response body leaks
/// internal schema detail to any client that can trigger the failure (OAU-1).
/// So a database (or otherwise internal) failure returns a generic 500, and the
/// only client-facing case is the genuine link conflict, which already carries a
/// fixed, non-sensitive message. The caller logs the full detail server-side.
fn resolve_user_client_error(e: &OAuthError) -> (StatusCode, &'static str) {
    match e {
        // A genuine account-link conflict (e.g. the identity is already linked
        // to a different user). The message is a fixed, safe string.
        OAuthError::Provider(_) => (
            StatusCode::CONFLICT,
            "this account is already linked to a different user",
        ),
        // Anything else — notably `Database` — is an internal fault. Return a
        // generic server error; the detail stays in the logs.
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Something went wrong completing sign-in. Please try again later.",
        ),
    }
}

/// Start a flow: persist a fresh `state` in the session, redirect to the
/// provider. `connect_user` distinguishes login from connect.
async fn begin_flow(
    plugin: &OAuthPlugin,
    token: &str,
    provider: &str,
    connect_user: Option<i64>,
    return_to: Option<String>,
) -> Response {
    let Some(p) = plugin.lookup(provider) else {
        // Unknown / unconfigured provider key is a client error (a foreseeable
        // input: a misspelled or unconfigured provider), NOT a server fault.
        return (
            StatusCode::NOT_FOUND,
            format!("unknown or unconfigured oauth provider `{provider}`"),
        )
            .into_response();
    };
    let state = uuid::Uuid::new_v4().to_string();
    // PKCE (RFC 7636): mint a secret verifier, persist it with the flow,
    // and send only its hash on the redirect.
    let code_verifier = crate::pkce::generate_verifier();
    let code_challenge = crate::pkce::challenge_s256(&code_verifier);
    let flow = FlowState {
        state: state.clone(),
        provider: provider.to_string(),
        connect_user,
        return_to,
        code_verifier,
    };
    if let Err(e) = set_data(token, FLOW_KEY, &flow).await {
        return server_error(&format!("failed to store flow state: {e}"));
    }
    let url = p.authorize_url(&state, &plugin.redirect_uri(provider), &code_challenge);
    Redirect::to(&url).into_response()
}

async fn oauth_login(
    Extension(plugin): Extension<OAuthPlugin>,
    Extension(SessionToken(token)): Extension<SessionToken>,
    Path(provider): Path<String>,
    Query(q): Query<StartQuery>,
) -> Response {
    let return_to = match validate_next(&plugin, q.next) {
        Ok(rt) => rt,
        Err(resp) => return *resp,
    };
    begin_flow(&plugin, &token, &provider, None, return_to).await
}

async fn oauth_connect(
    Extension(plugin): Extension<OAuthPlugin>,
    Extension(SessionToken(token)): Extension<SessionToken>,
    Path(provider): Path<String>,
    Query(q): Query<StartQuery>,
    headers: umbral::web::HeaderMap,
) -> Response {
    let Some(user_id) = current_session_user_id(&headers).await else {
        return (
            StatusCode::UNAUTHORIZED,
            "log in before connecting an account",
        )
            .into_response();
    };
    let return_to = match validate_next(&plugin, q.next) {
        Ok(rt) => rt,
        Err(resp) => return *resp,
    };
    begin_flow(&plugin, &token, &provider, Some(user_id), return_to).await
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
    headers: umbral::web::HeaderMap,
) -> Response {
    let Some(p) = plugin.lookup(&provider) else {
        return (
            StatusCode::NOT_FOUND,
            format!("unknown or unconfigured oauth provider `{provider}`"),
        )
            .into_response();
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
    let state_ok = query
        .state
        .as_deref()
        .map(|s| ct_eq(&flow.state, s))
        .unwrap_or(false);
    if !state_ok || flow.provider != provider {
        return (StatusCode::BAD_REQUEST, "oauth state mismatch").into_response();
    }
    let Some(raw_session_token) = cookie_from_headers(&headers) else {
        return (StatusCode::BAD_REQUEST, "missing oauth session").into_response();
    };
    if let Err(e) = set_data(&raw_session_token, FLOW_KEY, &serde_json::Value::Null).await {
        return server_error(&format!("failed to consume oauth flow state: {e}"));
    }
    let Some(code) = query.code else {
        return (StatusCode::BAD_REQUEST, "missing authorization code").into_response();
    };

    // Exchange the code and resolve the identity.
    let redirect_uri = plugin.redirect_uri(&provider);
    let tokens = match p
        .exchange_code(&code, &redirect_uri, &flow.code_verifier)
        .await
    {
        Ok(t) => t,
        Err(e) => return server_error(&format!("token exchange failed for `{provider}`: {e}")),
    };
    let identity = match p.fetch_identity(&tokens).await {
        Ok(i) => i,
        Err(e) => return server_error(&format!("identity fetch failed for `{provider}`: {e}")),
    };

    // Apply the create-or-link policy.
    let user_id = match resolve_user(
        &provider,
        &identity,
        &tokens,
        flow.connect_user,
        p.trusts_verified_email(),
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            // Log the full detail server-side; never echo it to the client.
            tracing::warn!("oauth: resolve_user failed for `{provider}`: {e}");
            let (status, msg) = resolve_user_client_error(&e);
            return (status, msg).into_response();
        }
    };

    // Where the browser lands: a SPA's allowlisted `return_to`, else
    // the configured `login_redirect`.
    let mut target = flow
        .return_to
        .clone()
        .unwrap_or_else(|| plugin.login_redirect.clone());

    // SPA token mode: a login flow with a `return_to` mints a bearer
    // token and hands it back in the URL fragment, so a separate-origin
    // SPA can authenticate the REST API. Connect flows never mint a
    // token (the user already holds one).
    if flow.connect_user.is_none() && flow.return_to.is_some() {
        match mint_login_token(user_id).await {
            Ok(plaintext) => {
                let sep = if target.contains('#') { '&' } else { '#' };
                target = format!("{target}{sep}token={plaintext}&token_type=Bearer");
            }
            Err(e) => return server_error(&format!("failed to mint login token: {e}")),
        }
    }

    let mut response = Redirect::to(&target).into_response();
    // Connect flow: the user is already logged in as themselves — leave
    // their session alone. Login flow: establish the session now (still
    // useful for a same-origin client; harmless for a token-mode SPA).
    if flow.connect_user.is_none() {
        if let Err(e) =
            login_user_id(&headers, response.headers_mut(), Some(user_id.to_string())).await
        {
            return server_error(&format!("failed to establish session: {e}"));
        }
    }
    response
}

/// Mint a fresh bearer token for a just-resolved user. `resolve_user`
/// returns the id; `AuthToken::create_for` needs the row, so we load it
/// back (it was just written/looked-up, so this is a cheap point read).
async fn mint_login_token(user_id: i64) -> Result<String, String> {
    let user = AuthUser::objects()
        .filter(auth_user::ID.eq(user_id))
        .first()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("resolved user {user_id} not found"))?;
    let (_row, plaintext) = AuthToken::create_for(&user, "oauth")
        .await
        .map_err(|e| e.to_string())?;
    Ok(plaintext.0)
}

async fn oauth_disconnect(
    Path(provider): Path<String>,
    headers: umbral::web::HeaderMap,
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

/// Constant-time string equality for the OAuth `state` CSRF check.
/// `state` is a server-minted UUID, so a timing oracle is impractical,
/// but a constant-time compare matches the rest of the framework's
/// posture (sessions, CSRF tokens). Length is compared directly — it
/// isn't secret — then every byte is mixed before the verdict.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::{OAuthError, StatusCode, ct_eq, resolve_user_client_error};

    #[test]
    fn database_error_never_leaks_detail_to_client() {
        // OAU-1: a DB error's raw text (constraint / table / column detail) must
        // NOT reach the response body. It maps to a generic 500 whose message is
        // fixed and contains none of the error's own string.
        let secret = "duplicate key value violates unique constraint \"social_account_secret_idx\"";
        let err = OAuthError::Database(secret.to_string());
        let (status, msg) = resolve_user_client_error(&err);
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            !msg.contains("constraint") && !msg.contains("social_account") && !msg.contains(secret),
            "client message must not echo internal DB detail: {msg}"
        );
    }

    #[test]
    fn link_conflict_is_a_fixed_safe_message() {
        // A genuine link conflict is the one client-facing case — CONFLICT with a
        // fixed, non-sensitive message (still no raw error text).
        let err = OAuthError::Provider("this account is already linked to a different user".into());
        let (status, msg) = resolve_user_client_error(&err);
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(msg, "this account is already linked to a different user");
    }

    #[test]
    fn ct_eq_matches_only_identical_strings() {
        assert!(ct_eq("abc123", "abc123"));
        assert!(ct_eq("", ""));
        assert!(!ct_eq("abc123", "abc124"));
        assert!(!ct_eq("abc", "abcd")); // length mismatch
        assert!(!ct_eq("abcd", "abc"));
        // A real UUID-shaped state round-trips.
        let state = "550e8400-e29b-41d4-a716-446655440000";
        assert!(ct_eq(state, state));
        assert!(!ct_eq(state, "550e8400-e29b-41d4-a716-446655440001"));
    }
}
