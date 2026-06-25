//! Password-strength validation — umbral's `AUTH_PASSWORD_VALIDATORS` equivalent.
//!
//! Django ships a small set of validators that every password is checked
//! against before it's hashed: minimum length, a common-password denylist,
//! an all-numeric reject, and a similarity-to-user-attributes guard. umbral
//! mirrors that surface here, with one deliberate difference: **it's on by
//! default with no opt-in.** A fresh `AuthPlugin::default()` enforces the
//! full set, so `create_user("a", ...)` is rejected out of the box. An app
//! that genuinely wants no policy must say so explicitly via
//! [`crate::AuthPlugin::disable_password_validation`].
//!
//! ## The contract
//!
//! A [`PasswordValidator`] is `fn validate(&self, password, ctx) -> Result<(), String>`,
//! where the `String` is a human-readable reason the password was rejected
//! (Django returns a `ValidationError` with a message; we return the message
//! directly). [`PasswordContext`] carries the username / email so the
//! similarity validator can compare against them — both are `None` when
//! there's no user context (e.g. a standalone strength check).
//!
//! A [`PasswordPolicy`] is an ordered `Vec<Box<dyn PasswordValidator>>`.
//! [`validate_password`] runs the ambiently-installed policy (falling back
//! to [`PasswordPolicy::default`] when none is installed, so the free-function
//! helpers stay secure even before `on_ready` runs) and **collects every
//! failure**, not just the first — a caller showing a form gets the full list.
//!
//! ## Ambient install
//!
//! The active policy lives in a process-wide `OnceLock`, installed once in
//! [`crate::AuthPlugin::on_ready`]. This mirrors the sessions plugin's
//! `SLIDING_EXPIRY_ENABLED` flag: free functions (`create_user`,
//! `set_password`) have no handle to the `AuthPlugin`, so they read the
//! policy ambiently.

use std::sync::OnceLock;

/// The user context a validator can compare a candidate password against.
///
/// Both fields are `Option` because not every call site has a user: a bare
/// strength check (e.g. a password-meter endpoint) passes `PasswordContext::empty()`,
/// while `create_user` / `set_password` fill in what they know. The
/// similarity validator simply skips any field that's `None`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PasswordContext<'a> {
    /// The account's login handle, when known.
    pub username: Option<&'a str>,
    /// The account's email address, when known. The similarity check uses
    /// the local-part (before the `@`) as well as the whole string.
    pub email: Option<&'a str>,
}

impl<'a> PasswordContext<'a> {
    /// A context with no user attributes — the similarity validator is a
    /// no-op against it. Use for standalone strength checks.
    pub fn empty() -> Self {
        Self::default()
    }

    /// A context carrying just a username.
    pub fn for_username(username: &'a str) -> Self {
        Self {
            username: Some(username),
            email: None,
        }
    }

    /// A context carrying a username and an email.
    pub fn new(username: Option<&'a str>, email: Option<&'a str>) -> Self {
        Self { username, email }
    }
}

/// One password-strength rule. Implementors return `Ok(())` when the
/// password passes and `Err(reason)` with a human-readable message when it
/// fails. Stateless rules (most of them) are zero-cost; rules that carry
/// config (e.g. [`MinLengthValidator`]'s threshold) own their data.
pub trait PasswordValidator: Send + Sync + std::fmt::Debug {
    /// Check `password` against this rule. `ctx` carries the user
    /// attributes a rule may compare against (only the similarity rule
    /// uses it today). Return `Err(reason)` to reject.
    fn validate(&self, password: &str, ctx: &PasswordContext<'_>) -> Result<(), String>;
}

// =========================================================================
// Default validators (Django parity)
// =========================================================================

/// Reject passwords shorter than `min` characters. Django's default is 8.
///
/// Length is counted in Unicode scalar values (`chars().count()`), not
/// bytes, so a password of 8 emoji counts as 8.
#[derive(Debug, Clone, Copy)]
pub struct MinLengthValidator(pub usize);

impl Default for MinLengthValidator {
    fn default() -> Self {
        Self(8)
    }
}

impl PasswordValidator for MinLengthValidator {
    fn validate(&self, password: &str, _ctx: &PasswordContext<'_>) -> Result<(), String> {
        let len = password.chars().count();
        if len < self.0 {
            Err(format!(
                "This password is too short. It must contain at least {} characters.",
                self.0
            ))
        } else {
            Ok(())
        }
    }
}

/// The embedded common-password denylist. One password per line, lowercase.
/// Curated from the well-known "most common passwords" lists. Case-insensitive
/// matching happens at validation time.
const COMMON_PASSWORDS: &str = include_str!("common_passwords.txt");

/// Reject passwords that appear in an embedded denylist of common
/// passwords. Matching is case-insensitive (`PASSWORD` and `password` are
/// both rejected). Django loads a 20k-entry gzip list; we embed a curated
/// few hundred — the entries that matter most — to keep the binary small
/// while still catching the obvious choices.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommonPasswordValidator;

impl PasswordValidator for CommonPasswordValidator {
    fn validate(&self, password: &str, _ctx: &PasswordContext<'_>) -> Result<(), String> {
        let lower = password.trim().to_lowercase();
        // The denylist is line-delimited and stored lowercase; a linear
        // scan over a few hundred entries is well under a microsecond and
        // runs once per registration, so a HashSet isn't worth the
        // allocation.
        let hit = COMMON_PASSWORDS
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .any(|entry| entry == lower);
        if hit {
            Err("This password is too common.".to_string())
        } else {
            Ok(())
        }
    }
}

/// Reject passwords made up entirely of digits (`"12345678"`). An
/// all-numeric password — even a long one — has far less entropy than a
/// mixed one and is trivially brute-forced. Empty strings are not numeric
/// (the min-length rule catches those).
#[derive(Debug, Clone, Copy, Default)]
pub struct NumericPasswordValidator;

impl PasswordValidator for NumericPasswordValidator {
    fn validate(&self, password: &str, _ctx: &PasswordContext<'_>) -> Result<(), String> {
        if !password.is_empty() && password.chars().all(|c| c.is_ascii_digit()) {
            Err("This password is entirely numeric.".to_string())
        } else {
            Ok(())
        }
    }
}

/// Reject passwords that are too similar to the username or the email
/// local-part. Django uses a `SequenceMatcher` ratio with a 0.7 threshold;
/// we approximate with a cheaper two-pronged check that catches the same
/// real-world cases:
///
/// 1. **Substring containment** — the password contains the attribute (or
///    vice-versa), case-insensitive. Catches `alice` → `alice123`.
/// 2. **Character-overlap ratio** — the fraction of the attribute's
///    characters present in the password is `>= threshold`. Catches
///    rearrangements / interleavings that substring containment misses.
///
/// Attributes shorter than 3 characters are skipped (a 1-char username
/// would match almost everything). The email is checked both whole and as
/// its local-part (before `@`).
#[derive(Debug, Clone, Copy)]
pub struct UserAttributeSimilarityValidator {
    /// Overlap ratio at or above which the password is rejected. Django's
    /// default is 0.7.
    pub threshold: f64,
}

impl Default for UserAttributeSimilarityValidator {
    fn default() -> Self {
        Self { threshold: 0.7 }
    }
}

impl UserAttributeSimilarityValidator {
    /// True when `password` is too similar to `attribute` under this
    /// validator's rules. Both are lowercased by the caller.
    fn too_similar(&self, password: &str, attribute: &str) -> bool {
        if attribute.chars().count() < 3 {
            return false;
        }
        if password.contains(attribute) || attribute.contains(password) {
            return true;
        }
        // Fraction of the attribute's distinct characters that also appear
        // in the password. A cheap stand-in for SequenceMatcher that still
        // flags heavy reuse of the attribute's letters.
        let pw_chars: std::collections::HashSet<char> = password.chars().collect();
        let attr_chars: std::collections::HashSet<char> = attribute.chars().collect();
        if attr_chars.is_empty() {
            return false;
        }
        let shared = attr_chars.iter().filter(|c| pw_chars.contains(c)).count();
        let ratio = shared as f64 / attr_chars.len() as f64;
        ratio >= self.threshold
    }
}

impl PasswordValidator for UserAttributeSimilarityValidator {
    fn validate(&self, password: &str, ctx: &PasswordContext<'_>) -> Result<(), String> {
        let pw = password.to_lowercase();

        let mut attributes: Vec<String> = Vec::new();
        if let Some(username) = ctx.username {
            attributes.push(username.to_lowercase());
        }
        if let Some(email) = ctx.email {
            let email = email.to_lowercase();
            if let Some((local, _domain)) = email.split_once('@') {
                attributes.push(local.to_string());
            }
            attributes.push(email);
        }

        for attribute in attributes {
            if self.too_similar(&pw, &attribute) {
                return Err("This password is too similar to your username or email.".to_string());
            }
        }
        Ok(())
    }
}

// =========================================================================
// PasswordPolicy
// =========================================================================

/// An ordered set of [`PasswordValidator`]s applied to every password.
///
/// [`PasswordPolicy::default`] is the Django-parity set: min-length 8,
/// common-password denylist, all-numeric reject, and user-attribute
/// similarity. Construct a custom policy with [`PasswordPolicy::new`] +
/// [`PasswordPolicy::with`], or start from the defaults and tweak.
#[derive(Debug)]
pub struct PasswordPolicy {
    validators: Vec<Box<dyn PasswordValidator>>,
}

impl PasswordPolicy {
    /// An empty policy — no validators, every password passes. Used as the
    /// base for a hand-built policy and as the marker for
    /// [`crate::AuthPlugin::disable_password_validation`].
    pub fn empty() -> Self {
        Self {
            validators: Vec::new(),
        }
    }

    /// Alias for [`PasswordPolicy::empty`], read as "no validation".
    pub fn none() -> Self {
        Self::empty()
    }

    /// Start a custom policy from a vector of validators.
    pub fn new(validators: Vec<Box<dyn PasswordValidator>>) -> Self {
        Self { validators }
    }

    /// Append a validator, returning `self` for chaining.
    pub fn with(mut self, validator: Box<dyn PasswordValidator>) -> Self {
        self.validators.push(validator);
        self
    }

    /// The number of validators in the policy. `0` means no enforcement.
    pub fn len(&self) -> usize {
        self.validators.len()
    }

    /// Whether the policy is empty (no enforcement).
    pub fn is_empty(&self) -> bool {
        self.validators.is_empty()
    }

    /// Run every validator against `password` and collect **all** failure
    /// reasons. Returns `Ok(())` only when every validator passes.
    pub fn check(&self, password: &str, ctx: &PasswordContext<'_>) -> Result<(), Vec<String>> {
        let mut reasons = Vec::new();
        for validator in &self.validators {
            if let Err(reason) = validator.validate(password, ctx) {
                reasons.push(reason);
            }
        }
        if reasons.is_empty() {
            Ok(())
        } else {
            Err(reasons)
        }
    }
}

/// The default policy IS the secure-by-default set. Mirrors Django's
/// out-of-the-box `AUTH_PASSWORD_VALIDATORS`.
impl PasswordPolicy {
    /// The four Django-parity validators with their default settings. This
    /// is what an unconfigured `AuthPlugin` enforces.
    pub fn django_defaults() -> Self {
        Self::new(vec![
            Box::new(MinLengthValidator::default()),
            Box::new(CommonPasswordValidator),
            Box::new(NumericPasswordValidator),
            Box::new(UserAttributeSimilarityValidator::default()),
        ])
    }
}

// `Default` and `django_defaults` are the same thing; keep both so callers
// can read whichever is clearer at the call site.
impl PasswordPolicy {
    /// Construct the default secure policy. Named separately from the
    /// `Default` trait impl so it reads clearly in the `OnceLock` fallback.
    fn default_secure() -> Self {
        Self::django_defaults()
    }
}

impl std::default::Default for PasswordPolicy {
    fn default() -> Self {
        // SECURE BY DEFAULT: a default policy is the full validator set, NOT
        // an empty one. An app that wants no validation must ask explicitly.
        Self::django_defaults()
    }
}

// =========================================================================
// Ambient install + free-function entry point
// =========================================================================

/// The process-wide active policy. Installed once in
/// [`crate::AuthPlugin::on_ready`]. Mirrors the sessions plugin's
/// `SLIDING_EXPIRY_ENABLED` ambient flag.
static PASSWORD_POLICY: OnceLock<PasswordPolicy> = OnceLock::new();

/// Install the active policy. Called once at boot from `on_ready`.
/// Idempotent — the first install wins (same "first wins" contract as the
/// ambient pool / sliding-expiry flag), so a second plugin or a test that
/// boots twice in one process can't clobber it.
pub(crate) fn install_policy(policy: PasswordPolicy) {
    let _ = PASSWORD_POLICY.set(policy);
}

/// Validate `password` against the active policy, collecting every failure.
///
/// Falls back to [`PasswordPolicy::default`] (the full secure set) when no
/// policy has been installed yet — so the free-function helpers
/// (`create_user`, `set_password`) are enforced even before `on_ready`
/// runs, and in tests that exercise the helpers without a full boot.
///
/// Returns `Err(reasons)` listing all the rules the password failed.
pub fn validate_password(password: &str, ctx: &PasswordContext<'_>) -> Result<(), Vec<String>> {
    match PASSWORD_POLICY.get() {
        Some(policy) => policy.check(password, ctx),
        None => {
            // Build the default lazily rather than installing it: an
            // explicit `on_ready` install must still win, so we don't seed
            // the OnceLock here.
            PasswordPolicy::default_secure().check(password, ctx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_length_rejects_short() {
        let v = MinLengthValidator::default();
        assert!(v.validate("abc", &PasswordContext::empty()).is_err());
        assert!(v.validate("abcdefgh", &PasswordContext::empty()).is_ok());
    }

    #[test]
    fn min_length_honours_custom_threshold() {
        let v = MinLengthValidator(12);
        assert!(v.validate("abcdefgh", &PasswordContext::empty()).is_err());
        assert!(
            v.validate("abcdefghijkl", &PasswordContext::empty())
                .is_ok()
        );
    }

    #[test]
    fn common_rejects_password_case_insensitive() {
        let v = CommonPasswordValidator;
        assert!(v.validate("password", &PasswordContext::empty()).is_err());
        assert!(v.validate("PASSWORD", &PasswordContext::empty()).is_err());
        assert!(v.validate("qwerty", &PasswordContext::empty()).is_err());
        assert!(v.validate("letmein", &PasswordContext::empty()).is_err());
        assert!(
            v.validate("Tr0ub4dour&3xpl", &PasswordContext::empty())
                .is_ok()
        );
    }

    #[test]
    fn numeric_rejects_all_digits() {
        let v = NumericPasswordValidator;
        assert!(v.validate("12345678", &PasswordContext::empty()).is_err());
        assert!(v.validate("0000000000", &PasswordContext::empty()).is_err());
        assert!(v.validate("abc12345", &PasswordContext::empty()).is_ok());
        // Empty is handled by min-length, not the numeric rule.
        assert!(v.validate("", &PasswordContext::empty()).is_ok());
    }

    #[test]
    fn similarity_rejects_username_in_password() {
        let v = UserAttributeSimilarityValidator::default();
        let ctx = PasswordContext::for_username("alice");
        assert!(v.validate("alice123", &ctx).is_err());
        assert!(v.validate("Tr0ub4dour&3xpl", &ctx).is_ok());
    }

    #[test]
    fn similarity_uses_email_local_part() {
        let v = UserAttributeSimilarityValidator::default();
        let ctx = PasswordContext::new(None, Some("bob.smith@example.com"));
        assert!(v.validate("bob.smith99", &ctx).is_err());
    }

    #[test]
    fn similarity_skips_short_attributes() {
        let v = UserAttributeSimilarityValidator::default();
        let ctx = PasswordContext::for_username("ab");
        // A 2-char username must not flag an unrelated strong password.
        assert!(v.validate("Tr0ub4dour&3xpl", &ctx).is_ok());
    }

    #[test]
    fn policy_aggregates_multiple_failures() {
        let policy = PasswordPolicy::default();
        // "alice" → too short, similar to username, and (lowercased) a
        // common-ish weak choice. Expect at least two distinct reasons.
        let ctx = PasswordContext::for_username("alice");
        let err = policy
            .check("alice", &ctx)
            .expect_err("weak password must fail");
        assert!(
            err.len() >= 2,
            "expected multiple failure reasons, got {err:?}"
        );
    }

    #[test]
    fn strong_password_passes_all() {
        let policy = PasswordPolicy::default();
        let ctx = PasswordContext::new(Some("alice"), Some("alice@example.com"));
        assert!(
            policy.check("Tr0ub4dour&3xpl", &ctx).is_ok(),
            "a strong password must pass the default policy"
        );
    }

    #[test]
    fn default_policy_is_not_empty() {
        // The load-bearing secure-by-default guarantee.
        assert!(!PasswordPolicy::default().is_empty());
        assert_eq!(PasswordPolicy::default().len(), 4);
    }

    #[test]
    fn empty_policy_passes_everything() {
        let policy = PasswordPolicy::empty();
        assert!(policy.check("a", &PasswordContext::empty()).is_ok());
    }

    #[test]
    fn validate_password_falls_back_to_secure_default() {
        // Even with no install, a weak password is rejected.
        assert!(validate_password("a", &PasswordContext::empty()).is_err());
        assert!(validate_password("Tr0ub4dour&3xpl", &PasswordContext::empty()).is_ok());
    }
}
