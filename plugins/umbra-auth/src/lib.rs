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
//! - argon2 password hashing via [`hash_password`] / [`verify_password`].
//! - [`create_user`], [`authenticate`], [`set_password`] helpers.
//! - [`AuthPlugin`] registers the [`AuthUser`] model and contributes
//!   one system check.
//!
//! ## Deferred (per `docs/specs/outlines/auth-and-sessions.md`)
//!
//! - Custom user model swap (the `UserProvider` associated-type
//!   mechanism). M9 v1 ships exactly one user model.
//! - Permissions, groups, the auth-backend chain. The deep spec
//!   promotes from the outline when these land.
//! - The `Auth<U>` request extractor + `#[login_required]`
//!   middleware. Needs `Plugin::middleware()` lifted (M7 deferral).
//! - Login / logout / password-reset HTTP flows. Needs
//!   `umbra-sessions` and `umbra-email`, both not yet built.
//! - `umbra-sessions` itself — sessions plugin lands separately.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Argon2, password_hash::rand_core::OsRng};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbra::prelude::*;

/// The canonical authentication user. `#[derive(Model)]` snake_cases
/// the struct name into the table name `auth_user`; the M3 derive
/// doesn't yet accept `#[umbra(table = ...)]` so the snake_case
/// round-trip is the only way to get a plugin-prefixed table name
/// until the attribute lands.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
pub struct AuthUser {
    pub id: i64,
    pub username: String,
    pub email: String,
    pub password_hash: String,
    pub is_active: bool,
    pub is_staff: bool,
    pub is_superuser: bool,
    pub date_joined: DateTime<Utc>,
    pub last_login: Option<DateTime<Utc>>,
}

/// The built-in authentication plugin. Registers the [`AuthUser`]
/// model and contributes one system check (the password-hash
/// algorithm is reachable at boot).
#[derive(Debug, Default)]
pub struct AuthPlugin;

impl Plugin for AuthPlugin {
    fn name(&self) -> &'static str {
        "auth"
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        vec![umbra::migrate::ModelMeta::for_::<AuthUser>()]
    }
}

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

/// Hash a plaintext password with argon2's framework-chosen
/// parameters. Returns the PHC-encoded string ready to store in
/// `auth_user.password_hash`. The hash is self-describing so future
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
    let now = chrono::Utc::now();
    let hash = hash_password(plaintext)?;
    let pool = umbra::db::pool();
    let row = sqlx::query_as::<_, AuthUser>(
        "INSERT INTO auth_user
           (username, email, password_hash, is_active, is_staff, is_superuser, date_joined, last_login)
         VALUES (?, ?, ?, 1, 0, 0, ?, NULL)
         RETURNING *",
    )
    .bind(username)
    .bind(email)
    .bind(&hash)
    .bind(now)
    .fetch_one(&pool)
    .await?;
    Ok(row)
}

/// Verify a username + plaintext password against the user table.
/// Returns the user on success; returns `AuthError::InvalidCredentials`
/// for both "no such user" and "wrong password" (the same shape, so a
/// caller can't enumerate accounts).
///
/// Does not update `last_login`; that's the login-flow's job once the
/// HTTP layer lands.
pub async fn authenticate(username: &str, plaintext: &str) -> Result<AuthUser, AuthError> {
    let pool = umbra::db::pool();
    let user: Option<AuthUser> = sqlx::query_as::<_, AuthUser>(
        "SELECT * FROM auth_user WHERE username = ? AND is_active = 1",
    )
    .bind(username)
    .fetch_optional(&pool)
    .await?;

    let Some(user) = user else {
        return Err(AuthError::InvalidCredentials);
    };

    if verify_password(plaintext, &user.password_hash)? {
        Ok(user)
    } else {
        Err(AuthError::InvalidCredentials)
    }
}

/// Replace a user's password with a fresh hash of the given plaintext.
/// Writes through to the database. `user.password_hash` is updated in
/// place on success so the caller can keep using the same value.
pub async fn set_password(user: &mut AuthUser, plaintext: &str) -> Result<(), AuthError> {
    let hash = hash_password(plaintext)?;
    let pool = umbra::db::pool();
    sqlx::query("UPDATE auth_user SET password_hash = ? WHERE id = ?")
        .bind(&hash)
        .bind(user.id)
        .execute(&pool)
        .await?;
    user.password_hash = hash;
    Ok(())
}
