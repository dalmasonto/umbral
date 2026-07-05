//! umbral-auth — the built-in authentication plugin.
//!
//! The first crate under `plugins/` and the proof of the M7 plugin
//! contract: a real built-in expressed through `umbral::prelude::Plugin`
//! with no special-casing inside `umbral-core`. Auth is the most common
//! plugin, so getting it right here also pressure-tests the
//! contract for the rest.
//!
//! ## M9 v1 scope
//!
//! - [`AuthUser`] model: the canonical User model (username,
//!   email, password hash, `is_active` / `is_staff` / `is_superuser`,
//!   `date_joined`, `last_login`).
//! - [`UserModel`] trait: the minimum surface a custom user model must
//!   satisfy so `AuthPlugin<U>` can swap in any user type. Default impls
//!   cover the optional flag methods so a minimal custom user struct
//!   only has to implement the load-bearing four.
//! - argon2 password hashing via [`hash_password`] / [`verify_password`].
//! - [`create_user`], [`authenticate`], [`set_password`] helpers.
//!   `authenticate` and `set_password` are generic over any `U: UserModel`.
//! - [`AuthPlugin`] registers the user model (which becomes a migration)
//!   plus the `/auth` routes and management commands. The type parameter
//!   defaults to [`AuthUser`] so existing apps need no changes.
//! - [`login_required`] module: `LoginRequired` config, `LoggedIn<U>`
//!   extractor, `LoginRequiredLayer` middleware, and the
//!   `login_required()` / `login_required_html()` convenience
//!   constructors. A login-required gate in two shapes.
//!
//! ## Custom user models
//!
//! ```ignore
//! // 1. Declare a custom user struct.
//! #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
//! pub struct TenantUser {
//!     pub id: i64,
//!     pub username: String,
//!     pub password_hash: String,
//!     pub tenant_id: i64,
//!     pub is_active: bool,
//! }
//!
//! // 2. Implement UserModel (only the four required methods).
//! impl umbral_auth::UserModel for TenantUser {
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
//!   `umbral-sessions` session middleware wired end-to-end.
//! - Periodic session cleanup via `umbral-tasks`.

pub mod auth_routes;
pub mod bearer_auth;
pub mod challenge;
pub mod extractors;
pub mod form_routes;
pub mod login_required;
pub mod mailer;
pub mod password_validation;
pub mod session_user;
pub mod throttle;
pub mod token;

pub use mailer::{AuthMailError, AuthMailer, ConsoleMailer, MailKind, OutgoingMail};
pub use password_validation::{
    CommonPasswordValidator, MinLengthValidator, NumericPasswordValidator, PasswordContext,
    PasswordPolicy, PasswordValidator, UserAttributeSimilarityValidator, validate_password,
};

pub use bearer_auth::{BearerAuthentication, parse_bearer_header};
pub use challenge::{
    AuthChallenge, reset_password, start_email_verification, start_password_reset, verify_email,
};
pub use extractors::{CurrentIdentity, OptionalIdentity, resolve_identity};
pub use login_required::{
    LoggedIn, LoginRequired, LoginRequiredLayer, current_session_user_id, current_session_user_pk,
    login_required, login_required_html, resolve_user as current_user_as,
};
pub use session_user::{
    OptionalUser, SessionAuthentication, User, current_user, login, login_with_request,
    user_context_layer,
};
pub use throttle::{
    Throttle, ThrottleConfig, email_action_throttle_check, login_throttle_check,
    login_throttle_clear, register_throttle_check,
};
pub use token::{AuthToken, PlaintextToken, TOKEN_PREFIX, digest_token};

/// Test shim: thin wrapper over `auth_routes::openapi_paths` so test binaries
/// (which can't reach into `pub(crate)`) can assert the full path list.
#[doc(hidden)]
pub fn auth_routes_openapi_for_test(prefix: &str) -> Vec<(String, serde_json::Value)> {
    auth_routes::openapi_paths(prefix)
}

use std::marker::PhantomData;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version, password_hash::rand_core::OsRng};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbral::prelude::*;

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
///          umbral::orm::Model)]
/// pub struct UuidUser {
///     pub id: uuid::Uuid,
///     pub username: String,
///     pub password_hash: String,
///     pub is_active: bool,
///     pub is_staff: bool,
/// }
/// impl umbral_auth::UserModel for UuidUser {
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

    /// The PK as a string. Used by [`umbral_sessions`] (which stores
    /// `user_id` as text) and by the REST identity contract's
    /// [`Identity::user_id`](umbral::auth::Identity) (which is
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
/// doesn't yet accept `#[umbral(table = ...)]` so the snake_case
/// round-trip is the only way to get a plugin-prefixed table name
/// until the attribute lands.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct AuthUser {
    pub id: i64,
    #[umbral(unique)]
    pub username: String,
    /// Shown read-only on edit forms; never on create forms (use the
    /// admin's password field mechanism for changes).
    #[umbral(noedit, unique)]
    pub email: String,
    /// Never shown on any form — password management goes through the
    /// dedicated Change Password flow in the admin. `signal_skip` keeps the
    /// hash out of every ORM signal payload (audit_2 core-app-config #10), so
    /// an audit-log subscriber can't copy password hashes into its logs.
    #[umbral(noform, signal_skip)]
    pub password_hash: String,
    pub is_active: bool,
    /// Staff flag — grants admin-site access. Privileged: the untrusted JSON
    /// write path (REST create/update, admin form-submit) refuses to set it
    /// unless the caller authorizes it via `DynQuerySet::allow_privileged`
    /// (audit_2 H3). Prevents a self-service `POST /users {is_staff: true}`
    /// privilege escalation. An admin acting as a superuser still toggles it.
    /// `default = "false"` so a create that had the field stripped fills the
    /// safe value at the DB rather than tripping NOT NULL.
    #[umbral(privileged, default = "false")]
    pub is_staff: bool,
    /// Superuser flag — full authority. Privileged for the same reason as
    /// `is_staff`; this is the field a mass-assignment attack most wants.
    #[umbral(privileged, default = "false")]
    pub is_superuser: bool,
    pub date_joined: DateTime<Utc>,
    pub last_login: Option<DateTime<Utc>>,
    /// When this user's email was verified, NULL until they complete the
    /// verification flow. Tracked always; only enforced when the plugin is
    /// built with `require_verified_email()`.
    pub email_verified_at: Option<DateTime<Utc>>,
}

impl UserModel for AuthUser {
    // `<AuthUser as Model>::PrimaryKey` is `i64` — the derive picks
    // it up from the `id: i64` field. Returning `self.id` directly
    // satisfies `fn id(&self) -> <Self as Model>::PrimaryKey` for
    // the default AuthUser shape; a custom user model with a
    // `uuid::Uuid` PK would return `self.id` of that type, and the
    // default `id_string()` would stringify via `Display` for free.
    fn id(&self) -> <Self as umbral::orm::Model>::PrimaryKey {
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

/// A `Mutex`-wrapped optional mailer slot that implements `Debug` manually so
/// `#[derive(Debug)]` on `AuthPlugin` keeps working even though
/// `Arc<dyn AuthMailer>` is not `Debug`.
struct MailerSlot(std::sync::Mutex<Option<std::sync::Arc<dyn mailer::AuthMailer>>>);
impl std::fmt::Debug for MailerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MailerSlot(..)")
    }
}

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
    /// When `Some`, mount the 7 POST form-action routes (login, logout,
    /// signup, verify-email, resend, password-forgot, password-reset)
    /// under this prefix. Default `None` — opt in via
    /// [`AuthPlugin::with_form_routes`] / [`AuthPlugin::with_form_routes_at`].
    /// Only settable on `AuthPlugin<AuthUser>`.
    pub form_routes_prefix: Option<String>,
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
    /// The mailer sealed into the ambient `OnceLock` on `on_ready`. Wrapped
    /// in a `Mutex` (via `MailerSlot`) so `on_ready`'s `&self` can `.take()`
    /// the value. First boot wins; subsequent calls are no-ops.
    mailer: MailerSlot,
    /// When `true`, the `register` route auto-sends a verification code and the
    /// `login` route returns 403 until `email_verified_at` is stamped. Off by
    /// default — the column is tracked and the endpoints exist regardless; only
    /// the enforcement gate is toggled here. Set via
    /// [`AuthPlugin::require_verified_email`] (available on
    /// `AuthPlugin<AuthUser>` only, since it gates the built-in routes).
    require_verified: bool,
    /// Optional override for the argon2 concurrency cap (audit_2 plugin-auth
    /// #4). `None` uses the framework default — machine parallelism (min 2),
    /// or the `UMBRAL_AUTH_HASH_CONCURRENCY` env var. Sealed at `on_ready`.
    hash_concurrency: Option<usize>,
    _u: PhantomData<U>,
}

impl<U: UserModel> Default for AuthPlugin<U> {
    fn default() -> Self {
        Self {
            user_model_name: None,
            default_routes_prefix: None,
            form_routes_prefix: None,
            user_in_templates: false,
            // SECURE BY DEFAULT: an unconfigured AuthPlugin enforces the
            // full validator set. `None` defers to PasswordPolicy::default()
            // (the secure set) at install time; it does NOT mean "off".
            password_policy: std::sync::Mutex::new(None),
            // SECURE BY DEFAULT: throttling is ON for login + register with
            // the credential-stuffing-resistant budgets above. `disable_throttle`
            // is the only path that turns it off.
            throttle_config: throttle::ThrottleConfig::default(),
            mailer: MailerSlot(std::sync::Mutex::new(None)),
            require_verified: false,
            hash_concurrency: None,
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
    /// the default validators are NOT merged in. Build the policy
    /// you want from scratch:
    ///
    /// ```ignore
    /// use umbral_auth::{AuthPlugin, PasswordPolicy, MinLengthValidator, CommonPasswordValidator};
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

    /// Tune the email-action rate limit: `max` attempts per trailing `window`,
    /// keyed per IP + email. Covers verify-email, resend-verification, and
    /// password-forgot. The default is 5 / hour — enough for a user who needs
    /// a couple of resends, but low enough to stop email-bombing / online
    /// code-guessing scripts dead.
    pub fn email_action_throttle(mut self, max: usize, window: std::time::Duration) -> Self {
        self.throttle_config.email_action_max = max;
        self.throttle_config.email_action_window = window;
        self
    }

    /// Explicit opt-OUT: turn login, register, and email-action throttling OFF
    /// entirely. Secure-by-default means an app that genuinely wants no rate
    /// limit — a load test, an internal tool behind its own gateway limiter —
    /// has to ask for it by name. Don't reach for this to silence a throttled
    /// test; use a distinct IP/username per attempt or generous budget methods
    /// instead.
    pub fn disable_throttle(mut self) -> Self {
        self.throttle_config.enabled = false;
        self
    }

    /// Cap how many argon2 hash/verify operations may run concurrently
    /// (audit_2 plugin-auth #4). Each argon2id op allocates ~19 MiB and pins a
    /// CPU, so without a bound a login/register/reset flood can spawn hundreds
    /// at once and OOM the process. The default is the machine's parallelism
    /// (min 2) — more concurrent hashes than cores only thrashes and multiplies
    /// peak memory. Requests past `cap × 8` in-flight (running + waiting) are
    /// shed with HTTP 503 so clients back off. Override only if you have a
    /// specific reason (e.g. reserving cores for request handling).
    ///
    /// `UMBRAL_AUTH_HASH_CONCURRENCY` overrides this at runtime; a `0` here is
    /// ignored (the default applies).
    pub fn hash_concurrency(mut self, cap: usize) -> Self {
        self.hash_concurrency = Some(cap);
        self
    }

    /// Wire the mailer used by the verification + password-reset flows.
    /// Pass a type implementing [`AuthMailer`] or an async closure
    /// `|mail| async { ... }`. Unset → [`ConsoleMailer`] (stderr in dev).
    ///
    /// ```ignore
    /// AuthPlugin::<AuthUser>::default().mailer(|m: OutgoingMail| async move {
    ///     umbral_email::send(&umbral_email::EmailMessage::new(m.subject, vec![m.to])
    ///         .html_body(m.html).text_body(m.text)).await
    ///         .map(|_| ()).map_err(|e| AuthMailError::Send(e.to_string()))
    /// })
    /// ```
    pub fn mailer(self, m: impl mailer::AuthMailer + 'static) -> Self {
        *self.mailer.0.lock().expect("mailer slot poisoned") = Some(std::sync::Arc::new(m));
        self
    }

    /// Resolve the JSON route prefix.
    ///
    /// Returns `None` when `with_default_routes[_at]` was not called (no
    /// routes mounted). When the stored value equals `JSON_PREFIX_SENTINEL`
    /// (set by `with_default_routes()`), returns `{api_base()}/auth` —
    /// resolved at call-time, after `App::build` has had a chance to set the
    /// base. A literal prefix stored by `with_default_routes_at` is returned
    /// as-is.
    ///
    /// Private: called from the `Plugin` trait impl (`routes`,
    /// `route_paths`, `openapi_paths`). Not part of the public API.
    fn json_prefix(&self) -> Option<String> {
        self.default_routes_prefix.as_ref().map(|p| {
            if p == JSON_PREFIX_SENTINEL {
                format!("{}/auth", umbral::web::api_base())
            } else {
                p.clone()
            }
        })
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

// =========================================================================
// Ambient require_verified seal — mirrors the password policy / mailer pattern.
// =========================================================================

/// Process-global flag set once in `on_ready`. Handlers read it as a free
/// function so they don't need a handle to `AuthPlugin<U>`.
static REQUIRE_VERIFIED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether the `require_verified_email()` builder was called on the active
/// `AuthPlugin`. `false` until `on_ready` seals it; `false` as the fallback
/// if `on_ready` was somehow skipped (should never happen in a well-formed
/// `App::build`, but safe-default matters here — off = permissive).
pub(crate) fn verified_email_required() -> bool {
    *REQUIRE_VERIFIED.get().unwrap_or(&false)
}

/// Stored by `with_default_routes()` so the JSON prefix can be resolved at
/// build time (when `api_base()` is already set by `App::build`) rather than
/// when the builder method is called (before `App::build` has set the base).
/// An internal null-byte sentinel that no real path can equal.
const JSON_PREFIX_SENTINEL: &str = "\0auto-api-base\0";

impl AuthPlugin<AuthUser> {
    /// Mount the built-in `/api/auth/{register,login,logout,me,…}`
    /// surface. Same handlers that lived in the derive-demo example
    /// app, promoted to the framework so every app gets them with one
    /// line. JSON-only; UNIQUE-violation → 409; login returns both a
    /// Set-Cookie and a bearer token in one response so browsers and
    /// CLI clients share an endpoint.
    ///
    /// The prefix resolves at build time: `{api_base()}/auth`, so it
    /// follows whatever base the REST plugin set (default `/api/auth`).
    /// Use [`Self::with_default_routes_at`] to fix a literal prefix.
    pub fn with_default_routes(mut self) -> Self {
        // Store the sentinel; `json_prefix()` resolves it at call-time
        // (which is during `App::build` → `Plugin::routes`), after the
        // REST plugin has had a chance to call `set_api_base`.
        self.default_routes_prefix = Some(JSON_PREFIX_SENTINEL.to_string());
        self
    }

    /// Same as [`Self::with_default_routes`] but the prefix is yours
    /// to pick. Useful when `/api/auth` collides with an existing
    /// surface or you want versioning (`/v1/auth`).
    pub fn with_default_routes_at(mut self, prefix: impl Into<String>) -> Self {
        self.default_routes_prefix = Some(prefix.into());
        self
    }

    /// Block login until the user's `email_verified_at` column is stamped, and
    /// auto-send a verification code immediately on `register`. Off by default
    /// — the `email_verified_at` column is always tracked and the
    /// `/verify-email` + `/resend-verification` endpoints are always mounted;
    /// this flag only controls enforcement:
    ///
    /// - **register**: after a successful `create_user`, fires
    ///   `start_email_verification` best-effort (a mail failure does NOT fail
    ///   registration; it is logged at `warn` level). The `201` response is
    ///   unchanged.
    /// - **login**: after `authenticate` succeeds and before minting the
    ///   bearer token / session, checks `email_verified_at IS NULL`; returns
    ///   `403 {error: "email_not_verified"}` if so.
    ///
    /// Available only on `AuthPlugin<AuthUser>` because enforcement is
    /// implemented inside the built-in handlers (which are `AuthUser`-only).
    /// Custom user models bring their own routes and their own enforcement.
    ///
    /// Requires a working mailer in production — wire
    /// [`AuthPlugin::mailer`] alongside this builder, or users won't receive
    /// the verification code and will be permanently locked out:
    ///
    /// ```ignore
    /// AuthPlugin::<AuthUser>::default()
    ///     .with_default_routes()
    ///     .mailer(my_smtp_mailer)
    ///     .require_verified_email()
    /// ```
    pub fn require_verified_email(mut self) -> Self {
        self.require_verified = true;
        self
    }

    /// Mount the 7 POST form-action auth routes (login, logout, signup,
    /// verify-email, resend, password-forgot, password-reset) under the
    /// default `/auth` prefix.
    ///
    /// These are the form-action **endpoints** that developer-written HTML
    /// forms POST to: `<form method="POST" action="/auth/login">`. The
    /// framework never ships the pages themselves — the developer writes
    /// those with their own brand and design.
    ///
    /// Each handler receives a form-encoded body, runs the same auth logic
    /// as the JSON surface (including throttle and enumeration-safe guards),
    /// sets a flash message via the session, then returns a 303 redirect.
    ///
    /// Use [`Self::with_form_routes_at`] to mount under a custom prefix.
    pub fn with_form_routes(mut self) -> Self {
        self.form_routes_prefix = Some("/auth".into());
        self
    }

    /// Same as [`Self::with_form_routes`] but you choose the prefix.
    ///
    /// ```ignore
    /// AuthPlugin::<AuthUser>::default().with_form_routes_at("/accounts")
    /// ```
    pub fn with_form_routes_at(mut self, prefix: impl Into<String>) -> Self {
        self.form_routes_prefix = Some(prefix.into());
        self
    }
}

impl<U: UserModel> Plugin for AuthPlugin<U> {
    fn name(&self) -> &'static str {
        "auth"
    }

    fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
        // AuthToken FKs against AuthUser specifically (FK target is
        // a concrete `Model` type, not a `UserModel`). Apps wiring
        // `AuthPlugin::<CustomUser>` get the user table migrated but
        // NOT the token table — they bring their own token model
        // and their own bearer-auth backend.
        let mut models = vec![umbral::migrate::ModelMeta::for_::<U>()];
        if std::any::TypeId::of::<U>() == std::any::TypeId::of::<AuthUser>() {
            models.push(umbral::migrate::ModelMeta::for_::<AuthToken>());
            models.push(umbral::migrate::ModelMeta::for_::<AuthChallenge>());
        }
        models
    }

    fn templates_dirs(&self) -> Vec<std::path::PathBuf> {
        // The auth plugin ships its own templates (email bodies, future
        // HTML auth forms). They live under `plugins/umbral-auth/templates/`
        // in the repo, and `CARGO_MANIFEST_DIR` resolves to that crate root
        // at compile time so the path stays correct regardless of where the
        // binary is invoked from.
        vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }

    fn commands(&self) -> Vec<Box<dyn umbral::cli::PluginCommand>> {
        vec![Box::new(CreateSuperuserCommand)]
    }

    fn routes(&self) -> umbral::web::Router {
        // `default_routes_prefix` is only ever Some when U = AuthUser
        // (the only impl block that sets it is `impl AuthPlugin<AuthUser>`).
        // So the prefix-guarded branch is dead code for any custom user
        // model — both at compile time (the builder method isn't
        // visible) and at runtime (the field stays None).
        //
        // `json_prefix()` resolves the sentinel stored by `with_default_routes()`
        // to `{api_base()}/auth` at build time, after `App::build` has
        // had a chance to set the REST base path.
        let mut r = match self.json_prefix() {
            Some(prefix) => auth_routes::build_router(&prefix),
            None => umbral::web::Router::new(),
        };
        if let Some(p) = &self.form_routes_prefix {
            r = r.merge(form_routes::build_router(p));
        }
        r
    }

    fn route_paths(&self) -> Vec<umbral::routes::RouteSpec> {
        let mut paths = match self.json_prefix() {
            Some(prefix) => auth_routes::declared_routes(&prefix),
            None => Vec::new(),
        };
        if let Some(p) = &self.form_routes_prefix {
            paths.extend(form_routes::declared_routes(p));
        }
        paths
    }

    fn openapi_paths(&self) -> Vec<(String, serde_json::Value)> {
        match self.json_prefix() {
            Some(prefix) => auth_routes::openapi_paths(&prefix),
            None => Vec::new(),
        }
    }

    /// Mount [`user_context_layer`] on the full merged router when the
    /// `user_in_templates` flag is on (see
    /// [`AuthPlugin::with_user_in_templates`]). The layer reads the
    /// session cookie, hydrates the [`AuthUser`], and pushes a
    /// `serde_json` representation into [`umbral::templates::CURRENT_USER`]
    /// for the duration of the request — every template render
    /// downstream gets `user` in its global context with no per-handler
    /// plumbing.
    ///
    /// Off by default — see the builder method's docstring for the
    /// "why" (one DB read per request, pointless for REST-only apps).
    fn wrap_router(&self, router: umbral::web::Router) -> umbral::web::Router {
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
    fn on_ready(
        &self,
        _ctx: &umbral::plugin::AppContext,
    ) -> Result<(), umbral::plugin::PluginError> {
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
        // Seal the mailer into the ambient OnceLock. If None (not configured
        // by the builder), the active_mailer() fallback supplies ConsoleMailer.
        if let Ok(mut guard) = self.mailer.0.lock() {
            if let Some(m) = guard.take() {
                crate::mailer::install_mailer(m);
            }
        }
        // Seal the verified-email enforcement flag. First boot wins (idempotent),
        // matching the password-policy / mailer / ambient-pool contract.
        let _ = REQUIRE_VERIFIED.set(self.require_verified);
        // Seal the argon2 concurrency cap BEFORE any request hashing runs, so
        // the gate's semaphore is sized from it (audit_2 plugin-auth #4). Only
        // when the builder set an explicit value; otherwise the lazy default
        // (machine parallelism / env var) applies.
        if let Some(n) = self.hash_concurrency.filter(|&n| n > 0) {
            let _ = HASH_CONCURRENCY.set(n);
        }
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
    Write(umbral::orm::write::WriteError),
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
    /// `set_password` are all low-level and do not validate). It is
    /// constructed at the **registration boundary** — the `register` route
    /// calls [`crate::validate_password`] up front and wraps any failure in
    /// this variant, which the route layer then maps to 400. A custom signup
    /// flow that wants the same behaviour follows the same pattern.
    WeakPassword(Vec<String>),
    /// A blocking task offloaded to the tokio blocking pool (argon2
    /// hashing / verification via [`hash_password_async`] /
    /// [`verify_password_async`]) failed to join — i.e. the task panicked
    /// or was cancelled. Carries the `JoinError`'s message. A panic in the
    /// hash worker is a real error, surfaced rather than swallowed.
    Runtime(String),
    /// A session-layer error surfaced through one of the auth helpers
    /// (`logout`, etc.). Carries the session error's display string so
    /// callers match a single `AuthError` type without importing
    /// `umbral_sessions::SessionError`.
    Session(String),
    /// Template rendering failed (e.g. a missing template file or a
    /// syntax error). Carries the minijinja error message.
    Template(String),
    /// The ambient mailer failed to accept the message for delivery.
    /// Carries the `AuthMailError` display string.
    Mail(String),
    /// A challenge lookup or verification failed. Returned for ALL failure
    /// arms in the verification flows (no such user, no active challenge,
    /// attempt cap reached, wrong code) so a caller can't distinguish
    /// which arm fired — prevents account enumeration.
    InvalidChallenge,
    /// The argon2 concurrency gate shed this request: too much password
    /// hashing/verification is already in flight (audit_2 plugin-auth #4).
    /// Route handlers map this to HTTP 503 so clients back off rather than
    /// the process ballooning memory under a login/register flood.
    Overloaded,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::PasswordHash(e) => write!(f, "umbral-auth: password hash: {e}"),
            AuthError::Sqlx(e) => write!(f, "umbral-auth: sqlx: {e}"),
            AuthError::Write(e) => write!(f, "umbral-auth: write: {e:?}"),
            AuthError::InvalidCredentials => write!(f, "umbral-auth: invalid credentials"),
            AuthError::WeakPassword(reasons) => {
                write!(f, "umbral-auth: password rejected: {}", reasons.join(" "))
            }
            AuthError::Runtime(msg) => write!(f, "umbral-auth: blocking task failed: {msg}"),
            AuthError::Session(msg) => write!(f, "umbral-auth: session: {msg}"),
            AuthError::Template(msg) => write!(f, "umbral-auth: template: {msg}"),
            AuthError::Mail(msg) => write!(f, "umbral-auth: mail: {msg}"),
            AuthError::InvalidChallenge => write!(f, "umbral-auth: invalid or expired challenge"),
            AuthError::Overloaded => {
                write!(
                    f,
                    "umbral-auth: password-hashing capacity exceeded (try again)"
                )
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

impl From<umbral::orm::write::WriteError> for AuthError {
    fn from(e: umbral::orm::write::WriteError) -> Self {
        Self::Write(e)
    }
}

// =========================================================================
// Logout helper — single reusable logout for both built-in surfaces and
// any custom handler.
// =========================================================================

/// Log the current request's user out: destroy the session row and emit a
/// clearing Set-Cookie on `resp`.
///
/// This is the single reusable logout — both built-in surfaces (the JSON
/// `/auth/logout` route, the HTML auth forms) and any custom handler call
/// this rather than reaching for `umbral_sessions::logout` directly.
///
/// Does NOT revoke bearer tokens (those are explicit-revoke; use
/// [`crate::token::AuthToken::revoke`]).
///
/// # Errors
///
/// Returns [`AuthError::Session`] if the underlying session destruction
/// fails (e.g. DB unreachable). The clearing Set-Cookie is still written
/// to `resp` by `umbral_sessions::logout` before the error is returned, so
/// the client-side cookie is cleared even on failure.
pub async fn logout(
    req: &umbral::web::HeaderMap,
    resp: &mut umbral::web::HeaderMap,
) -> Result<(), AuthError> {
    umbral_sessions::logout(req, resp)
        .await
        .map_err(|e| AuthError::Session(e.to_string()))
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

// ── Argon2 concurrency gate (audit_2 plugin-auth #4) ─────────────────────────
//
// Each argon2id hash/verify allocates ~19 MiB and pins a CPU for ~100 ms.
// `spawn_blocking` alone bounds nothing: tokio's blocking pool defaults to 512
// threads, so a login/register/reset flood (e.g. distinct usernames that slip
// past the per-IP throttle) can run hundreds of hashes at once — 512 × 19 MiB
// ≈ 10 GB — and OOM the process. The gate caps CONCURRENT argon2 work so peak
// memory is bounded to `cap × 19 MiB`.
//
// The permit is acquired BEFORE `spawn_blocking`, so a waiting request holds
// only its plaintext `String`, not the 19-MiB argon2 buffer — waiting is cheap
// and memory stays bounded no matter how deep the queue. To also bound LATENCY
// (and stop connections piling up without limit) a second cap on total
// in-flight work (`cap × HASH_QUEUE_MULT`, running + waiting) sheds load past
// that point with [`AuthError::Overloaded`] → HTTP 503, so clients back off
// instead of hanging.

/// How many waiters-per-running-slot to admit before shedding load with 503.
/// `cap` running + `cap × (MULT-1)` waiting are admitted; the rest get 503.
const HASH_QUEUE_MULT: usize = 8;

static HASH_CONCURRENCY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
static HASH_GATE: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
static HASH_IN_FLIGHT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The maximum number of argon2 operations that may run at once. Defaults to
/// the machine's parallelism (min 2) — running more concurrent hashes than
/// cores only thrashes and multiplies peak memory for no throughput. Override
/// with the `UMBRAL_AUTH_HASH_CONCURRENCY` env var (a positive integer);
/// [`AuthPlugin::hash_concurrency`] seals a programmatic value at boot.
fn hash_concurrency() -> usize {
    *HASH_CONCURRENCY.get_or_init(|| {
        std::env::var("UMBRAL_AUTH_HASH_CONCURRENCY")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4)
                    .max(2)
            })
    })
}

fn hash_gate() -> &'static tokio::sync::Semaphore {
    HASH_GATE.get_or_init(|| tokio::sync::Semaphore::new(hash_concurrency()))
}

/// Run one CPU-bound argon2 closure on the blocking pool under the concurrency
/// gate. Sheds load with [`AuthError::Overloaded`] once total in-flight work
/// exceeds `cap × HASH_QUEUE_MULT`; otherwise waits for a permit (cheaply) and
/// runs `f` on `spawn_blocking`.
async fn with_hash_gate<F, T>(f: F) -> Result<T, AuthError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    use std::sync::atomic::Ordering;

    let max_in_flight = hash_concurrency().saturating_mul(HASH_QUEUE_MULT);
    // Reserve a slot; reject immediately if the bounded queue is full.
    let prev = HASH_IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    if prev >= max_in_flight {
        HASH_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        return Err(AuthError::Overloaded);
    }
    // Ensure the counter is decremented on every exit path.
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            HASH_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _guard = Guard;

    // Wait for one of `cap` permits — cheap: only a String is held meanwhile.
    let _permit = hash_gate()
        .acquire()
        .await
        .map_err(|e| AuthError::Runtime(e.to_string()))?;
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AuthError::Runtime(e.to_string()))
}

/// Async wrapper around [`hash_password`] that runs the CPU-bound argon2
/// work on tokio's blocking pool via `spawn_blocking`, under the concurrency
/// gate (see above). argon2id with the framework parameters takes ~100ms of
/// CPU; calling it directly from a request handler pins an async worker thread
/// for that whole time, so a login/registration burst starves the runtime and
/// HTTP/1.1 connections hang. Offloading keeps the async workers free to drive
/// other tasks. **Async request handlers must use this**; the sync
/// [`hash_password`] remains for non-async / CLI / test callers.
pub async fn hash_password_async(plaintext: &str) -> Result<String, AuthError> {
    let p = plaintext.to_owned();
    with_hash_gate(move || hash_password(&p)).await?
}

/// Async wrapper around [`verify_password`] that runs the CPU-bound argon2
/// verification on tokio's blocking pool via `spawn_blocking`, under the same
/// concurrency gate. See [`hash_password_async`] for the starvation rationale.
/// **Async request handlers must use this**; the sync [`verify_password`]
/// remains for non-async / CLI / test callers.
pub async fn verify_password_async(plaintext: &str, hash: &str) -> Result<bool, AuthError> {
    let p = plaintext.to_owned();
    let h = hash.to_owned();
    with_hash_gate(move || verify_password(&p, &h)).await?
}

fn password_hasher() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(19_456, 2, 1, None).expect("hard-coded argon2 params are valid"),
    )
}

/// A fixed, valid Argon2id hash used purely to spend the same CPU on the
/// user-lookup-miss / inactive-user paths of [`authenticate`] as a real
/// verify would. Without this, a login for an existing active username costs
/// one ~30-50 ms Argon2 verify while a login for a non-existent (or inactive)
/// username returns right after the DB SELECT — a measurable timing side
/// channel that enumerates valid usernames. Computed once, lazily.
///
/// The plaintext hashed here is irrelevant; it is never compared against a
/// real password. What matters is that the string is a well-formed PHC hash
/// so `verify_password` runs the full Argon2 KDF against it.
fn dummy_password_hash() -> &'static str {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DUMMY.get_or_init(|| {
        hash_password("umbral-timing-dummy-*").expect("hard-coded dummy hash is valid")
    })
}

/// Spend one Argon2 verify against [`dummy_password_hash`] so a lookup-miss
/// path costs the same wall-clock time as a real credential check. The result
/// is intentionally discarded; only the CPU cost matters.
async fn burn_password_verify() {
    let _ = verify_password_async("umbral-timing-burn", dummy_password_hash()).await;
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
    // does NOT run the password-strength policy. By design, the low-level
    // create_superuser doesn't validate; only the
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
/// deliberate: by design the low-level `create_user` doesn't validate;
/// the registration boundary does (in umbral, the `register` route, which calls
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
    let hash = hash_password_async(plaintext).await?;
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
            email_verified_at: None,
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
        + umbral::orm::HydrateRelated
        + Unpin,
{
    let user: Option<U> = umbral::orm::Manager::<U>::default()
        .filter(
            umbral::orm::Predicate::<U>::col_eq("username", username)
                & umbral::orm::Predicate::<U>::col_eq("is_active", true),
        )
        .first()
        .await?;

    let Some(user) = user else {
        // Constant-work miss path: run one Argon2 verify against a dummy hash so
        // an unknown username costs the same wall-clock time as a real one. Skips
        // the username-enumeration timing oracle (audit plugin-auth #2).
        burn_password_verify().await;
        return Err(AuthError::InvalidCredentials);
    };

    // Defence-in-depth: also check the trait method so custom types
    // that compute is_active dynamically (e.g. checking a TTL field)
    // are still respected even if the SQL filter passed.
    if !user.is_active() {
        // Same constant-work reasoning as the lookup-miss branch above: an
        // inactive account must not be distinguishable by response latency.
        burn_password_verify().await;
        return Err(AuthError::InvalidCredentials);
    }

    if verify_password_async(plaintext, user.password_hash()).await? {
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
    // Keeping the helper non-validating makes `set_password` a pure setter;
    // the form is what validates.
    let hash = hash_password_async(plaintext).await?;
    let mut patch = serde_json::Map::new();
    patch.insert(
        "password_hash".to_string(),
        serde_json::Value::String(hash.clone()),
    );
    umbral::orm::Manager::<U>::default()
        .filter(umbral::orm::Predicate::<U>::col_eq("id", user.id()))
        .update_values(patch)
        .await?;
    user.set_password_hash(hash);
    Ok(())
}

// =========================================================================
// Management command: createsuperuser
// =========================================================================

/// `createsuperuser` - interactive superuser creation,
/// dispatched via `cargo run -- createsuperuser` from any umbral
/// project that registers [`AuthPlugin`].
///
/// Prompts for username, email, and password (the password input
/// is read without terminal echo via `rpassword`). The new user
/// lands with `is_active = true`, `is_staff = true`, `is_superuser =
/// true` - the standard shape for the bootstrap admin account.
///
/// Flags:
///
/// - `--username <name>` - skip the username prompt.
/// - `--email <addr>` - skip the email prompt.
/// - `--noinput` - fail if any required value is missing instead of
///   prompting. Useful in CI / containers / declarative seed paths.
///   Reads password from `UMBRAL_SUPERUSER_PASSWORD` when set.
#[derive(Debug, Default)]
pub struct CreateSuperuserCommand;

#[async_trait::async_trait]
impl umbral::cli::PluginCommand for CreateSuperuserCommand {
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
                         Reads password from UMBRAL_SUPERUSER_PASSWORD env var.",
                    )
                    .action(clap::ArgAction::SetTrue),
            )
    }

    async fn run(&self, matches: &clap::ArgMatches) -> Result<(), umbral::cli::CliError> {
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
            .map_err(|e| -> umbral::cli::CliError { Box::new(e) })?;
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
) -> Result<String, umbral::cli::CliError> {
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
            format!("umbral createsuperuser: {label} not provided and --noinput is set").into(),
        );
    }
    print!("{label}: ");
    use std::io::Write;
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    let v = s.trim().to_string();
    if v.is_empty() {
        return Err(format!("umbral createsuperuser: {label} cannot be empty").into());
    }
    Ok(v)
}

/// Get the password - env var -> confirm-prompt with no-echo. Refuses
/// to proceed when the two confirmation entries don't match.
fn resolve_password(noinput: bool) -> Result<String, umbral::cli::CliError> {
    if let Ok(v) = std::env::var("UMBRAL_SUPERUSER_PASSWORD")
        && !v.is_empty()
    {
        return Ok(v);
    }
    if noinput {
        return Err(
            "umbral createsuperuser: password not provided (set UMBRAL_SUPERUSER_PASSWORD) \
             and --noinput is set"
                .into(),
        );
    }
    let first = rpassword::prompt_password("Password: ")?;
    if first.is_empty() {
        return Err("umbral createsuperuser: password cannot be empty".into());
    }
    let second = rpassword::prompt_password("Password (again): ")?;
    if first != second {
        return Err("umbral createsuperuser: passwords do not match".into());
    }
    Ok(first)
}

#[cfg(test)]
mod timing_tests {
    use super::*;

    /// The constant-work miss path (audit plugin-auth #2) is only real if the
    /// dummy hash is a well-formed Argon2id PHC string — otherwise
    /// `verify_password` errors out early instead of spending the KDF cost,
    /// re-opening the timing oracle. Assert the dummy is a valid hash and that a
    /// verify against it actually runs the KDF (returns Ok(false), not Err).
    #[test]
    fn dummy_hash_is_valid_argon2id_so_miss_path_spends_kdf() {
        let h = dummy_password_hash();
        assert!(
            h.starts_with("$argon2id$"),
            "dummy hash must be Argon2id PHC, got {h}"
        );
        // A real verify runs against it; a wrong password yields Ok(false),
        // which means the full KDF executed (an invalid hash would be Err).
        assert!(!verify_password("not-the-dummy", h).unwrap());
    }
}
