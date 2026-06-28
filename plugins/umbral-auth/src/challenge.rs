//! Short-lived, single-use, hashed-at-rest secrets for the email-verification
//! and password-reset flows. One table, discriminated by `purpose`.

use crate::AuthUser;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbral::orm::ForeignKey;

/// Stored discriminator values for [`AuthChallenge::purpose`].
pub const PURPOSE_EMAIL_VERIFY: &str = "email_verify";
pub const PURPOSE_PASSWORD_RESET: &str = "password_reset";

/// One pending challenge. The plaintext (6-digit code or opaque token) is
/// never stored — only `base64(sha256(plaintext))`. Single-use (`used_at`),
/// time-boxed (`expires_at`), and (for codes) attempt-capped (`attempts`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
pub struct AuthChallenge {
    pub id: i64,
    #[umbral(on_delete = "cascade")]
    pub user_id: ForeignKey<AuthUser>,
    #[umbral(max_length = 32)]
    pub purpose: String,
    #[umbral(max_length = 64)]
    pub secret_hash: String,
    pub expires_at: DateTime<Utc>,
    pub attempts: i32,
    pub used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
