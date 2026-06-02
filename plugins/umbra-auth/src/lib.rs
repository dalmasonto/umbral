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

pub mod login_required;

pub use login_required::{
    LoggedIn, LoginRequired, LoginRequiredLayer, current_session_user_id, login_required,
    login_required_html,
};

use std::marker::PhantomData;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Argon2, password_hash::rand_core::OsRng};
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
/// | `id()` | `id` | `create_session`, `set_password` WHERE |
/// | `username()` | `username` | `authenticate` SELECT, `createsuperuser` output |
/// | `password_hash()` | `password_hash` | `authenticate` verify step |
/// | `set_password_hash()` | `password_hash` | `set_password` in-place update |
///
/// ## Default methods
///
/// | Method | Default | Used by |
/// |---|---|---|
/// | `is_active()` | `true` | `authenticate` active-user gate |
/// | `is_staff()` | `false` | admin require_staff check |
/// | `is_superuser()` | `false` | permission gates |
pub trait UserModel: Model + Send + Sync + 'static {
    /// The row's integer primary key. `set_password` uses this in the
    /// UPDATE WHERE clause.
    fn id(&self) -> i64;

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
    pub username: String,
    /// Shown read-only on edit forms; never on create forms (use the
    /// admin's password field mechanism for changes).
    #[umbra(noedit)]
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
    fn id(&self) -> i64 {
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
    _u: PhantomData<U>,
}

impl<U: UserModel> Default for AuthPlugin<U> {
    fn default() -> Self {
        Self {
            user_model_name: None,
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
}

impl<U: UserModel> Plugin for AuthPlugin<U> {
    fn name(&self) -> &'static str {
        "auth"
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        vec![umbra::migrate::ModelMeta::for_::<U>()]
    }

    fn commands(&self) -> Vec<Box<dyn umbra::cli::PluginCommand>> {
        vec![Box::new(CreateSuperuserCommand)]
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
    /// `authenticate` was called with credentials that don't match any
    /// active user. Returned for both "no such user" and "wrong
    /// password" so a caller can't tell which from the error alone.
    InvalidCredentials,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::PasswordHash(e) => write!(f, "umbra-auth: password hash: {e}"),
            AuthError::Sqlx(e) => write!(f, "umbra-auth: sqlx: {e}"),
            AuthError::InvalidCredentials => write!(f, "umbra-auth: invalid credentials"),
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
    let hash = Argon2::default()
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
    match Argon2::default().verify_password(plaintext.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(e) => Err(AuthError::PasswordHash(e)),
    }
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
    create_user_with_flags(username, email, plaintext, true, true).await
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
    let now = chrono::Utc::now();
    let hash = hash_password(plaintext)?;
    let pool = umbra::db::pool();
    let row = sqlx::query_as::<_, AuthUser>(
        "INSERT INTO auth_user
           (username, email, password_hash, is_active, is_staff, is_superuser, date_joined, last_login)
         VALUES (?, ?, ?, 1, ?, ?, ?, NULL)
         RETURNING *",
    )
    .bind(username)
    .bind(email)
    .bind(&hash)
    .bind(is_staff)
    .bind(is_superuser)
    .bind(now)
    .fetch_one(&pool)
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
    U: UserModel + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Unpin,
{
    let pool = umbra::db::pool();
    let table = U::TABLE;
    let sql = format!("SELECT * FROM {table} WHERE username = ? AND is_active = 1");
    let user: Option<U> = sqlx::query_as::<_, U>(&sql)
        .bind(username)
        .fetch_optional(&pool)
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
    let hash = hash_password(plaintext)?;
    let pool = umbra::db::pool();
    let table = U::TABLE;
    let sql = format!("UPDATE {table} SET password_hash = ? WHERE id = ?");
    sqlx::query(&sql)
        .bind(&hash)
        .bind(user.id())
        .execute(&pool)
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
