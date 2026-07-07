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
//! 3. **Verified-email link** — a *trusted* provider asserts a *verified*
//!    email matching an existing `AuthUser`: link to that user. Two gates:
//!    the email must be verified (so an attacker can't pre-register an
//!    unverified address to capture a future real signup) AND the provider
//!    must be trusted to assert verification
//!    ([`OAuthProvider::trusts_verified_email`](crate::provider::OAuthProvider::trusts_verified_email)) —
//!    a custom/third-party provider is untrusted by default, so its verified
//!    claim can't take over an existing account (OAU-2).
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
///
/// `provider_trusts_verified_email` is the provider's
/// [`OAuthProvider::trusts_verified_email`](crate::provider::OAuthProvider::trusts_verified_email)
/// verdict: only when it is `true` does a provider-asserted verified email
/// participate in rule 3's auto-link (and become the new account's email in rule
/// 4). An untrusted provider's `email_verified` claim is ignored — its logins
/// always auto-create a fresh, isolated user (OAU-2).
pub async fn resolve_user(
    provider_key: &str,
    identity: &Identity,
    tokens: &TokenSet,
    connect_user: Option<i64>,
    provider_trusts_verified_email: bool,
) -> Result<i64, OAuthError> {
    // A verified email is only trustworthy if the provider itself is trusted to
    // assert it. An untrusted provider's `email_verified` is treated as false
    // throughout the policy, so it can neither link to nor seed an account.
    let trusted_verified_email = provider_trusts_verified_email && identity.email_verified;
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

    // 3. Verified-email link to an existing user — only when the provider is
    //    trusted to assert verification (OAU-2).
    if trusted_verified_email && let Some(email) = identity.email.as_deref() {
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

    // 4. Auto-create a new user AND its social account atomically. A partial
    //    failure — the `AuthUser` insert succeeds but the `SocialAccount`
    //    insert doesn't — would otherwise leave an orphan user with an unusable
    //    password occupying the verified email, blocking a later legitimate link
    //    and unable to log in (OAU-4). Both writes share one transaction, so a
    //    failure after the user insert rolls the user back too.
    create_user_with_social(provider_key, identity, tokens, trusted_verified_email).await
}

/// Build (but do not persist) the `SocialAccount` row linking `user_id` to
/// this identity, with the tokens sealed into `Masked` columns.
fn build_social_account(
    user_id: i64,
    provider_key: &str,
    identity: &Identity,
    tokens: &TokenSet,
) -> SocialAccount {
    SocialAccount {
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
    }
}

/// Insert a `SocialAccount` linking `user_id` to this identity. Used by the
/// connect-mode and verified-email-link paths, where the user already exists
/// so a single write is enough (the atomic user+social create lives in
/// [`create_user_with_social`]).
async fn create_social_account(
    user_id: i64,
    provider_key: &str,
    identity: &Identity,
    tokens: &TokenSet,
) -> Result<(), OAuthError> {
    SocialAccount::objects()
        .create(build_social_account(
            user_id,
            provider_key,
            identity,
            tokens,
        ))
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

/// Mint a fresh `AuthUser` for a social signup AND its `SocialAccount`, in
/// one transaction (OAU-4). The password is set to an unusable marker (`"!"`)
/// — these accounts authenticate only through the provider until the user
/// sets a password.
///
/// Username uniqueness is enforced by the DB `UNIQUE` constraint on
/// `auth_user.username`. Rather than a SELECT-then-INSERT (which has a TOCTOU
/// race when two OAuth callbacks race for the same base name), we attempt the
/// INSERT and catch `WriteError::UniqueViolation` on the `username` column,
/// then retry with the next numeric suffix.
///
/// The retry runs a **fresh transaction per attempt**, not one long-lived
/// transaction with a retry loop inside: on Postgres a constraint violation
/// aborts the surrounding transaction (every later statement errors until
/// rollback), so an in-place retry would need a SAVEPOINT per attempt. A
/// per-attempt transaction is simpler and correct on both backends — a
/// username collision rolls the whole attempt back and the next attempt
/// starts clean; the winning attempt commits the user and the social account
/// together. Up to `MAX_USERNAME_RETRIES` attempts are made before giving up.
async fn create_user_with_social(
    provider_key: &str,
    identity: &Identity,
    tokens: &TokenSet,
    trusted_verified_email: bool,
) -> Result<i64, OAuthError> {
    // A trusted-verified email is safe to use as the account's unique email
    // (rule 3 already proved no existing user holds it). An unverified, missing,
    // or untrusted-provider email gets a unique no-reply placeholder so it can
    // neither collide nor be trusted; the raw value still lives on the social
    // account's `provider_email`.
    let placeholder = format!("{provider_key}_{}@users.noreply.umbral", identity.uid);
    let email = if trusted_verified_email {
        identity.email.clone().unwrap_or(placeholder)
    } else {
        placeholder
    };

    let base = sanitize_username(
        &identity
            .email
            .as_deref()
            .filter(|_| trusted_verified_email)
            .and_then(|e| e.split('@').next())
            .map(str::to_string)
            .or_else(|| identity.display_name.clone())
            .unwrap_or_else(|| format!("{provider_key}_{}", identity.uid)),
    );

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

        let mut tx = umbral::db::begin().await.map_err(db_err)?;
        match AuthUser::objects().on_tx(&mut tx).create(user).await {
            Ok(created) => {
                // User inserted in this tx; the social account joins it. Any
                // failure here rolls the user back too — no orphan (OAU-4).
                let account = build_social_account(created.id, provider_key, identity, tokens);
                match SocialAccount::objects()
                    .on_tx(&mut tx)
                    .create(account)
                    .await
                {
                    Ok(_) => {
                        tx.commit().await.map_err(db_err)?;
                        return Ok(created.id);
                    }
                    Err(e) => {
                        let _ = tx.rollback().await;
                        return Err(db_err(e));
                    }
                }
            }
            Err(WriteError::UniqueViolation { field, .. })
                if field.as_deref() == Some("username") || field.is_none() =>
            {
                // Roll the poisoned attempt back before the next INSERT.
                let _ = tx.rollback().await;
                n += 1;
                if n > MAX_USERNAME_RETRIES {
                    return Err(OAuthError::Database(format!(
                        "could not find a unique username after {MAX_USERNAME_RETRIES} attempts \
                         (base: {base})"
                    )));
                }
                // Try next suffix.
            }
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(db_err(e));
            }
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
                    last_login TEXT,
                    email_verified_at TEXT
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
        // Google is a trusted provider (trusts_verified_email = true).
        let user_id = resolve_user("google", &id, &tokens(), None, true)
            .await
            .unwrap();

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
        let resolved = resolve_user("github", &id, &tokens(), None, true)
            .await
            .unwrap();
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
        // Trusted provider, but the identity's email is UNVERIFIED → no link.
        let resolved = resolve_user("github", &id, &tokens(), None, true)
            .await
            .unwrap();
        assert_ne!(
            resolved, existing,
            "unverified email must not hijack the account"
        );
    }

    // OAU-2: an UNTRUSTED provider (trusts_verified_email = false) asserting a
    // verified email that matches an existing user must NOT link to that user,
    // even though the email is "verified" — it auto-creates a separate, isolated
    // account instead, and does not seize the existing user's email.
    #[tokio::test]
    async fn untrusted_provider_verified_email_does_not_link() {
        boot().await;
        let victim = seed_user("victim", "victim@corp.example.com").await;
        let id = identity("custom-uid-Z", Some("victim@corp.example.com"), true);
        // provider_trusts_verified_email = false → the verified claim is ignored.
        let resolved = resolve_user("customidp", &id, &tokens(), None, false)
            .await
            .unwrap();
        assert_ne!(
            resolved, victim,
            "an untrusted provider's verified email must not take over the account"
        );
        // The auto-created account did NOT claim the victim's email — it got a
        // no-reply placeholder, so it can't be trusted or collide.
        let created = AuthUser::objects()
            .filter(auth_user::ID.eq(resolved))
            .first()
            .await
            .unwrap()
            .unwrap();
        assert_ne!(created.email, "victim@corp.example.com");
        assert!(created.email.ends_with("@users.noreply.umbral"));
    }

    // Rule 2: connect mode attaches the provider to the logged-in user.
    #[tokio::test]
    async fn connect_mode_links_to_logged_in_user() {
        boot().await;
        let me = seed_user("connector", "connector@example.com").await;
        let id = identity("google-uid-D", Some("other@example.com"), true);
        let resolved = resolve_user("google", &id, &tokens(), Some(me), true)
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
            true,
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
            true,
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
        let user_id = create_user_with_social("google", &id, &tokens(), true)
            .await
            .unwrap();

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
        let user_id2 = create_user_with_social("google", &id2, &tokens(), true)
            .await
            .unwrap();

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
            true,
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
            true,
        )
        .await;
        assert!(result.is_err(), "cannot connect someone else's identity");
    }

    // OAU-4: if the SocialAccount insert fails after the AuthUser insert, the
    // whole thing rolls back — no orphan user is left occupying the email.
    // We force the social-insert failure by reusing a `provider_uid` (the
    // `(provider, provider_uid)` unique_together), calling the atomic creator
    // directly so it bypasses resolve_user's rule-1 "already linked" dedupe.
    #[tokio::test]
    async fn social_insert_failure_leaves_no_orphan_user() {
        boot().await;

        // First signup succeeds: user + social (google, orphan-uid-1).
        create_user_with_social(
            "google",
            &identity("orphan-uid-1", Some("orphanfirst@example.com"), true),
            &tokens(),
            true,
        )
        .await
        .expect("first social signup");

        let count_before = AuthUser::objects().count().await.unwrap();

        // Second signup: a *different* email (so the AuthUser insert itself
        // succeeds with base "orphansecond") but the SAME provider_uid, so the
        // social insert trips the unique_together and the tx must roll back.
        let result = create_user_with_social(
            "google",
            &identity("orphan-uid-1", Some("orphansecond@example.com"), true),
            &tokens(),
            true,
        )
        .await;

        assert!(
            result.is_err(),
            "reusing provider_uid must fail on the social insert"
        );

        let count_after = AuthUser::objects().count().await.unwrap();
        assert_eq!(
            count_after, count_before,
            "the failed social insert must roll back the AuthUser too (no orphan, OAU-4)"
        );

        let orphan = AuthUser::objects()
            .filter(auth_user::USERNAME.eq("orphansecond"))
            .first()
            .await
            .unwrap();
        assert!(
            orphan.is_none(),
            "no AuthUser should survive a rolled-back social signup"
        );
    }
}
