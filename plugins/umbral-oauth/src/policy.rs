//! The create-or-link policy — what happens on a callback once a
//! provider has resolved an [`Identity`].
//!
//! Order (the security-sensitive part is rule 3's *verified* gate):
//!
//! 1. **Already linked** — a `SocialAccount` exists for
//!    `(provider, provider_uid)`: refresh its tokens and return its user.
//!    In connect mode, refuse if it belongs to a *different* user (that
//!    would let a logged-in attacker hijack someone's identity row).
//! 2. **Connect mode** — a logged-in user is attaching this provider:
//!    link the new identity to them.
//! 3. **Verified-email link** — the provider asserts a *verified* email
//!    matching an existing `AuthUser`: link to that user. Only when
//!    verified, so an attacker can't pre-register an unverified address
//!    to capture a future real signup.
//! 4. **Auto-create** — otherwise mint a fresh `AuthUser` (unique
//!    username; a verified email becomes the user's email, an unverified
//!    one is kept only on the social account behind a no-reply
//!    placeholder so it can't collide or be trusted).

use chrono::{Duration, Utc};
use umbral::orm::Masked;
use umbral::orm::write::WriteError;
use umbral::prelude::*;
use umbral_auth::{AuthUser, auth_user};

use crate::models::{SocialAccount, social_account};
use crate::provider::{Identity, OAuthError, TokenSet};

fn db_err(e: impl std::fmt::Display) -> OAuthError {
    OAuthError::Database(e.to_string())
}

/// Resolve (or create) the umbral user this identity should authenticate
/// as, persisting / refreshing the `SocialAccount`. `connect_user` is
/// `Some(id)` when a logged-in user is connecting a provider (vs. a
/// social login). Returns the resolved `AuthUser` id.
pub async fn resolve_user(
    provider_key: &str,
    identity: &Identity,
    tokens: &TokenSet,
    connect_user: Option<i64>,
) -> Result<i64, OAuthError> {
    // 1. Already linked?
    let existing = SocialAccount::objects()
        .filter(social_account::PROVIDER.eq(provider_key))
        .filter(social_account::PROVIDER_UID.eq(&identity.uid))
        .first()
        .await
        .map_err(db_err)?;

    if let Some(acct) = existing {
        if let Some(cu) = connect_user
            && acct.user.id() != cu
        {
            return Err(OAuthError::Provider(
                "this account is already linked to a different user".to_string(),
            ));
        }
        let user_id = acct.user.id();
        refresh_tokens(acct.id, identity, tokens).await?;
        return Ok(user_id);
    }

    // 2. Connect mode — attach to the logged-in user.
    if let Some(cu) = connect_user {
        create_social_account(cu, provider_key, identity, tokens).await?;
        return Ok(cu);
    }

    // 3. Verified-email link to an existing user.
    if identity.email_verified
        && let Some(email) = identity.email.as_deref()
    {
        let matched = AuthUser::objects()
            .filter(auth_user::EMAIL.eq(email))
            .first()
            .await
            .map_err(db_err)?;
        if let Some(user) = matched {
            create_social_account(user.id, provider_key, identity, tokens).await?;
            return Ok(user.id);
        }
    }

    // 4. Auto-create a new user.
    let user_id = create_auth_user(provider_key, identity).await?;
    create_social_account(user_id, provider_key, identity, tokens).await?;
    Ok(user_id)
}

/// Insert a `SocialAccount` linking `user_id` to this identity, with the
/// tokens sealed into `Masked` columns.
async fn create_social_account(
    user_id: i64,
    provider_key: &str,
    identity: &Identity,
    tokens: &TokenSet,
) -> Result<(), OAuthError> {
    let account = SocialAccount {
        id: 0,
        user: ForeignKey::new(user_id),
        provider: provider_key.to_string(),
        provider_uid: identity.uid.clone(),
        provider_email: identity.email.clone(),
        email_verified: identity.email_verified,
        access_token: Masked::new(tokens.access_token.clone()),
        refresh_token: tokens.refresh_token.clone().map(Masked::new),
        scopes: tokens.scopes.clone(),
        expires_at: tokens.expires_in.map(|s| Utc::now() + Duration::seconds(s)),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    SocialAccount::objects()
        .create(account)
        .await
        .map_err(db_err)?;
    Ok(())
}

/// Refresh the tokens on an existing `SocialAccount` (a re-auth rotates
/// the access token). Encryption happens in `Masked`'s Serialize, so the
/// values placed in the update map are ciphertext.
async fn refresh_tokens(
    account_id: i64,
    identity: &Identity,
    tokens: &TokenSet,
) -> Result<(), OAuthError> {
    let mut values = serde_json::Map::new();
    values.insert(
        "access_token".to_string(),
        serde_json::to_value(Masked::<String>::new(tokens.access_token.clone())).map_err(db_err)?,
    );
    // Only overwrite the refresh token when the provider issued a new one
    // — Google omits it on subsequent consents, and we don't want to wipe
    // a still-valid stored refresh token.
    if let Some(rt) = &tokens.refresh_token {
        values.insert(
            "refresh_token".to_string(),
            serde_json::to_value(Masked::<String>::new(rt.clone())).map_err(db_err)?,
        );
    }
    values.insert("scopes".to_string(), serde_json::json!(tokens.scopes));
    values.insert(
        "email_verified".to_string(),
        serde_json::json!(identity.email_verified),
    );
    SocialAccount::objects()
        .filter(social_account::ID.eq(account_id))
        .update_values(values)
        .await
        .map_err(db_err)?;
    Ok(())
}

/// Reduce a string to a username-safe slug.
fn sanitize_username(raw: &str) -> String {
    let base: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .collect();
    if base.is_empty() {
        "user".to_string()
    } else {
        base.to_lowercase()
    }
}

/// Mint a fresh `AuthUser` for a social signup. The password is set to an
/// unusable marker (`"!"`) — these accounts authenticate only through the
/// provider until the user sets a password.
///
/// Username uniqueness is enforced by the DB `UNIQUE` constraint on
/// `auth_user.username`. Rather than a SELECT-then-INSERT (which has a
/// TOCTOU race when two OAuth callbacks race for the same base name), we
/// attempt the INSERT and catch `WriteError::UniqueViolation` on the
/// `username` column, then retry with the next numeric suffix. Up to
/// `MAX_USERNAME_RETRIES` attempts are made before giving up.
async fn create_auth_user(provider_key: &str, identity: &Identity) -> Result<i64, OAuthError> {
    // A verified email is safe to use as the account's unique email (rule
    // 3 already proved no existing user holds it). An unverified or
    // missing email gets a unique no-reply placeholder so it can neither
    // collide nor be trusted; the raw value still lives on the social
    // account's `provider_email`.
    let placeholder = format!("{provider_key}_{}@users.noreply.umbral", identity.uid);
    let email = if identity.email_verified {
        identity.email.clone().unwrap_or(placeholder)
    } else {
        placeholder
    };

    let base = sanitize_username(
        &identity
            .email
            .as_deref()
            .filter(|_| identity.email_verified)
            .and_then(|e| e.split('@').next())
            .map(str::to_string)
            .or_else(|| identity.display_name.clone())
            .unwrap_or_else(|| format!("{provider_key}_{}", identity.uid)),
    );

    // Attempt INSERT, retrying on username UNIQUE collision by appending a
    // numeric suffix (base → base1 → base2 → …). The DB constraint is the
    // authoritative uniqueness check, eliminating the TOCTOU race that a
    // SELECT-before-INSERT would carry.
    const MAX_USERNAME_RETRIES: u32 = 20;
    let mut n = 0u32;
    loop {
        let candidate = if n == 0 {
            base.clone()
        } else {
            format!("{base}{n}")
        };

        let user = AuthUser {
            id: 0,
            username: candidate,
            email: email.clone(),
            password_hash: "!".to_string(),
            is_active: true,
            is_staff: false,
            is_superuser: false,
            date_joined: Utc::now(),
            last_login: Some(Utc::now()),
            email_verified_at: None,
        };

        match AuthUser::objects().create(user).await {
            Ok(created) => return Ok(created.id),
            Err(WriteError::UniqueViolation { field, .. })
                if field.as_deref() == Some("username") || field.is_none() =>
            {
                n += 1;
                if n > MAX_USERNAME_RETRIES {
                    return Err(OAuthError::Database(format!(
                        "could not find a unique username after {MAX_USERNAME_RETRIES} attempts \
                         (base: {base})"
                    )));
                }
                // Try next suffix.
            }
            Err(e) => return Err(db_err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::OnceCell;
    use umbral::orm::{MaskKeyring, set_mask_keyring};

    fn identity(uid: &str, email: Option<&str>, verified: bool) -> Identity {
        Identity {
            uid: uid.to_string(),
            email: email.map(str::to_string),
            email_verified: verified,
            display_name: Some("Test User".to_string()),
        }
    }

    fn tokens() -> TokenSet {
        TokenSet {
            access_token: "access-123".to_string(),
            refresh_token: Some("refresh-456".to_string()),
            expires_in: Some(3600),
            scopes: "openid email".to_string(),
        }
    }

    static BOOT: OnceCell<()> = OnceCell::const_new();
    async fn boot() {
        BOOT.get_or_init(|| async {
            let (public, secret) = MaskKeyring::generate();
            set_mask_keyring(MaskKeyring::from_base64(&public, Some(&secret)).unwrap());

            let pool = umbral::db::connect_sqlite("sqlite::memory:").await.unwrap();
            let mut settings = umbral::Settings::from_env().unwrap();
            settings.database_url = "sqlite::memory:".to_string();
            umbral::App::builder()
                .settings(settings)
                .database("default", pool.clone())
                .model::<AuthUser>()
                .model::<SocialAccount>()
                .build()
                .unwrap();
            sqlx::query(
                "CREATE TABLE auth_user (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    username TEXT NOT NULL UNIQUE,
                    email TEXT NOT NULL UNIQUE,
                    password_hash TEXT NOT NULL,
                    is_active BOOLEAN NOT NULL DEFAULT 1,
                    is_staff BOOLEAN NOT NULL DEFAULT 0,
                    is_superuser BOOLEAN NOT NULL DEFAULT 0,
                    date_joined TEXT NOT NULL,
                    last_login TEXT
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            let create_sa = format!(
                "CREATE TABLE {t} (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    user INTEGER NOT NULL,
                    provider TEXT NOT NULL,
                    provider_uid TEXT NOT NULL,
                    provider_email TEXT,
                    email_verified BOOLEAN NOT NULL DEFAULT 0,
                    access_token TEXT NOT NULL,
                    refresh_token TEXT,
                    scopes TEXT NOT NULL,
                    expires_at TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE (provider, provider_uid)
                )",
                t = SocialAccount::TABLE
            );
            sqlx::query(&create_sa).execute(&pool).await.unwrap();
        })
        .await;
    }

    async fn seed_user(username: &str, email: &str) -> i64 {
        AuthUser::objects()
            .create(AuthUser {
                id: 0,
                username: username.to_string(),
                email: email.to_string(),
                password_hash: "x".to_string(),
                is_active: true,
                is_staff: false,
                is_superuser: false,
                date_joined: Utc::now(),
                last_login: None,
                email_verified_at: None,
            })
            .await
            .unwrap()
            .id
    }

    // Rule 4: a brand-new social login with no email match auto-creates a
    // user and links the social account with sealed tokens.
    #[tokio::test]
    async fn social_login_auto_creates_user_and_links_account() {
        boot().await;
        let id = identity("google-uid-A", Some("newperson@example.com"), true);
        let user_id = resolve_user("google", &id, &tokens(), None).await.unwrap();

        let user = AuthUser::objects()
            .filter(auth_user::ID.eq(user_id))
            .first()
            .await
            .unwrap()
            .unwrap();
        // Verified email becomes the account email.
        assert_eq!(user.email, "newperson@example.com");

        let acct = SocialAccount::objects()
            .filter(social_account::PROVIDER_UID.eq("google-uid-A"))
            .first()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(acct.user.id(), user_id);
        // Token round-trips through the Masked column.
        assert_eq!(acct.access_token.reveal().unwrap(), "access-123");
        assert_eq!(
            acct.refresh_token.as_ref().unwrap().reveal().unwrap(),
            "refresh-456"
        );
    }

    // Rule 3: a verified email matching an existing user links to them —
    // no new user is created.
    #[tokio::test]
    async fn verified_email_links_to_existing_user() {
        boot().await;
        let existing = seed_user("ada", "ada@verified.com").await;
        let id = identity("github-uid-B", Some("ada@verified.com"), true);
        let resolved = resolve_user("github", &id, &tokens(), None).await.unwrap();
        assert_eq!(
            resolved, existing,
            "linked to the existing user, not a new one"
        );
    }

    // Rule 3 gate: an UNVERIFIED email matching an existing user must NOT
    // link — it auto-creates a separate user instead (anti-takeover).
    #[tokio::test]
    async fn unverified_email_does_not_link() {
        boot().await;
        let existing = seed_user("grace", "grace@verified.com").await;
        let id = identity("github-uid-C", Some("grace@verified.com"), false);
        let resolved = resolve_user("github", &id, &tokens(), None).await.unwrap();
        assert_ne!(
            resolved, existing,
            "unverified email must not hijack the account"
        );
    }

    // Rule 2: connect mode attaches the provider to the logged-in user.
    #[tokio::test]
    async fn connect_mode_links_to_logged_in_user() {
        boot().await;
        let me = seed_user("connector", "connector@example.com").await;
        let id = identity("google-uid-D", Some("other@example.com"), true);
        let resolved = resolve_user("google", &id, &tokens(), Some(me))
            .await
            .unwrap();
        assert_eq!(resolved, me);
        let acct = SocialAccount::objects()
            .filter(social_account::PROVIDER_UID.eq("google-uid-D"))
            .first()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(acct.user.id(), me);
    }

    // Rule 1: re-authenticating the same identity returns the same user
    // and refreshes the stored access token in place (no duplicate row).
    #[tokio::test]
    async fn reauth_updates_tokens_without_duplicating() {
        boot().await;
        let first = resolve_user(
            "google",
            &identity("google-uid-E", Some("repeat@example.com"), true),
            &tokens(),
            None,
        )
        .await
        .unwrap();

        let mut newer = tokens();
        newer.access_token = "rotated-789".to_string();
        let second = resolve_user(
            "google",
            &identity("google-uid-E", Some("repeat@example.com"), true),
            &newer,
            None,
        )
        .await
        .unwrap();

        assert_eq!(first, second, "same identity → same user");
        let count = SocialAccount::objects()
            .filter(social_account::PROVIDER_UID.eq("google-uid-E"))
            .count()
            .await
            .unwrap();
        assert_eq!(count, 1, "no duplicate social account row");
        let acct = SocialAccount::objects()
            .filter(social_account::PROVIDER_UID.eq("google-uid-E"))
            .first()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(acct.access_token.reveal().unwrap(), "rotated-789");
    }

    // Retry contract: when the first candidate username is already taken, the
    // INSERT-with-retry path resolves to the next suffix rather than erroring.
    // This is the observable contract that the TOCTOU fix enforces — a
    // pre-existing row at candidate N forces a successful resolve at N+1.
    #[tokio::test]
    async fn username_collision_retries_to_next_suffix() {
        boot().await;

        // Seed a user that will occupy the base candidate.
        // The identity below derives base = "alice" (from the verified email).
        seed_user("alice", "taken_alice@seed.example.com").await;

        // A new OAuth login whose base username resolves to "alice" (taken).
        // The retry loop must succeed with "alice1".
        let id = identity(
            "google-uid-retry-01",
            Some("alice@provider.example.com"),
            true,
        );
        let user_id = create_auth_user("google", &id).await.unwrap();

        let user = AuthUser::objects()
            .filter(auth_user::ID.eq(user_id))
            .first()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            user.username, "alice1",
            "collision at base → resolved to base + suffix 1"
        );

        // A third signup with the same base also resolves cleanly (alice1 taken → alice2).
        let id2 = identity("google-uid-retry-02", Some("alice@other.example.com"), true);
        let user_id2 = create_auth_user("google", &id2).await.unwrap();

        let user2 = AuthUser::objects()
            .filter(auth_user::ID.eq(user_id2))
            .first()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            user2.username, "alice2",
            "collision at base1 → resolved to base + suffix 2"
        );
    }

    // Connect-mode safety: connecting an identity already linked to a
    // *different* user is refused.
    #[tokio::test]
    async fn connect_refuses_identity_owned_by_another_user() {
        boot().await;
        let owner = resolve_user(
            "github",
            &identity("github-uid-F", Some("owner@example.com"), true),
            &tokens(),
            None,
        )
        .await
        .unwrap();
        let attacker = seed_user("attacker", "attacker@example.com").await;
        assert_ne!(owner, attacker);
        let result = resolve_user(
            "github",
            &identity("github-uid-F", Some("owner@example.com"), true),
            &tokens(),
            Some(attacker),
        )
        .await;
        assert!(result.is_err(), "cannot connect someone else's identity");
    }
}
