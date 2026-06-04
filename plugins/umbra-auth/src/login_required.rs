//! `@login_required` equivalent for umbra handlers.
//!
//! Django's `@login_required` decorator gates a view behind authentication.
//! umbra ships the same idea in two composable shapes:
//!
//! - [`LoggedIn<U>`] — a per-handler axum extractor. Drop it in a handler
//!   signature and the handler only runs when a valid session exists.
//! - [`LoginRequiredLayer`] — a per-Router tower middleware layer. Every
//!   route in the wrapped subtree is gated; unauthenticated requests never
//!   reach the inner handler.
//!
//! Both shapes share [`LoginRequired`] for the redirect vs. 401 fork.
//!
//! ## Design decisions
//!
//! - `LoggedIn<U: UserModel>` is **fully generic** over the user model
//!   (option a from the spec). The cookie/session reading is ~25 lines of
//!   direct logic (read cookie, hash it, query session table, hydrate U).
//!   Keeping it generic means a custom user model (`TenantUser` etc.) can
//!   use `LoggedIn<TenantUser>` without any wrapper or code duplication.
//!
//! - The `LoginRequired` config is read from `request.extensions()` when
//!   set by `LoginRequiredLayer`, or falls back to `LoginRequired::API`
//!   (401 JSON) if the extractor is used directly without the layer.
//!
//! - `LoginRequiredLayer` implements `tower::Layer<S>` directly so it
//!   works with `Router::layer(login_required())` and
//!   `Router::layer(login_required_html("/login"))` without extra
//!   wrapping.
//!
//! - The layer gate does NOT load the full user struct — it checks only
//!   the session table (`user_id IS NOT NULL AND expires_at > now`). The
//!   `LoggedIn<U>` extractor does the full hydration. This avoids the `U`
//!   bound at the layer level, so `login_required()` works with any user
//!   model without a type parameter on the layer.
//!
//! ## Deferred
//!
//! - `permission_required(perm)` and `staff_member_required` are deferred
//!   pending gap 33 (groups + content-type model). They can be added as
//!   thin wrappers once permission objects exist.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum_core::extract::FromRequestParts;
use chrono::{DateTime, Utc};
use http::request::Parts;
use serde_json::json;
use sha2::{Digest, Sha256};
use tower::{Layer, Service};

use crate::UserModel;

// =========================================================================
// LoginRequired — shared config struct
// =========================================================================

/// Configuration shared by both the extractor and the middleware.
///
/// Controls whether an unauthenticated request gets a JSON 401 (REST/API
/// behaviour) or a 302 redirect to a login page (server-rendered HTML
/// behaviour).
#[derive(Debug, Clone)]
pub struct LoginRequired {
    /// `None` = return 401 JSON. `Some("/login")` = 302 to
    /// `login_url?next=<uri>`.
    pub login_url: Option<String>,
    /// The query-string parameter name to append with the original URI.
    /// `Some("next")` appends `?next=<uri>`; `None` redirects without it.
    /// Only used when `login_url` is `Some`.
    pub next_param: Option<String>,
}

impl LoginRequired {
    /// API/REST shape: return a JSON 401 with a `WWW-Authenticate: Bearer`
    /// header.
    pub const API: Self = Self {
        login_url: None,
        next_param: None,
    };

    /// HTML shape: redirect to `login_url?next=<original-uri>`. The `next`
    /// parameter is named `"next"` by default, matching Django's convention.
    pub fn html(login_url: impl Into<String>) -> Self {
        Self {
            login_url: Some(login_url.into()),
            next_param: Some("next".to_string()),
        }
    }

    /// Drop the `next` parameter from the redirect.
    pub fn no_next(mut self) -> Self {
        self.next_param = None;
        self
    }

    /// Build the rejection response.
    pub(crate) fn rejection_response(&self, uri: &Uri) -> Response {
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
                    Some(param) => {
                        let original = uri.to_string();
                        format!("{url}?{param}={}", urlencoded(original.as_str()))
                    }
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
}

/// Percent-encode a URI for safe embedding in a query-string value.
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
// LoggedIn<U> extractor
// =========================================================================

/// Per-handler axum extractor that resolves the session cookie into a user
/// of type `U`.
///
/// ```rust,ignore
/// use umbra_auth::{AuthUser, login_required::LoggedIn};
///
/// async fn dashboard(LoggedIn(user): LoggedIn<AuthUser>) -> String {
///     format!("Hello, {}!", user.username())
/// }
/// ```
///
/// If no valid session exists the extractor returns the configured rejection
/// response. The config is read from `request.extensions()` (set by
/// [`LoginRequiredLayer`]) or falls back to [`LoginRequired::API`].
pub struct LoggedIn<U: UserModel>(pub U);

// `LoggedIn` is a tuple-newtype around `U`. Drop in `Deref` /
// `DerefMut` (so `user.username()` works directly without the
// `.0`) and `Serialize` (so it slots into template contexts via
// `context!(user)` without `user.0`). Closes BUG-18 from
// bugs/tests/testBugs.md — the original ergonomic gap that
// pushed test code to write `let username = user.0.username();`
// for what should be the obvious shape.
impl<U: UserModel> std::ops::Deref for LoggedIn<U> {
    type Target = U;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<U: UserModel> std::ops::DerefMut for LoggedIn<U> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<U: UserModel + serde::Serialize> serde::Serialize for LoggedIn<U> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Forward verbatim so `LoggedIn<AuthUser>` round-trips
        // exactly the same shape `AuthUser` would on its own.
        self.0.serialize(serializer)
    }
}

impl<U, S> FromRequestParts<S> for LoggedIn<U>
where
    U: UserModel
        + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
        + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
        + umbra::orm::HydrateRelated
        + Unpin
        + Send,
    // The session-row parse step is the bit that needs FromStr —
    // an `i64`, `Uuid`, `String`, or hand-rolled PK type all
    // implement it for free; a future PK shape with no string
    // representation would have to override `id_string` AND
    // provide a `FromStr` mirror to keep this extractor happy.
    <U as umbra::orm::Model>::PrimaryKey: std::str::FromStr,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let config = parts
            .extensions
            .get::<LoginRequired>()
            .cloned()
            .unwrap_or(LoginRequired::API);

        let uri = parts.uri.clone();

        match resolve_user::<U>(&parts.headers).await {
            Some(user) => Ok(LoggedIn(user)),
            None => Err(config.rejection_response(&uri)),
        }
    }
}

// =========================================================================
// Session resolution helpers
// =========================================================================

/// SHA-256 hash the raw session token. Mirrors `umbra-sessions`'s
/// `hash_token`. umbra-auth must not depend on umbra-sessions (the dep
/// arrow runs the other way), so we re-implement the trivial hash step.
fn hash_token(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    format!("{:x}", h.finalize())
}

/// Extract the `umbra_session` cookie from the request headers.
fn cookie_from_headers(headers: &http::HeaderMap) -> Option<String> {
    let header = headers.get(http::header::COOKIE)?.to_str().ok()?;
    for pair in header.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix("umbra_session=") {
            return Some(value.to_string());
        }
    }
    None
}

/// Load a user of type `U` from the session cookie in the given
/// headers. The generic shape powers both [`LoggedIn`] and the
/// public [`crate::current_user_as`] helper — apps using a custom
/// `UserModel` reach for the latter from their own handlers when
/// the AuthUser-flavoured [`crate::current_user`] doesn't fit.
///
/// **Polymorphic over `U::PrimaryKey`** — the session row stores
/// the user PK as text (gap #59); we parse it back to the typed PK
/// via `FromStr` before feeding it to the ORM, so a `UuidUser`
/// stays UUID-shaped on the WHERE clause and an `AuthUser` stays
/// `i64`-shaped. There is no `parse::<i64>()` hardcoded anywhere
/// in the framework's session-read path; the typed PK threads
/// through verbatim.
///
/// Conventions assumed: `U` has an `id` column populated by the
/// model's PK type, and an `is_active` boolean column the filter
/// excludes deactivated rows on. Custom user models that rename
/// either column write their own resolver against
/// [`umbra_sessions::current_user_id_str`] instead.
pub async fn resolve_user<U>(headers: &http::HeaderMap) -> Option<U>
where
    U: UserModel
        + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
        + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
        + umbra::orm::HydrateRelated
        + Unpin
        + Send,
    <U as umbra::orm::Model>::PrimaryKey: std::str::FromStr,
{
    let user_id = current_session_user_pk::<U>(headers).await?;
    umbra::orm::Manager::<U>::default()
        .filter(
            umbra::orm::Predicate::<U>::col_eq("id", user_id)
                & umbra::orm::Predicate::<U>::col_eq("is_active", true),
        )
        .first()
        .await
        .ok()
        .flatten()
}

/// Read the request's session cookie and resolve it to the
/// authenticated user's TYPED primary key. The generic version of
/// [`current_session_user_id`]; this is what [`resolve_user`] and
/// the future `permission_required_as<U>` build on.
///
/// Parses the text `session.user_id` (gap #59) via
/// `<U::PrimaryKey as FromStr>::from_str`. A non-parseable value
/// (the row was written by a different `UserModel` impl) resolves
/// to `None` — same as missing cookie or expired session, so the
/// caller's "anonymous" branch fires.
pub async fn current_session_user_pk<U>(
    headers: &http::HeaderMap,
) -> Option<<U as umbra::orm::Model>::PrimaryKey>
where
    U: UserModel,
    <U as umbra::orm::Model>::PrimaryKey: std::str::FromStr,
{
    let raw_token = cookie_from_headers(headers)?;
    let stored_id = hash_token(&raw_token);
    let row: Option<SessionRow> = umbra::orm::Manager::<SessionRow>::default()
        .filter(umbra::orm::Predicate::<SessionRow>::col_eq("id", stored_id))
        .first()
        .await
        .ok()
        .flatten();
    let row = row?;
    if row.expires_at < Utc::now() {
        return None;
    }
    row.user_id?.parse().ok()
}

/// Check whether headers carry a valid authenticated session.
/// Returns `true` iff a valid, non-expired, non-anonymous session is present.
pub(crate) async fn is_authenticated(headers: &http::HeaderMap) -> bool {
    current_session_user_id(headers).await.is_some()
}

/// Resolve the `umbra_session` cookie in `headers` to the
/// authenticated user's `i64` PK — the AuthUser-specific shorthand
/// for [`current_session_user_pk::<AuthUser>`]. Returns `None` for
/// missing cookie, expired session, anonymous session, a
/// non-parseable `user_id` (session written by a non-AuthUser
/// model), or any sqlx error.
///
/// This is the primitive `permission_required` (in `umbra-permissions`)
/// builds on. Callers using a custom user model reach for
/// [`current_session_user_pk`] (the typed generic) or
/// [`umbra_sessions::current_user_id_str`] (the raw string)
/// instead — both stay polymorphic over the active user model's PK.
pub async fn current_session_user_id(headers: &http::HeaderMap) -> Option<i64> {
    current_session_user_pk::<crate::AuthUser>(headers).await
}

/// Private mirror of `umbra_sessions::Session`. Lives here because
/// `umbra-auth` does not depend on `umbra-sessions` (the dep arrow runs
/// the other way), but we still need ORM access to the `session` table.
/// Multiple `Model` impls can target the same table — sea-query treats
/// the schema as data, not a type-level singleton.
#[doc(hidden)]
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "session")]
pub struct SessionRow {
    pub id: String,
    /// Polymorphic user-PK column (gap #59). Stored as the user's PK
    /// `Display` form — i64 for AuthUser, UUID for custom user models,
    /// etc. Parse with `<U::PrimaryKey as FromStr>::from_str` on the
    /// way out.
    pub user_id: Option<String>,
    pub data: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

// =========================================================================
// LoginRequiredLayer — tower::Layer impl
// =========================================================================

/// Per-router middleware layer that gates every route in the wrapped subtree.
///
/// ```rust,ignore
/// use umbra_auth::login_required::{login_required, login_required_html};
///
/// // REST subtree — 401 JSON on unauthenticated.
/// let api_router = Router::new()
///     .route("/api/me", get(me_handler))
///     .layer(login_required());
///
/// // HTML subtree — 302 to /login?next=<uri>.
/// let app_router = Router::new()
///     .route("/dashboard", get(dashboard_handler))
///     .layer(login_required_html("/login"));
/// ```
///
/// The layer also inserts the [`LoginRequired`] config into request
/// extensions so nested [`LoggedIn<U>`] extractors pick it up without
/// re-declaration.
#[derive(Clone)]
pub struct LoginRequiredLayer {
    config: LoginRequired,
}

impl LoginRequiredLayer {
    /// Build a layer with an explicit config.
    pub fn new(config: LoginRequired) -> Self {
        Self { config }
    }

    /// Apply this layer to a Router, returning the gated router.
    ///
    /// ```rust,ignore
    /// let gated = LoginRequiredLayer::new(LoginRequired::html("/login"))
    ///     .apply(my_router);
    /// ```
    pub fn apply(self, router: axum::Router) -> axum::Router {
        router.layer(self)
    }
}

impl<S> Layer<S> for LoginRequiredLayer {
    type Service = LoginRequiredService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LoginRequiredService {
            inner,
            config: self.config.clone(),
        }
    }
}

/// The tower `Service` produced by [`LoginRequiredLayer`].
#[derive(Clone)]
pub struct LoginRequiredService<S> {
    inner: S,
    config: LoginRequired,
}

impl<S> Service<axum::extract::Request> for LoginRequiredService<S>
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

    fn call(&mut self, mut req: axum::extract::Request) -> Self::Future {
        let config = self.config.clone();
        // Clone inner for the async block — `self.inner` is consumed
        // by `call()` semantically and must be driven after `poll_ready`.
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let uri = req.uri().clone();

            if !is_authenticated(req.headers()).await {
                return Ok(config.rejection_response(&uri));
            }

            // Insert config so LoggedIn<U> extractors can find it.
            req.extensions_mut().insert(config);

            inner.call(req).await
        })
    }
}

// =========================================================================
// Convenience constructors
// =========================================================================

/// Returns a [`LoginRequiredLayer`] configured for REST/API use (401 JSON).
///
/// ```rust,ignore
/// Router::new()
///     .route("/api/me", get(me_handler))
///     .layer(login_required())
/// ```
pub fn login_required() -> LoginRequiredLayer {
    LoginRequiredLayer::new(LoginRequired::API)
}

/// Returns a [`LoginRequiredLayer`] configured for HTML use (302 redirect).
///
/// ```rust,ignore
/// Router::new()
///     .route("/dashboard", get(dashboard_handler))
///     .layer(login_required_html("/login"))
/// ```
pub fn login_required_html(login_url: impl Into<String>) -> LoginRequiredLayer {
    LoginRequiredLayer::new(LoginRequired::html(login_url))
}
