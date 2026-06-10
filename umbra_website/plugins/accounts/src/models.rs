//! Account and trust-gate models for the Umbra website.
//!
//! GitHub OAuth is deferred, but the database shape is ready for it:
//! connected accounts can satisfy review, plugin submission, security
//! voting, and showcase ownership gates once the OAuth plugin exists.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbra::prelude::*;
use umbra_auth::AuthUser;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum GitHubAccountStatus {
    Pending,
    Connected,
    Verified,
    Suspended,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TrustGateKind {
    DeveloperReview,
    PluginSubmission,
    PluginSecurityVote,
    ShowcaseSubmission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TrustGateStatus {
    Deferred,
    Pending,
    Passed,
    Failed,
    ManualReview,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(plugin = "accounts", display = "Website profiles", icon = "user")]
pub struct WebsiteProfile {
    pub id: i64,
    pub user: OneToOne<AuthUser>,
    #[umbra(string, max_length = 120)]
    pub display_name: String,
    pub role: Option<String>,
    pub company: Option<String>,
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
    #[umbra(default = "true")]
    pub is_public: bool,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(plugin = "accounts", display = "GitHub accounts", icon = "github")]
pub struct GitHubAccount {
    pub id: i64,
    #[umbra(on_delete = "set_null")]
    pub user: Option<ForeignKey<AuthUser>>,
    #[umbra(unique, string, max_length = 80)]
    pub username: String,
    #[umbra(unique)]
    pub github_id: Option<i64>,
    pub profile_url: String,
    pub avatar_url: Option<String>,
    pub account_created_at: Option<DateTime<Utc>>,
    pub connected_at: Option<DateTime<Utc>>,
    #[umbra(choices, index)]
    pub status: GitHubAccountStatus,
    pub raw_profile: Option<serde_json::Value>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(plugin = "accounts", display = "Trust gate checks", icon = "shield-check")]
pub struct TrustGateCheck {
    pub id: i64,
    #[umbra(on_delete = "cascade")]
    pub github_account: ForeignKey<GitHubAccount>,
    #[umbra(choices, index)]
    pub gate: TrustGateKind,
    #[umbra(choices, index)]
    pub status: TrustGateStatus,
    #[umbra(default = "0", min = 0)]
    pub required_account_age_days: i32,
    #[umbra(min = 0)]
    pub observed_account_age_days: Option<i32>,
    pub checked_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub metadata: Option<serde_json::Value>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}
