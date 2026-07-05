//! `permission_required(perm)`: the perm-gate axum layer.
//!
//! Where [`LoginRequiredLayer`](umbral_auth::LoginRequiredLayer) gates a
//! Router subtree on "is the user logged in?", this layer adds a second
//! check on "does the logged-in user hold permission `perm`?". Failure
//! at the auth step returns 401 / 302 (mirroring `login_required`); a
//! logged-in user who lacks the permission gets a 403 Forbidden.
//!
//! ## Composition
//!
//! `permission_required` does the auth check itself, so it can be used
//! standalone:
//!
//! ```ignore
//! use umbral_permissions::permission_required;
//!
//! let publish_routes = Router::new()
//!     .route("/publish/{id}", post(publish_handler))
//!     .layer(permission_required("blog.publish_post"));
//! ```
//!
//! It composes cleanly with `login_required_html` for HTML flows:
//!
//! ```ignore
//! use umbral_auth::login_required_html;
//! use umbral_permissions::permission_required_html;
//!
//! // Inner layer fires first (auth → perm).  Either misstep produces
//! // the right user-facing redirect / status.
//! Router::new()
//!     .route("/admin/blog/publish/{id}", post(publish_handler))
//!     .layer(permission_required_html("blog.publish_post", "/login"))
//! ```
//!
//! ## Why this lives in umbral-permissions, not umbral-auth
//!
//! Dependency direction. `permission_required` needs to call
//! `has_perm`, which is owned by this crate. If the layer lived in
//! `umbral-auth`, it would either have to depend on `umbral-permissions`
//! (creating a cycle with the dev-deps tests already rely on) or
//! duplicate the perm-decision SQL. Hosting the layer here keeps both
//! `has_perm` and its layer wrapper next to each other and follows the
//! natural arrow: auth is the lower-level capability; permissions
//! depend on it.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tower::{Layer, Service};

use crate::perm::has_perm;

// =========================================================================
// PermissionRequired — config shared by the layer
// =========================================================================

/// Configuration carried by [`PermissionRequiredLayer`]. Pin both the
/// permission to check and the rejection shape (API JSON vs HTML
/// redirect on unauthenticated).
#[derive(Debug, Clone)]
pub struct PermissionRequired {
    /// The `"app_label.codename"` string to check via `has_perm`.
    pub perm: String,
    /// `None` = return 401 JSON on unauthenticated. `Some("/login")`
    /// = 302 to `login_url?next=<uri>`.
    pub login_url: Option<String>,
    /// The query-string parameter to append to the redirect URI when
    /// `login_url` is `Some`. `Some("next")` is the convention;
    /// pass `None` to drop the parameter.
    pub next_param: Option<String>,
}

impl PermissionRequired {
    /// API/REST shape: 401 JSON if unauthenticated, 403 JSON if
    /// authenticated but lacks the permission.
    pub fn api(perm: impl Into<String>) -> Self {
        Self {
            perm: perm.into(),
            login_url: None,
            next_param: None,
        }
    }

    /// HTML shape: 302 to `login_url?next=<uri>` if unauthenticated,
    /// 403 HTML if authenticated but lacks the permission.
    pub fn html(perm: impl Into<String>, login_url: impl Into<String>) -> Self {
        Self {
            perm: perm.into(),
            login_url: Some(login_url.into()),
            next_param: Some("next".to_string()),
        }
    }

    /// Drop the `next` parameter from the unauth redirect.
    pub fn no_next(mut self) -> Self {
        self.next_param = None;
        self
    }

    fn unauth_response(&self, uri: &Uri) -> Response {
        match &self.login_url {
            None => {
                let body = json!({"error": "authentication required"}).to_string();
                axum::http::Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .header("content-type", "application/json")
                    .header("www-authenticate", "Bearer")
                    .body(Body::from(body))
                    .expect("building 401 response cannot fail")
                    .into_response()
            }
            Some(url) => {
                let location = match &self.next_param {
                    Some(param) => format!("{url}?{param}={}", urlencoded(&uri.to_string())),
                    None => url.clone(),
                };
                axum::http::Response::builder()
                    .status(StatusCode::FOUND)
                    .header("location", location)
                    .body(Body::empty())
                    .expect("building 302 response cannot fail")
                    .into_response()
            }
        }
    }

    fn forbidden_response(&self) -> Response {
        // 403 stays JSON regardless of the auth shape — the user is
        // authenticated, so we know they have a client capable of
        // reading status codes. A flow that wants a styled 403 page
        // overrides at the handler level.
        let body = json!({
            "error": "permission denied",
            "perm": self.perm,
        })
        .to_string();
        axum::http::Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("building 403 response cannot fail")
            .into_response()
    }
}

fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '?' => out.push_str("%3F"),
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            '+' => out.push_str("%2B"),
            '%' => out.push_str("%25"),
            ' ' => out.push_str("%20"),
            c => out.push(c),
        }
    }
    out
}

// =========================================================================
// PermissionRequiredLayer — tower::Layer impl
// =========================================================================

/// Per-router middleware layer that gates every route in the wrapped
/// subtree on a single permission. Holds the config in an `Arc` so the
/// emitted `Service`s are cheap to clone per request.
#[derive(Clone)]
pub struct PermissionRequiredLayer {
    config: Arc<PermissionRequired>,
}

impl PermissionRequiredLayer {
    pub fn new(config: PermissionRequired) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl<S> Layer<S> for PermissionRequiredLayer {
    type Service = PermissionRequiredService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PermissionRequiredService {
            inner,
            config: self.config.clone(),
        }
    }
}

#[derive(Clone)]
pub struct PermissionRequiredService<S> {
    inner: S,
    config: Arc<PermissionRequired>,
}

impl<S> Service<axum::extract::Request> for PermissionRequiredService<S>
where
    S: Service<axum::extract::Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Response, S::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: axum::extract::Request) -> Self::Future {
        let config = self.config.clone();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let uri = req.uri().clone();

            // audit_2 P4: resolve the caller as a raw STRING user id so the
            // layer is PK-agnostic (i64 / UUID / slug all round-trip). The perm
            // tables already store `user_id` as TEXT; the old i64-only
            // resolver locked every non-i64-PK app out of `permission_required`
            // entirely. Fail closed on an anonymous session OR a session-store
            // error (never trust the request when we can't identify it).
            let user_id_str = match umbral_sessions::current_user_id_str(req.headers()).await {
                Ok(Some(id)) => id,
                _ => return Ok(config.unauth_response(&uri)),
            };

            // One probe of the built-in AuthUser gives both the account's
            // active status and its superuser flag. `None` = not the built-in
            // AuthUser (a custom user model owns activity/superuser elsewhere),
            // so we don't lock those out here.
            let flags = auth_user_flags(&user_id_str).await;

            // Resolve the deactivation / superuser precedence in a pure fn
            // (audit_2 P3 — see `pre_perm_check`). Deactivated denies before
            // any perm/superuser logic; a live superuser bypasses the perm
            // check; everyone else falls through to the DB perm lookup (only
            // then do we pay for `has_perm`).
            match pre_perm_check(flags) {
                PrePermCheck::Deny => return Ok(config.unauth_response(&uri)),
                PrePermCheck::SuperuserAllow => {}
                PrePermCheck::NeedsPerm => {
                    if !has_perm(&user_id_str, &config.perm).await.unwrap_or(false) {
                        return Ok(config.forbidden_response());
                    }
                }
            }

            inner.call(req).await
        })
    }
}

/// The decision reached from the AuthUser flags probe, before any DB perm
/// lookup. Extracted as a pure enum so the deactivation/superuser/perm
/// precedence (audit_2 P3) is exhaustively unit-testable without a live session.
#[derive(Debug, PartialEq, Eq)]
enum PrePermCheck {
    /// Deactivated account — deny outright (a stolen/lingering session for a
    /// disabled user must not retain access, superuser or not).
    Deny,
    /// Active superuser — bypass the perm check.
    SuperuserAllow,
    /// Everyone else — fall through to the `has_perm` DB lookup.
    NeedsPerm,
}

/// Pure precedence for the perm layer, given the `(is_active, is_superuser)`
/// probe (or `None` for a non-AuthUser / custom user model). Deactivation wins
/// over superuser; a custom model is never treated as deactivated or superuser
/// here, so it falls through to the perm lookup rather than being locked out.
fn pre_perm_check(flags: Option<(bool, bool)>) -> PrePermCheck {
    match flags {
        Some((false, _)) => PrePermCheck::Deny, // deactivated — precedence over superuser
        Some((true, true)) => PrePermCheck::SuperuserAllow,
        Some((true, false)) => PrePermCheck::NeedsPerm,
        None => PrePermCheck::NeedsPerm, // custom user model — not locked out
    }
}

/// Best-effort `(is_active, is_superuser)` probe of the built-in AuthUser via
/// the ORM (backend-aware QuerySet). Returns `None` when the id doesn't resolve
/// to an `AuthUser` (a custom user model, or any query error) so custom models
/// aren't locked out — `AuthUser` is the only model probed here. One query
/// serves both the P3 deactivation gate and the superuser bypass.
async fn auth_user_flags(user_id: &str) -> Option<(bool, bool)> {
    use umbral_auth::AuthUser;
    // The built-in AuthUser has an i64 PK. A `user_id` that doesn't parse as
    // i64 is a custom user model (UUID/slug PK) — not this AuthUser — so we
    // return `None` and let the request fall through to the `has_perm` check
    // rather than locking it out (audit_2 P4). Deactivation/superuser (P3) still
    // apply to the built-in AuthUser.
    let id: i64 = user_id.parse().ok()?;
    match AuthUser::objects()
        .filter(umbral::orm::Predicate::<AuthUser>::col_eq("id", id))
        .first()
        .await
    {
        Ok(Some(u)) => Some((u.is_active, u.is_superuser)),
        _ => None,
    }
}

// =========================================================================
// Convenience constructors
// =========================================================================

/// Returns a [`PermissionRequiredLayer`] configured for REST/API use.
/// Unauthenticated → 401 JSON; authenticated-but-no-perm → 403 JSON.
pub fn permission_required(perm: impl Into<String>) -> PermissionRequiredLayer {
    PermissionRequiredLayer::new(PermissionRequired::api(perm))
}

/// Returns a [`PermissionRequiredLayer`] configured for HTML use.
/// Unauthenticated → 302 to `login_url?next=<uri>`; lacks-perm → 403.
pub fn permission_required_html(
    perm: impl Into<String>,
    login_url: impl Into<String>,
) -> PermissionRequiredLayer {
    PermissionRequiredLayer::new(PermissionRequired::html(perm, login_url))
}

#[cfg(test)]
mod pre_perm_check_tests {
    use super::{PrePermCheck, pre_perm_check};

    // audit_2 P3: deactivation must win over every other signal.
    #[test]
    fn deactivated_is_denied_even_when_superuser() {
        assert_eq!(pre_perm_check(Some((false, true))), PrePermCheck::Deny);
        assert_eq!(pre_perm_check(Some((false, false))), PrePermCheck::Deny);
    }

    #[test]
    fn active_superuser_bypasses_perm_check() {
        assert_eq!(
            pre_perm_check(Some((true, true))),
            PrePermCheck::SuperuserAllow
        );
    }

    #[test]
    fn active_non_superuser_needs_perm() {
        assert_eq!(pre_perm_check(Some((true, false))), PrePermCheck::NeedsPerm);
    }

    #[test]
    fn custom_user_model_is_not_locked_out() {
        // `None` = the id isn't the built-in AuthUser; fall through to the perm
        // lookup rather than deny (custom models own their own activity check).
        assert_eq!(pre_perm_check(None), PrePermCheck::NeedsPerm);
    }

    // audit_2 P4: a non-i64 user PK (UUID/slug) isn't the built-in AuthUser, so
    // the flags probe returns `None` WITHOUT a DB hit (the parse short-circuits)
    // — the caller falls through to the PK-agnostic `has_perm` instead of being
    // denied outright as the old i64-only layer did.
    #[tokio::test]
    async fn non_i64_user_id_is_not_probed_as_authuser() {
        let flags = super::auth_user_flags("11111111-1111-1111-1111-111111111111").await;
        assert_eq!(flags, None);
        assert_eq!(pre_perm_check(flags), PrePermCheck::NeedsPerm);
    }
}
