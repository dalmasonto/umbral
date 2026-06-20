//! umbra-auth — the built-in authentication plugin.
//!
//! The first crate under `plugins/` and the proof of the M7 plugin
//! contract: a real built-in expressed through `umbra::prelude::Plugin`
//! with no special-casing inside `umbra-core`. Auth is the most common
//! Django plugin, so getting it right here also pressure-tests the
//! contract for the rest.
//!
//! ## M9 v1 scope
//!
//! - [`AuthUser`] model: the canonical Django-shape User (username,
//!   email, password hash, `is_active` / `is_staff` / `is_superuser`,
//!   `date_joined`, `last_login`).
//! - [`UserModel`] trait: the minimum surface a custom user model must
//!   satisfy so `AuthPlugin<U>` can swap in any user type. Default impls
//!   cover the optional flag methods so a minimal custom user struct
//!   only has to implement the load-bearing four.
//! - argon2 password hashing via [`hash_password`] / [`verify_password`].
//! - [`create_user`], [`authenticate`], [`set_password`] helpers.
//!   `authenticate` and `set_password` are generic over any `U: UserModel`.
//! - [`AuthPlugin`] registers the user model and contributes one system
//!   check. The type parameter defaults to [`AuthUser`] so existing apps
//!   need no changes.
//! - [`login_required`] module: `LoginRequired` config, `LoggedIn<U>`
//!   extractor, `LoginRequiredLayer` middleware, and the
//!   `login_required()` / `login_required_html()` convenience
//!   constructors. Django's `@login_required` in two shapes.
//!
//! ## Custom user models
//!
//! ```ignore
//! // 1. Declare a custom user struct.
//! #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
//! pub struct TenantUser {
//!     pub id: i64,
//!     pub username: String,
//!     pub password_hash: String,
//!     pub tenant_id: i64,
//!     pub is_active: bool,
//! }
//!
//! // 2. Implement UserModel (only the four required methods).
//! impl umbra_auth::UserModel for TenantUser {
//!     fn id(&self) -> i64               { self.id }
//!     fn username(&self) -> &str        { &self.username }
//!     fn password_hash(&self) -> &str   { &self.password_hash }
//!     fn set_password_hash(&mut self, h: String) { self.password_hash = h; }
//! }
//!
//! // 3. Wire the plugin with your type.
//! App::builder()
//!     .plugin(AuthPlugin::<TenantUser>::default())
//!     .build()?
//! ```
//!
//! ## Deferred (per `docs/specs/outlines/auth-and-sessions.md`)
//!
//! - Permissions, groups, the auth-backend chain.
//! - The `Auth<U>` request extractor + `#[login_required]`
//!   middleware. Needs `Plugin::middleware()` lifted (M7 deferral).
//! - Login / logout / password-reset HTTP flows. Needs the full
//!   `umbra-sessions` session middleware wired end-to-end.
//! - Periodic session cleanup via `umbra-tasks`.

pub mod auth_routes;
pub mod bearer_auth;
pub mod extractors;
pub mod login_required;
pub mod password_validation;
pub mod session_user;
pub mod throttle;
pub mod token;

pub use password_validation::{
    CommonPasswordValidator, MinLengthValidator, NumericPasswordValidator, PasswordContext,
    PasswordPolicy, PasswordValidator, UserAttributeSimilarityValidator, validate_password,
};

pub use bearer_auth::{BearerAuthentication, parse_bearer_header};
pub use extractors::{CurrentIdentity, OptionalIdentity, resolve_identity};
pub use login_required::{
    LoggedIn, LoginRequired, LoginRequiredLayer, current_session_user_id, login_required,
    login_required_html, resolve_user as current_user_as,
};
pub use session_user::{
    OptionalUser, SessionAuthentication, User, current_user, login, login_with_request, logout,
    user_context_layer,
};
pub use throttle::{
    Throttle, ThrottleConfig, login_throttle_check, login_throttle_clear, register_throttle_check,
};
pub use token::{AuthToken, PlaintextToken, TOKEN_PREFIX, digest_token};

use std::marker::PhantomData;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version, password_hash::rand_core::OsRng};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbra::prelude::*;

// =========================================================================
// UserModel trait
// =========================================================================

/// The minimum surface a user model must expose so `AuthPlugin<U>` can
/// operate on it generically.
///
/// All four required methods map directly to columns that auth ACTUALLY
/// reads or writes. Optional flag methods (`is_active`, `is_staff`,
/// `is_superuser`) have default impls that return the safe defaults so a
/// minimal custom user struct doesn't have to repeat them.
///
/// `AuthUser` implements this trait unchanged, so existing code that
/// calls the auth helpers directly keeps working.
///
/// ## Required methods
///
/// | Method | Column | Used by |
/// |---|---|---|
/// | `id()` | `id` | `set_password` WHERE clause; session storage |
/// | `username()` | `username` | `authenticate` SELECT, `createsuperuser` output |
/// | `password_hash()` | `password_hash` | `authenticate` verify step |
/// | `set_password_hash()` | `password_hash` | `set_password` in-place update |
///
/// ## Default methods
///
/// | Method | Default | Used by |
/// |---|---|---|
/// | `id_string()` | `self.id().to_string()` | `Identity::user_id`, session row |
/// | `is_active()` | `true` | `authenticate` active-user gate |
/// | `is_staff()` | `false` | admin require_staff check |
/// | `is_superuser()` | `false` | permission gates |
///
/// ## Polymorphic primary key
///
/// `id()` returns the model's typed primary key via the existing
/// `Model::PrimaryKey` associated type — the framework no longer
/// hardcodes `i64`. A custom user model keyed by `uuid::Uuid`
/// works as-is:
///
/// ```ignore
/// #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize,
///          umbra::orm::Model)]
/// pub struct UuidUser {
///     pub id: uuid::Uuid,
///     pub username: String,
///     pub password_hash: String,
///     pub is_active: bool,
///     pub is_staff: bool,
/// }
/// impl umbra_auth::UserModel for UuidUser {
///     fn id(&self) -> uuid::Uuid { self.id }
///     fn username(&self) -> &str { &self.username }
///     fn password_hash(&self) -> &str { &self.password_hash }
///     fn set_password_hash(&mut self, h: String) { self.password_hash = h; }
///     fn is_active(&self) -> bool { self.is_active }
///     fn is_staff(&self) -> bool { self.is_staff }
/// }
/// ```
///
/// The session-row text column, [`Identity::user_id`], and the
/// permissions plugin all speak strings (via `id_string()`); the
/// ORM-side WHERE clauses use the typed PK directly (via the
/// `PrimaryKey: Into<sea_query::Value>` bound). Nothing in the
/// framework parses `id()` back to `i64`.
pub trait UserModel: Model + Send + Sync + 'static {
    /// The row's typed primary key. `set_password` uses this in the
    /// UPDATE WHERE clause; bearer-token / session backends use it
    /// to filter on `auth_user::ID.eq(user.id())` style predicates.
    ///
    /// The return type is `<Self as Model>::PrimaryKey`, which the
    /// `#[derive(Model)]` macro derives from the `id` field's type
    /// (`i64`, `uuid::Uuid`, `String`, etc.). All `PrimaryKey`
    /// types implement `Display`, so [`id_string`](Self::id_string)
    /// can stringify without an explicit per-impl override.
    fn id(&self) -> <Self as Model>::PrimaryKey;

    /// The PK as a string. Used by [`umbra_sessions`] (which stores
    /// `user_id` as text) and by the REST identity contract's
    /// [`Identity::user_id`](umbra::auth::Identity) (which is
    /// uniform across user models).
    ///
    /// Default uses the typed PK's `Display` impl — override only
    /// when the stringification needs to differ from `Display`
    /// (e.g. a base64-encoded ULID).
    fn id_string(&self) -> String {
        self.id().to_string()
    }

    /// The unique login handle. Matched against the username column in
    /// `authenticate`'s SELECT query.
    fn username(&self) -> &str;

    /// The argon2 PHC-encoded password hash stored in the DB column.
    /// `authenticate` reads this, verifies it, and moves on.
    fn password_hash(&self) -> &str;

    /// Replace the in-memory password hash. Called by `set_password`
    /// after writing the new hash to the database, so the caller's
    /// `&mut U` reflects the update without a re-fetch.
    fn set_password_hash(&mut self, hash: String);

    /// Whether this account is active. `authenticate` rejects inactive
    /// users with `InvalidCredentials` (same error as wrong password -
    /// no account enumeration). Default: `true`.
    fn is_active(&self) -> bool {
        true
    }

    /// Whether this account has staff-level access to the admin
    /// interface. Default: `false`.
    fn is_staff(&self) -> bool {
        false
    }

    /// Whether this account has superuser rights. Default: `false`.
    fn is_superuser(&self) -> bool {
        false
    }
}

// =========================================================================
// Built-in AuthUser model
// =========================================================================

/// The canonical authentication user. `#[derive(Model)]` snake_cases
/// the struct name into the table name `auth_user`; the M3 derive
/// doesn't yet accept `#[umbra(table = ...)]` so the snake_case
/// round-trip is the only way to get a plugin-prefixed table name
/// until the attribute lands.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct AuthUser {
    pub id: i64,
    #[umbra(unique)]
    pub username: String,
    /// Shown read-only on edit forms; never on create forms (use the
    /// admin's password field mechanism for changes).
    #[umbra(noedit, unique)]
    pub email: String,
    /// Never shown on any form — password management goes through the
    /// dedicated Change Password flow in the admin.
    #[umbra(noform)]
    pub password_hash: String,
    pub is_active: bool,
    pub is_staff: bool,
    pub is_superuser: bool,
    pub date_joined: DateTime<Utc>,
    pub last_login: Option<DateTime<Utc>>,
}

impl UserModel for AuthUser {
    // `<AuthUser as Model>::PrimaryKey` is `i64` — the derive picks
    // it up from the `id: i64` field. Returning `self.id` directly
    // satisfies `fn id(&self) -> <Self as Model>::PrimaryKey` for
    // the default AuthUser shape; a custom user model with a
    // `uuid::Uuid` PK would return `self.id` of that type, and the
    // default `id_string()` would stringify via `Display` for free.
    fn id(&self) -> <Self as umbra::orm::Model>::PrimaryKey {
        self.id
    }

    fn username(&self) -> &str {
        &self.username
    }

    fn password_hash(&self) -> &str {
        &self.password_hash
    }

    fn set_password_hash(&mut self, hash: String) {
        self.password_hash = hash;
    }

    fn is_active(&self) -> bool {
        self.is_active
    }

    fn is_staff(&self) -> bool {
        self.is_staff
    }

    fn is_superuser(&self) -> bool {
        self.is_superuser
    }
}

// =========================================================================
// AuthPlugin<U>
// =========================================================================

/// The built-in authentication plugin, generic over the user model.
///
/// `U` defaults to [`AuthUser`] so `AuthPlugin::default()` continues to
/// work in all existing code unchanged. Apps that need a custom user type
/// opt in with one line:
///
/// ```ignore
/// .plugin(AuthPlugin::<CustomUser>::default())
/// ```
///
/// ## `user_model_name`
///
/// An optional informational string surfaced in OpenAPI schemas and the
/// admin nav. Default `None` (resolved from `U::NAME` by the plugin
/// itself when left empty). Set it explicitly when the type name is
/// insufficient:
///
/// ```ignore
/// AuthPlugin::<TenantUser>::default().user_model_name("tenant_user")
/// ```
#[derive(Debug)]
pub struct AuthPlugin<U: UserModel = AuthUser> {
    /// Documentation-only: the human-readable name of the active user
    /// model. Consumed by admin / OpenAPI when surfacing the user table.
    /// The actual dispatch is entirely through the type parameter `U`.
    pub user_model_name: Option<String>,
    /// When `Some`, mount the four built-in routes (register / login /
    /// logout / me) under this prefix. `None` skips them — the user
    /// either doesn't want them or is rolling their own surface. Only
    /// settable on `AuthPlugin<AuthUser>` (the handlers FK into
    /// `AuthToken` → `AuthUser`); custom user models bring their own.
    pub default_routes_prefix: Option<String>,
    /// When true, wrap the app router with [`user_context_layer`] so
    /// every template render has `user` in its global context:
    /// `{ is_authenticated, is_staff, username, ... }`. Opt-in because
    /// it costs one DB read per request (cookie → session → user); a
    /// REST-only service has nothing to gain from it. Set via
    /// [`AuthPlugin::with_user_in_templates`].
    pub user_in_templates: bool,
    /// The password-strength policy this plugin installs at boot. `None`
    /// here is NOT "no validation" — `on_ready` installs
    /// [`PasswordPolicy::default`] (the full secure set) when this is left
    /// unset, so the plugin is secure by default. The only way to get an
    /// empty policy is to call [`AuthPlugin::disable_password_validation`],
    /// which stores an explicit [`PasswordPolicy::empty`].
    ///
    /// Wrapped in a `Mutex` because `Plugin::on_ready` only borrows `&self`
    /// yet needs to MOVE the policy into the ambient `OnceLock`
    /// ([`PasswordPolicy`] is not `Clone` — it holds boxed trait objects).
    /// The mutex lets `on_ready` `.take()` it; the first boot wins.
    password_policy: std::sync::Mutex<Option<PasswordPolicy>>,
    /// The login/register rate-limit configuration this plugin installs at
    /// boot. Secure by default ([`ThrottleConfig::default`]: login 5 / 5 min
    /// per IP+username, register 10 / hour per IP, `enabled = true`). Builder
    /// methods ([`AuthPlugin::login_throttle`], [`AuthPlugin::register_throttle`])
    /// tune the budgets; [`AuthPlugin::disable_throttle`] flips `enabled` off
    /// as an explicit opt-out. `Copy`, so no `Mutex`/`take` dance is needed —
    /// `on_ready` reads it directly.
    throttle_config: throttle::ThrottleConfig,
    _u: PhantomData<U>,
}

impl<U: UserModel> Default for AuthPlugin<U> {
    fn default() -> Self {
        Self {
            user_model_name: None,
            default_routes_prefix: None,
            user_in_templates: false,
            // SECURE BY DEFAULT: an unconfigured AuthPlugin enforces the
            // full validator set. `None` defers to PasswordPolicy::default()
            // (the secure set) at install time; it does NOT mean "off".
            password_policy: std::sync::Mutex::new(None),
            // SECURE BY DEFAULT: throttling is ON for login + register with
            // the credential-stuffing-resistant budgets above. `disable_throttle`
            // is the only path that turns it off.
            throttle_config: throttle::ThrottleConfig::default(),
            _u: PhantomData,
        }
    }
}

impl<U: UserModel> AuthPlugin<U> {
    /// Override the informational user-model name shown in admin / OpenAPI.
    /// Fluent builder method; the return type is `Self` so it chains.
    pub fn user_model_name(mut self, name: impl Into<String>) -> Self {
        self.user_model_name = Some(name.into());
        self
    }

    /// Mount the [`user_context_layer`] middleware globally so every
    /// HTML template gets `user` in its render context — anonymous
    /// requests see `{ is_authenticated: false }`, authenticated
    /// requests see the full serialized [`AuthUser`] merged with
    /// `is_authenticated: true`. Lets templates write
    /// `{% if user.is_staff %}` without the consumer having to thread
    /// a user value into every handler's context manually.
    ///
    /// One DB read per request (cookie → session → user row). Off by
    /// default because REST-only services have no templates and the
    /// cost would be pure overhead. Turn it on for HTML-heavy apps:
    ///
    /// ```ignore
    /// AuthPlugin::<AuthUser>::default()
    ///     .with_default_routes()
    ///     .with_user_in_templates()   // ← here
    /// ```
    ///
    /// Implemented via [`Plugin::wrap_router`]; the wrapper wraps the
    /// merged app router (including every other plugin's routes), so
    /// admin / REST / playground / your own handlers all see the
    /// populated context with one builder call.
    pub fn with_user_in_templates(mut self) -> Self {
        self.user_in_templates = true;
        self
    }

    /// Replace the default password-strength policy with a custom one.
    /// The full [`PasswordPolicy`] you pass becomes the active set at boot;
    /// the Django-default validators are NOT merged in. Build the policy
    /// you want from scratch:
    ///
    /// ```ignore
    /// use umbra_auth::{AuthPlugin, PasswordPolicy, MinLengthValidator, CommonPasswordValidator};
    /// AuthPlugin::<AuthUser>::default().password_validators(
    ///     PasswordPolicy::empty()
    ///         .with(Box::new(MinLengthValidator(12)))
    ///         .with(Box::new(CommonPasswordValidator)),
    /// )
    /// ```
    pub fn password_validators(mut self, policy: PasswordPolicy) -> Self {
        self.password_policy = std::sync::Mutex::new(Some(policy));
        self
    }

    /// Convenience: keep the four default validators but change the minimum
    /// password length. Equivalent to building a [`PasswordPolicy`] with a
    /// [`MinLengthValidator`] of `n` plus the other three defaults.
    pub fn min_password_length(self, n: usize) -> Self {
        self.password_validators(PasswordPolicy::new(vec![
            Box::new(MinLengthValidator(n)),
            Box::new(CommonPasswordValidator),
            Box::new(NumericPasswordValidator),
            Box::new(UserAttributeSimilarityValidator::default()),
        ]))
    }

    /// Explicit opt-OUT: install an empty policy so NO password validation
    /// runs. Secure-by-default means an app that genuinely wants to accept
    /// any password — a throwaway demo, a migration importing legacy hashes
    /// with externally-validated plaintext — has to ask for it by name.
    /// Don't reach for this to silence a failing test; fix the fixture's
    /// password instead.
    pub fn disable_password_validation(mut self) -> Self {
        self.password_policy = std::sync::Mutex::new(Some(PasswordPolicy::empty()));
        self
    }

    /// Tune the login rate limit: `max` failed-or-not attempts per trailing
    /// `window`, keyed per IP + username. The default is 5 / 5 min — a budget
    /// that stops credential-stuffing dead while leaving room for a human who
    /// fat-fingers their password a couple of times (a successful login also
    /// clears the counter). Lower it for a high-security surface; raise it for
    /// a shared-NAT office where many users hit login from one IP.
    ///
    /// ```ignore
    /// AuthPlugin::<AuthUser>::default().login_throttle(10, Duration::from_secs(300))
    /// ```
    pub fn login_throttle(mut self, max: usize, window: std::time::Duration) -> Self {
        self.throttle_config.login_max = max;
        self.throttle_config.login_window = window;
        self
    }

    /// Tune the register rate limit: `max` account-creation attempts per
    /// trailing `window`, keyed per IP. The default is 10 / hour, which brakes
    /// mass automated signups without blocking a legitimate burst from one
    /// office.
    pub fn register_throttle(mut self, max: usize, window: std::time::Duration) -> Self {
        self.throttle_config.register_max = max;
        self.throttle_config.register_window = window;
        self
    }

    /// Explicit opt-OUT: turn login + register throttling OFF entirely.
    /// Secure-by-default means an app that genuinely wants no rate limit — a
    /// load test, an internal tool behind its own gateway limiter — has to ask
    /// for it by name. Don't reach for this to silence a throttled test; use a
    /// distinct IP/username per attempt or a generous `login_throttle` instead.
    pub fn disable_throttle(mut self) -> Self {
        self.throttle_config.enabled = false;
        self
    }
}

// =========================================================================
// Default route opt-in. Only exposed on AuthPlugin<AuthUser> because the
// handlers FK into AuthUser via AuthToken. Custom user models would need a
// different token model + different handlers; they bring their own surface.
// The concrete impl block (no <U>) is the compile-time witness: calling
// `.with_default_routes()` on `AuthPlugin::<CustomUser>` is an error at
// the call site, not a silent no-op at runtime.
// =========================================================================
impl AuthPlugin<AuthUser> {
    /// Mount the built-in `/api/auth/{register,login,logout,me}`
    /// surface. Same handlers that lived in the derive-demo example
    /// app, promoted to the framework so every app gets them with one
    /// line. JSON-only; UNIQUE-violation → 409; login returns both a
    /// Set-Cookie and a bearer token in one response so browsers and
    /// CLI clients share an endpoint.
    pub fn with_default_routes(mut self) -> Self {
        self.default_routes_prefix = Some("/api/auth".to_string());
        self
    }

    /// Same as [`Self::with_default_routes`] but the prefix is yours
    /// to pick. Useful when `/api/auth` collides with an existing
    /// surface or you want versioning (`/v1/auth`).
    pub fn with_default_routes_at(mut self, prefix: impl Into<String>) -> Self {
        self.default_routes_prefix = Some(prefix.into());
        self
    }
}

impl<U: UserModel> Plugin for AuthPlugin<U> {
    fn name(&self) -> &'static str {
        "auth"
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        // AuthToken FKs against AuthUser specifically (FK target is
        // a concrete `Model` type, not a `UserModel`). Apps wiring
        // `AuthPlugin::<CustomUser>` get the user table migrated but
        // NOT the token table — they bring their own token model
        // and their own bearer-auth backend.
        let mut models = vec![umbra::migrate::ModelMeta::for_::<U>()];
        if std::any::TypeId::of::<U>() == std::any::TypeId::of::<AuthUser>() {
            models.push(umbra::migrate::ModelMeta::for_::<AuthToken>());
        }
        models
    }

    fn commands(&self) -> Vec<Box<dyn umbra::cli::PluginCommand>> {
        vec![Box::new(CreateSuperuserCommand)]
    }

    fn routes(&self) -> umbra::web::Router {
        // `default_routes_prefix` is only ever Some when U = AuthUser
        // (the only impl block that sets it is `impl AuthPlugin<AuthUser>`).
        // So the prefix-guarded branch is dead code for any custom user
        // model — both at compile time (the builder method isn't
        // visible) and at runtime (the field stays None).
        match &self.default_routes_prefix {
            Some(prefix) => auth_routes::build_router(prefix),
            None => umbra::web::Router::new(),
        }
    }

    fn route_paths(&self) -> Vec<umbra::routes::RouteSpec> {
        match &self.default_routes_prefix {
            Some(prefix) => auth_routes::declared_routes(prefix),
            None => Vec::new(),
        }
    }

    fn openapi_paths(&self) -> Vec<(String, serde_json::Value)> {
        match &self.default_routes_prefix {
            Some(prefix) => auth_routes::openapi_paths(prefix),
            None => Vec::new(),
        }
    }

    /// Mount [`user_context_layer`] on the full merged router when the
    /// `user_in_templates` flag is on (see
    /// [`AuthPlugin::with_user_in_templates`]). The layer reads the
    /// session cookie, hydrates the [`AuthUser`], and pushes a
    /// `serde_json` representation into [`umbra::templates::CURRENT_USER`]
    /// for the duration of the request — every template render
    /// downstream gets `user` in its global context with no per-handler
    /// plumbing.
    ///
    /// Off by default — see the builder method's docstring for the
    /// "why" (one DB read per request, pointless for REST-only apps).
    fn wrap_router(&self, router: umbra::web::Router) -> umbra::web::Router {
        if self.user_in_templates {
            router.layer(axum::middleware::from_fn(user_context_layer))
        } else {
            router
        }
    }

    /// Seal the password-strength policy into the ambient `OnceLock` so the
    /// free-function helpers (`create_user`, `set_password`) can read it
    /// without a handle to `Self`. Mirrors the sessions plugin's
    /// `SLIDING_EXPIRY_ENABLED` install.
    ///
    /// A `None` configured policy means "use the secure default" — NOT
    /// "off" — so we install [`PasswordPolicy::default`] in that case.
    /// `disable_password_validation` is the only path that installs an
    /// empty policy. The install is idempotent (first boot wins), matching
    /// the ambient-pool contract.
    fn on_ready(&self, _ctx: &umbra::plugin::AppContext) -> Result<(), umbra::plugin::PluginError> {
        let policy = self
            .password_policy
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
            .unwrap_or_default();
        password_validation::install_policy(policy);
        // Install the rate limiter the same way: the route handlers are free
        // functions, so they read the limiter ambiently via the `throttle`
        // free helpers. First boot wins (idempotent set), matching the
        // password-policy / ambient-pool contract.
        throttle::install(throttle::AuthThrottle::from_config(self.throttle_config));
        Ok(())
    }
}

// =========================================================================
// AuthError
// =========================================================================

/// Errors the auth helpers can produce. Kept narrow at M9 v1 so the
/// surface is easy to handle in one match arm.
#[derive(Debug)]
pub enum AuthError {
    /// argon2 produced or failed to parse a password hash. Carries the
    /// raw error so the diagnostic includes argon2's own message.
    PasswordHash(argon2::password_hash::Error),
    /// sqlx error executing one of the helper queries.
    Sqlx(sqlx::Error),
    /// ORM write error — `create`, `update_values`, etc.
    Write(umbra::orm::write::WriteError),
    /// `authenticate` was called with credentials that don't match any
    /// active user. Returned for both "no such user" and "wrong
    /// password" so a caller can't tell which from the error alone.
    InvalidCredentials,
    /// The plaintext password failed one or more password-strength
    /// validators (see [`crate::password_validation`]). Carries every
    /// human-readable reason so the route / form can show the full list.
    ///
    /// This is NOT produced by the low-level creation helpers anymore
    /// (`create_user` / `create_user_with_flags` / `create_superuser` /
    /// `set_password` are all Django-parity and do not validate). It is
    /// constructed at the **registration boundary** — the `register` route
    /// calls [`crate::validate_password`] up front and wraps any failure in
    /// this variant, which the route layer then maps to 400. A custom signup
    /// flow that wants the same behaviour follows the same pattern.
    WeakPassword(Vec<String>),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::PasswordHash(e) => write!(f, "umbra-auth: password hash: {e}"),
            AuthError::Sqlx(e) => write!(f, "umbra-auth: sqlx: {e}"),
            AuthError::Write(e) => write!(f, "umbra-auth: write: {e:?}"),
            AuthError::InvalidCredentials => write!(f, "umbra-auth: invalid credentials"),
            AuthError::WeakPassword(reasons) => {
                write!(f, "umbra-auth: password rejected: {}", reasons.join(" "))
            }
        }
    }
}

impl std::error::Error for AuthError {}

impl From<argon2::password_hash::Error> for AuthError {
    fn from(e: argon2::password_hash::Error) -> Self {
        Self::PasswordHash(e)
    }
}

impl From<sqlx::Error> for AuthError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

impl From<umbra::orm::write::WriteError> for AuthError {
    fn from(e: umbra::orm::write::WriteError) -> Self {
        Self::Write(e)
    }
}

// =========================================================================
// Password helpers - pure, no DB.
// =========================================================================

/// Hash a plaintext password with argon2's framework-chosen
/// parameters. Returns the PHC-encoded string ready to store in
/// the password_hash column. The hash is self-describing so future
/// parameter upgrades stay transparent: a verified hash with old
/// parameters can be re-hashed on next login.
pub fn hash_password(plaintext: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = password_hasher()
        .hash_password(plaintext.as_bytes(), &salt)?
        .to_string();
    Ok(hash)
}

/// Verify a plaintext password against an argon2 PHC-encoded hash.
/// Returns `Ok(true)` on match, `Ok(false)` on mismatch, and an error
/// only when the hash itself is malformed. Callers that just want a
/// bool can use `.unwrap_or(false)`.
pub fn verify_password(plaintext: &str, hash: &str) -> Result<bool, AuthError> {
    let parsed = PasswordHash::new(hash)?;
    match password_hasher().verify_password(plaintext.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(e) => Err(AuthError::PasswordHash(e)),
    }
}

fn password_hasher() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(19_456, 2, 1, None).expect("hard-coded argon2 params are valid"),
    )
}

// =========================================================================
// AuthUser-specific creation helpers.
//
// These functions are intentionally tied to `AuthUser` because they
// construct the struct from a fixed set of columns. A custom user model
// that wants equivalent creation helpers should provide its own, using
// `hash_password` for the password column. See the docs for the
// recommended pattern.
// =========================================================================

/// Create a new active user with the given username, email, and
/// plaintext password. The password is hashed before insert; the
/// plaintext never touches the database. `date_joined` is set to
/// `Utc::now()`; `last_login` is `None`; `is_active = true`,
/// `is_staff = false`, `is_superuser = false`.
pub async fn create_user(
    username: &str,
    email: &str,
    plaintext: &str,
) -> Result<AuthUser, AuthError> {
    create_user_with_flags(username, email, plaintext, false, false).await
}

/// Create a superuser - `is_staff = true`, `is_superuser = true`,
/// `is_active = true`. Used by the `createsuperuser` management
/// command and available directly for tests / seed scripts.
pub async fn create_superuser(
    username: &str,
    email: &str,
    plaintext: &str,
) -> Result<AuthUser, AuthError> {
    // Low-level, like every other creation helper: it inserts a row and
    // does NOT run the password-strength policy. This is Django parity —
    // `User.objects.create_superuser()` doesn't validate either; only the
    // registration boundary (the `register` route) and any custom signup
    // form do. A trusted operator path (the `createsuperuser` command, a
    // seed script, a test) chooses the password deliberately, so there's
    // nothing to gate here.
    insert_user(username, email, plaintext, true, true).await
}

/// Insert a new user with arbitrary `is_staff` / `is_superuser`
/// flags. Used by `create_user` (flags = false, false) and
/// `create_superuser` (flags = true, true); exposed publicly so
/// custom seed paths can pick a specific shape (e.g. a staff-but-
/// not-superuser editor account).
pub async fn create_user_with_flags(
    username: &str,
    email: &str,
    plaintext: &str,
    is_staff: bool,
    is_superuser: bool,
) -> Result<AuthUser, AuthError> {
    insert_user(username, email, plaintext, is_staff, is_superuser).await
}

/// The shared insert path behind [`create_user`], [`create_user_with_flags`]
/// and [`create_superuser`].
///
/// This is the **low-level** creation primitive: it hashes the plaintext and
/// writes the row, but it does NOT run the password-strength policy. That's
/// deliberate Django parity — `User.objects.create_user()` doesn't validate;
/// the registration boundary does (in umbra, the `register` route, which calls
/// [`validate_password`] itself before reaching here). Keeping validation out
/// of the insert path means seed scripts, bulk imports, and the workspace test
/// suite can create users with deliberately-chosen passwords without tripping
/// the policy. An untrusted signup surface must gate on `validate_password`
/// up front; the helper trusts its caller.
async fn insert_user(
    username: &str,
    email: &str,
    plaintext: &str,
    is_staff: bool,
    is_superuser: bool,
) -> Result<AuthUser, AuthError> {
    let now = chrono::Utc::now();
    let hash = hash_password(plaintext)?;
    let row = AuthUser::objects()
        .create(AuthUser {
            id: 0,
            username: username.to_string(),
            email: email.to_string(),
            password_hash: hash,
            is_active: true,
            is_staff,
            is_superuser,
            date_joined: now,
            last_login: None,
        })
        .await?;
    Ok(row)
}

// =========================================================================
// Generic auth helpers - work against any UserModel.
// =========================================================================

/// Verify a username + plaintext password against the user table for
/// user model `U`. Returns the user on success; returns
/// `AuthError::InvalidCredentials` for both "no such user" and "wrong
/// password" (the same shape, so a caller can't enumerate accounts).
///
/// The query uses `U::TABLE` for the table name. The WHERE clause
/// filters on `username = ?` and `is_active = 1` (the standard column
/// name for the active flag). Custom models that store the active flag
/// under a different column name should filter directly and call
/// `verify_password` themselves.
///
/// Does not update `last_login`; that is the login-flow's job once the
/// HTTP layer is wired end-to-end.
pub async fn authenticate<U>(username: &str, plaintext: &str) -> Result<U, AuthError>
where
    U: UserModel
        + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
        + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
        + umbra::orm::HydrateRelated
        + Unpin,
{
    let user: Option<U> = umbra::orm::Manager::<U>::default()
        .filter(
            umbra::orm::Predicate::<U>::col_eq("username", username)
                & umbra::orm::Predicate::<U>::col_eq("is_active", true),
        )
        .first()
        .await?;

    let Some(user) = user else {
        return Err(AuthError::InvalidCredentials);
    };

    // Defence-in-depth: also check the trait method so custom types
    // that compute is_active dynamically (e.g. checking a TTL field)
    // are still respected even if the SQL filter passed.
    if !user.is_active() {
        return Err(AuthError::InvalidCredentials);
    }

    if verify_password(plaintext, user.password_hash())? {
        Ok(user)
    } else {
        Err(AuthError::InvalidCredentials)
    }
}

/// Replace a user's password with a fresh hash of the given plaintext.
/// Writes through to the database using `U::TABLE`. `user.password_hash`
/// is updated in place on success so the caller can keep using the same
/// value.
pub async fn set_password<U>(user: &mut U, plaintext: &str) -> Result<(), AuthError>
where
    U: UserModel,
{
    // Low-level, like `create_user`: this rotates the stored hash and does
    // NOT run the password-strength policy. Validation belongs at the
    // boundary — a password-change route or form should call
    // `validate_password` (with whatever user context it has) BEFORE invoking
    // `set_password`, exactly as the `register` route gates `create_user`.
    // Keeping the helper non-validating matches Django's `set_password`, which
    // is a pure setter; the form is what validates.
    let hash = hash_password(plaintext)?;
    let mut patch = serde_json::Map::new();
    patch.insert(
        "password_hash".to_string(),
        serde_json::Value::String(hash.clone()),
    );
    umbra::orm::Manager::<U>::default()
        .filter(umbra::orm::Predicate::<U>::col_eq("id", user.id()))
        .update_values(patch)
        .await?;
    user.set_password_hash(hash);
    Ok(())
}

// =========================================================================
// Management command: createsuperuser
// =========================================================================

/// `createsuperuser` - Django's interactive superuser creation,
/// dispatched via `cargo run -- createsuperuser` from any umbra
/// project that registers [`AuthPlugin`].
///
/// Prompts for username, email, and password (the password input
/// is read without terminal echo via `rpassword`). The new user
/// lands with `is_active = true`, `is_staff = true`, `is_superuser =
/// true` - the standard Django shape for the bootstrap admin account.
///
/// Flags:
///
/// - `--username <name>` - skip the username prompt.
/// - `--email <addr>` - skip the email prompt.
/// - `--noinput` - fail if any required value is missing instead of
///   prompting. Useful in CI / containers / declarative seed paths.
///   Reads password from `UMBRA_SUPERUSER_PASSWORD` when set.
#[derive(Debug, Default)]
pub struct CreateSuperuserCommand;

#[async_trait::async_trait]
impl umbra::cli::PluginCommand for CreateSuperuserCommand {
    fn command(&self) -> clap::Command {
        clap::Command::new("createsuperuser")
            .about("Create a superuser account (is_staff = is_superuser = true)")
            .arg(
                clap::Arg::new("username")
                    .long("username")
                    .help("Skip the interactive username prompt")
                    .value_name("NAME"),
            )
            .arg(
                clap::Arg::new("email")
                    .long("email")
                    .help("Skip the interactive email prompt")
                    .value_name("ADDR"),
            )
            .arg(
                clap::Arg::new("noinput")
                    .long("noinput")
                    .help(
                        "Fail rather than prompt for any missing value. \
                         Reads password from UMBRA_SUPERUSER_PASSWORD env var.",
                    )
                    .action(clap::ArgAction::SetTrue),
            )
    }

    async fn run(&self, matches: &clap::ArgMatches) -> Result<(), umbra::cli::CliError> {
        let noinput = matches.get_flag("noinput");
        let username = resolve_or_prompt(
            matches.get_one::<String>("username").cloned(),
            "Username",
            noinput,
            None,
        )?;
        let email = resolve_or_prompt(
            matches.get_one::<String>("email").cloned(),
            "Email",
            noinput,
            None,
        )?;
        let password = resolve_password(noinput)?;

        let user = create_superuser(&username, &email, &password)
            .await
            .map_err(|e| -> umbra::cli::CliError { Box::new(e) })?;
        println!(
            "Created superuser `{}` (id = {}) - is_staff = true, is_superuser = true",
            user.username, user.id,
        );
        Ok(())
    }
}

/// Get a value from the CLI flag, the env var, or the interactive
/// prompt. The `noinput` flag fails the CLI call rather than
/// prompting when no value is available.
fn resolve_or_prompt(
    cli_value: Option<String>,
    label: &str,
    noinput: bool,
    env_var: Option<&str>,
) -> Result<String, umbra::cli::CliError> {
    if let Some(v) = cli_value
        && !v.is_empty()
    {
        return Ok(v);
    }
    if let Some(key) = env_var
        && let Ok(v) = std::env::var(key)
        && !v.is_empty()
    {
        return Ok(v);
    }
    if noinput {
        return Err(
            format!("umbra createsuperuser: {label} not provided and --noinput is set").into(),
        );
    }
    print!("{label}: ");
    use std::io::Write;
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    let v = s.trim().to_string();
    if v.is_empty() {
        return Err(format!("umbra createsuperuser: {label} cannot be empty").into());
    }
    Ok(v)
}

/// Get the password - env var -> confirm-prompt with no-echo. Refuses
/// to proceed when the two confirmation entries don't match.
fn resolve_password(noinput: bool) -> Result<String, umbra::cli::CliError> {
    if let Ok(v) = std::env::var("UMBRA_SUPERUSER_PASSWORD")
        && !v.is_empty()
    {
        return Ok(v);
    }
    if noinput {
        return Err(
            "umbra createsuperuser: password not provided (set UMBRA_SUPERUSER_PASSWORD) \
             and --noinput is set"
                .into(),
        );
    }
    let first = rpassword::prompt_password("Password: ")?;
    if first.is_empty() {
        return Err("umbra createsuperuser: password cannot be empty".into());
    }
    let second = rpassword::prompt_password("Password (again): ")?;
    if first != second {
        return Err("umbra createsuperuser: passwords do not match".into());
    }
    Ok(first)
}
