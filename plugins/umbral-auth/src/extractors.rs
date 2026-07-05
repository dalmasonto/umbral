//! Axum extractors that resolve a request to an
//! `umbral::auth::Identity`.
//!
//! Companion to the built-in [`crate::SessionAuthentication`] and
//! [`crate::BearerAuthentication`] classes — those run inside
//! `RestPlugin`'s CRUD handlers and stash the result for the
//! permission layer. Custom (non-CRUD) handlers don't go through
//! that pipeline; these extractors let them get at the same
//! `Identity` shape with one line in the handler signature.
//!
//! ```ignore
//! use umbral::web::Json;
//! use umbral_auth::OptionalIdentity;
//!
//! async fn me(OptionalIdentity(id): OptionalIdentity) -> Json<Value> {
//!     Json(json!({ "authenticated": id.is_some() }))
//! }
//! ```
//!
//! ## How they resolve
//!
//! Both extractors run the same chain `SessionAuthentication` runs
//! first, then `BearerAuthentication` — the same order
//! `ChainAuthentication([Session, Bearer])` would. If a handler
//! needs a different order, write a custom extractor instead;
//! the two built-ins are the common case.
//!
//! ## Custom user models
//!
//! These extractors assume `AuthUser` for the is_staff lookup (the
//! bearer path joins `auth_token` → `auth_user`; the session path
//! reads `session.user_id` and joins `auth_user`). Apps using a
//! custom `UserModel` should write their own extractor that joins
//! their user table instead.

use crate::bearer_auth::parse_bearer_header;
use crate::login_required::current_session_user_id;
use crate::token::AuthToken;
use crate::{auth_user, AuthUser};
use axum_core::extract::FromRequestParts;
use axum_core::response::{IntoResponse, Response};
use http::request::Parts;
use http::{HeaderMap, StatusCode};
use umbral::auth::Identity;

/// `OptionalIdentity(Option<Identity>)` — never rejects. Returns
/// the identity if either the session cookie or the bearer token
/// resolves to an active user; otherwise `None`.
///
/// Use when the handler can do something useful for anonymous
/// callers (a `/me` endpoint that returns `{authenticated: false}`,
/// a homepage that shows different links when logged in, an audit
/// log that records the actor when known but doesn't gate on it).
pub struct OptionalIdentity(pub Option<Identity>);

impl<S> FromRequestParts<S> for OptionalIdentity
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(resolve_identity(&parts.headers).await))
    }
}

/// `CurrentIdentity(Identity)` — rejects with 401 if neither
/// authentication path resolves. Use when the handler genuinely
/// needs an authenticated caller and an anonymous request is an
/// error.
///
/// The 401 body matches the JSON shape `umbral-rest` returns for
/// `Permission::AuthenticationRequired` so a single client error
/// handler can deal with both surfaces uniformly.
pub struct CurrentIdentity(pub Identity);

impl<S> FromRequestParts<S> for CurrentIdentity
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        match resolve_identity(&parts.headers).await {
            Some(id) => Ok(Self(id)),
            None => Err((
                StatusCode::UNAUTHORIZED,
                axum_core::body::Body::from(
                    r#"{"error":"authentication required","code":"unauthenticated"}"#,
                ),
            )
                .into_response()),
        }
    }
}

/// Run the session-then-bearer chain. Public so handlers that need
/// the resolution logic without the extractor framing can call it
/// directly (`resolve_identity(&headers).await`).
///
/// Session takes precedence because the cookie path is cheaper —
/// one indexed SELECT against the session table joined to
/// auth_user. Bearer needs a separate token table lookup plus the
/// user join.
pub async fn resolve_identity(headers: &HeaderMap) -> Option<Identity> {
    if let Some(id) = identity_from_session(headers).await {
        return Some(id);
    }
    identity_from_bearer(headers).await
}

async fn identity_from_session(headers: &HeaderMap) -> Option<Identity> {
    let user_id = current_session_user_id(headers).await?;
    let user: AuthUser = AuthUser::objects()
        .filter(auth_user::ID.eq(user_id) & auth_user::IS_ACTIVE.eq(true))
        .first()
        .await
        .ok()
        .flatten()?;
    Some(
        Identity::user(crate::UserModel::id_string(&user))
            .with_staff(user.is_staff)
            .with_superuser(user.is_superuser)
            .with_extra("auth", serde_json::json!("session")),
    )
}

async fn identity_from_bearer(headers: &HeaderMap) -> Option<Identity> {
    let plaintext = parse_bearer_header(headers)?;
    let token = AuthToken::lookup(plaintext).await.ok().flatten()?;
    let user: AuthUser = AuthUser::objects()
        .filter(auth_user::ID.eq(token.user_id.id()) & auth_user::IS_ACTIVE.eq(true))
        .first()
        .await
        .ok()
        .flatten()?;
    token.touch_last_used().await;
    Some(
        Identity::user(crate::UserModel::id_string(&user))
            .with_staff(user.is_staff)
            .with_superuser(user.is_superuser)
            .with_extra("auth", serde_json::json!("bearer")),
    )
}

/// Why a [`RequireStaff`] extraction was rejected. Split out so the gate rules
/// are unit-testable without wiring a DB session.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StaffReject {
    /// No authenticated identity on the request.
    Unauthenticated,
    /// Authenticated, but not a staff user.
    NotStaff,
    /// Staff, but `user_id` doesn't parse into the requested pk type.
    BadPk,
}

/// The staff-gate decision: require a present, staff identity whose pk parses
/// into `T`. Pure + synchronous so the rules are testable without a session; the
/// async [`RequireStaff`] extractor resolves the identity, then calls this.
pub(crate) fn require_staff_decision<T: std::str::FromStr>(
    identity: Option<&Identity>,
) -> Result<T, StaffReject> {
    let id = identity.ok_or(StaffReject::Unauthenticated)?;
    if !id.is_staff {
        return Err(StaffReject::NotStaff);
    }
    id.user_pk::<T>().map_err(|_| StaffReject::BadPk)
}

/// Extractor requiring an authenticated STAFF user; yields their primary key
/// parsed into `T` (default `i64`). Replaces the `require_staff(&identity)`
/// helper consumers copy-paste across plugins:
///
/// ```ignore
/// async fn organizers_only(RequireStaff(uid): RequireStaff) -> impl IntoResponse {
///     // `uid: i64` — the authenticated staff user's id, already typed.
/// }
/// ```
///
/// Rejections: `401` (not authenticated), `403` (authenticated but not staff),
/// `400` (pk doesn't parse into `T`).
pub struct RequireStaff<T = i64>(pub T);

impl<T, S> FromRequestParts<S> for RequireStaff<T>
where
    T: std::str::FromStr + Send,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let identity = resolve_identity(&parts.headers).await;
        match require_staff_decision::<T>(identity.as_ref()) {
            Ok(pk) => Ok(Self(pk)),
            Err(StaffReject::Unauthenticated) => Err((
                StatusCode::UNAUTHORIZED,
                axum_core::body::Body::from(
                    r#"{"error":"authentication required","code":"unauthenticated"}"#,
                ),
            )
                .into_response()),
            Err(StaffReject::NotStaff) => Err((
                StatusCode::FORBIDDEN,
                axum_core::body::Body::from(
                    r#"{"error":"staff access required","code":"forbidden"}"#,
                ),
            )
                .into_response()),
            Err(StaffReject::BadPk) => Err((
                StatusCode::BAD_REQUEST,
                axum_core::body::Body::from(r#"{"error":"invalid user id","code":"bad_identity"}"#),
            )
                .into_response()),
        }
    }
}

#[cfg(test)]
mod require_staff_tests {
    use super::{require_staff_decision, StaffReject};
    use umbral::auth::Identity;

    #[test]
    fn gates_unauthenticated_staff_and_typed_pk() {
        // No identity → unauthenticated.
        assert!(matches!(
            require_staff_decision::<i64>(None),
            Err(StaffReject::Unauthenticated)
        ));
        // Authenticated non-staff → forbidden.
        let member = Identity::user(5);
        assert!(matches!(
            require_staff_decision::<i64>(Some(&member)),
            Err(StaffReject::NotStaff)
        ));
        // Staff → returns the pk parsed into the requested type.
        let staff = Identity::user(7).with_staff(true);
        assert_eq!(require_staff_decision::<i64>(Some(&staff)).unwrap(), 7);
        // Staff, but the pk can't parse into the requested type → bad identity.
        let named = Identity::user("codename").with_staff(true);
        assert!(matches!(
            require_staff_decision::<i64>(Some(&named)),
            Err(StaffReject::BadPk)
        ));
        // Non-i64 keys work through the same path.
        let named_ok = Identity::user("codename").with_staff(true);
        assert_eq!(
            require_staff_decision::<String>(Some(&named_ok)).unwrap(),
            "codename"
        );
    }
}
