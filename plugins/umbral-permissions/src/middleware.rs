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
            let user_id = umbral_auth::current_session_user_id(req.headers()).await;

            let Some(user_id) = user_id else {
                return Ok(config.unauth_response(&uri));
            };

            // Superuser bypass: if the auth user table carries an
            // `is_superuser` column set to 1, skip the perm check.
            // umbral-permissions does not own the user table schema —
            // we read the column with a tolerant query that returns
            // false when the column doesn't exist (custom user
            // models can opt out by omitting `is_superuser`).
            let is_super = is_superuser_safe(user_id).await;

            // Stringify the user id once — the perm-query layer
            // takes `&str` because the perm tables now store
            // `user_id` as TEXT (PK-agnostic — UUID, slug, int all
            // round-trip via to_string()).
            let user_id_str = user_id.to_string();
            let allowed = if is_super {
                true
            } else {
                has_perm(&user_id_str, &config.perm).await.unwrap_or(false)
            };

            if !allowed {
                return Ok(config.forbidden_response());
            }

            inner.call(req).await
        })
    }
}

/// Best-effort superuser check via the ORM. Hits `auth_user` directly
/// through the `umbral_auth::AuthUser` model so the dispatch goes through
/// the backend-aware QuerySet. Returns false on any error (custom user
/// models that don't carry the `is_superuser` column simply never
/// match — `AuthUser` is the only model probed here).
async fn is_superuser_safe(user_id: i64) -> bool {
    use umbral_auth::AuthUser;
    matches!(
        AuthUser::objects()
            .filter(umbral::orm::Predicate::<AuthUser>::col_eq("id", user_id))
            .first()
            .await,
        Ok(Some(u)) if u.is_superuser && u.is_active
    )
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
