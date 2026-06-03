//! `AuthUser`-aware session helpers — moved from umbra-sessions so
//! sessions can stay free of any user-model dependency.
//!
//! The split mirrors the dep arrow: `umbra-auth` depends on
//! `umbra-sessions` (it needs cookie + session-table primitives),
//! `umbra-sessions` does not depend on `umbra-auth` (it knows
//! nothing about users). All the AuthUser hydration happens here.
//!
//! ## What this module owns
//!
//! - [`current_user`] — read the cookie, hydrate the [`AuthUser`].
//! - [`login`] / [`login_with_request`] — the one-call shape
//!   for credential check + session creation + cookie set +
//!   `last_login` bump.
//! - [`logout`] — re-exported convenience; same as
//!   `umbra_sessions::logout` plus a forwarding doc-comment.
//! - [`SessionAuthentication`] — the `umbra-rest` `Authentication`
//!   impl that produces an `Identity` for the permission layer
//!   (was in `umbra-sessions`; needs AuthUser to populate
//!   `is_staff`).
//! - [`User`] / [`OptionalUser`] — axum extractors that pull
//!   `AuthUser` from the request.
//! - [`user_context_layer`] — middleware that injects the current
//!   user into `umbra::templates::CURRENT_USER` so HTML templates
//!   can write `{% if user.is_authenticated %}` uniformly.
//!
//! ## Custom user models
//!
//! Everything in here is hard-bound to [`AuthUser`]. Apps using a
//! custom [`UserModel`] roll their own helpers — the building
//! blocks are all `pub`:
//!
//! - `umbra_sessions::current_user_id_str(&headers)` → user PK as
//!   a string (already user-agnostic).
//! - Their own user lookup against that PK.
//! - Their own `Identity` builder.
//!
//! [`UserModel`]: crate::UserModel

use crate::{AuthUser, auth_user};
use async_trait::async_trait;
use axum_core::extract::FromRequestParts;
use http::StatusCode;
use http::request::Parts;
use umbra::web::HeaderMap;
use umbra_rest::{Authentication, Identity};
use umbra_sessions::SessionError;

// =========================================================================
// current_user — the AuthUser-flavored wrapper around
// umbra_sessions::current_session.
// =========================================================================

/// Read the request's session cookie, look up the session row, then
/// hydrate the [`AuthUser`] it points at. Returns `None` for any
/// of: no cookie, expired session, anonymous session
/// (`user_id IS NULL`), parse failure on a non-i64 user_id, missing
/// user row, or inactive user.
///
/// One DB read (session row) + one DB read (user row). The
/// `is_active` predicate is part of the user query, so a deactivated
/// account silently looks anonymous from this helper's perspective
/// without an explicit second filter at the call site.
pub async fn current_user(headers: &HeaderMap) -> Result<Option<AuthUser>, SessionError> {
    let Some(user_id_str) = umbra_sessions::current_user_id_str(headers).await? else {
        return Ok(None);
    };
    // Session.user_id is text (gap #59) — parse back to AuthUser's
    // i64 PK. A non-parseable value means the session was written
    // by a different UserModel impl; from AuthUser's perspective
    // that's anonymous.
    let Ok(user_id) = user_id_str.parse::<i64>() else {
        return Ok(None);
    };
    let user: Option<AuthUser> = AuthUser::objects()
        .filter(auth_user::ID.eq(user_id) & auth_user::IS_ACTIVE.eq(true))
        .first()
        .await?;
    Ok(user)
}

// =========================================================================
// login / login_with_request — credential check ran outside, we just
// mint the session + cookie + bump last_login.
// =========================================================================

/// Convenience: [`login_with_request`] with an empty request
/// HeaderMap. Use when the handler doesn't already have a
/// `HeaderMap` extractor and you're not worried about preserving
/// an anonymous session's `data` (flash messages, cart) across the
/// login.
pub async fn login(
    response_headers: &mut HeaderMap,
    user: &AuthUser,
) -> Result<String, SessionError> {
    login_with_request(&HeaderMap::new(), response_headers, user).await
}

/// Mint an authenticated session for `user`, rotate the cookie, and
/// bump `auth_user.last_login`. The session-fixation defense fires
/// inside `umbra_sessions::login_user_id`: any anonymous session
/// the request carried is destroyed before the new authenticated
/// row is written.
///
/// `last_login` is a best-effort update: a failure logs a warning
/// but doesn't invalidate the login (the session was created and
/// the cookie was set, so the user is in).
pub async fn login_with_request(
    request_headers: &HeaderMap,
    response_headers: &mut HeaderMap,
    user: &AuthUser,
) -> Result<String, SessionError> {
    let token = umbra_sessions::login_user_id(
        request_headers,
        response_headers,
        Some(user.id.to_string()),
    )
    .await?;

    let mut patch = serde_json::Map::new();
    patch.insert(
        "last_login".to_string(),
        serde_json::to_value(chrono::Utc::now()).unwrap_or(serde_json::Value::Null),
    );
    if let Err(e) = AuthUser::objects()
        .filter(auth_user::ID.eq(user.id))
        .update_values(patch)
        .await
    {
        tracing::warn!(
            error = ?e,
            user_id = user.id,
            "umbra-auth::login: failed to update last_login (session still active)",
        );
    }
    Ok(token)
}

// =========================================================================
// SessionAuthentication — produce an `Identity` for the REST
// permission layer.
// =========================================================================

/// The session-cookie authenticator for `umbra-rest`. Reads the
/// cookie, hydrates the [`AuthUser`], turns it into an [`Identity`]
/// with `is_staff` set. Same shape `current_user` produces, packaged
/// for `RestPlugin::authenticate`.
///
/// Was in `umbra-sessions` before the de-coupling; now here so it
/// can name `AuthUser`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SessionAuthentication;

impl SessionAuthentication {
    /// Convenience constructor identical to `Default::default()`.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Authentication for SessionAuthentication {
    async fn authenticate(&self, headers: &HeaderMap) -> Option<Identity> {
        let user = current_user(headers).await.ok().flatten()?;
        Some(
            Identity::user(user.id)
                .with_staff(user.is_staff)
                .with_extra("auth", serde_json::json!("session")),
        )
    }
}

// =========================================================================
// User / OptionalUser axum extractors. Same shapes that used to live
// in umbra-sessions::extractors.
// =========================================================================

/// Required-user extractor. 401 on anonymous requests.
///
/// ```ignore
/// async fn dashboard(User(user): User) -> Html<String> {
///     Html(format!("Welcome, {}!", user.username))
/// }
/// ```
#[derive(Debug, Clone)]
pub struct User(pub AuthUser);

/// Optional-user extractor. Anonymous requests get `None`.
///
/// ```ignore
/// async fn home(OptionalUser(maybe): OptionalUser) -> Html<String> {
///     match maybe {
///         Some(u) => Html(format!("Hi, {}", u.username)),
///         None    => Html("<a href=\"/login\">Log in</a>".into()),
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct OptionalUser(pub Option<AuthUser>);

impl<S> FromRequestParts<S> for User
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        match current_user(&parts.headers).await.ok().flatten() {
            Some(u) => Ok(User(u)),
            None => Err((StatusCode::UNAUTHORIZED, "authentication required")),
        }
    }
}

impl<S> FromRequestParts<S> for OptionalUser
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(OptionalUser(
            current_user(&parts.headers).await.ok().flatten(),
        ))
    }
}

// =========================================================================
// Template-injection middleware. Stash the current user under the
// `umbra::templates::CURRENT_USER` task-local so HTML renders can
// pick it up as `{{ user }}`.
// =========================================================================

/// Resolve [`current_user`] and stash it in
/// [`umbra::templates::CURRENT_USER`] for the request's duration.
/// Always injects a value:
///
/// - Authenticated → the serialized user merged with
///   `is_authenticated: true`.
/// - Anonymous → the sentinel `{ is_authenticated: false }`.
///
/// One DB read per request (cookie + session + user) on top of
/// whatever else the handler does. Opt in via
/// [`crate::AuthPlugin::with_user_in_templates`] when the app is
/// HTML-heavy; leave off for REST-only services.
pub async fn user_context_layer(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let user_value = match current_user(req.headers()).await {
        Ok(Some(u)) => serialize_authenticated(&u),
        _ => anonymous_user_value(),
    };
    umbra::templates::with_current_user(Some(user_value), next.run(req)).await
}

fn serialize_authenticated(user: &AuthUser) -> umbra::templates::Value {
    let mut json = match serde_json::to_value(user) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    json.insert(
        "is_authenticated".to_string(),
        serde_json::Value::Bool(true),
    );
    umbra::templates::Value::from_serialize(serde_json::Value::Object(json))
}

fn anonymous_user_value() -> umbra::templates::Value {
    let mut json = serde_json::Map::new();
    json.insert(
        "is_authenticated".to_string(),
        serde_json::Value::Bool(false),
    );
    umbra::templates::Value::from_serialize(serde_json::Value::Object(json))
}

// =========================================================================
// logout — pure re-export. Sessions still owns the call shape (no
// user model involved), but we re-export from umbra-auth too so the
// import surface stays uniform across login + logout.
// =========================================================================

/// Pure forwarding alias for [`umbra_sessions::logout`]. Re-exported
/// so handlers that import `umbra_auth::login` also reach for
/// `umbra_auth::logout` without flipping crates. Sessions still owns
/// the implementation; it's user-agnostic.
pub use umbra_sessions::logout;
