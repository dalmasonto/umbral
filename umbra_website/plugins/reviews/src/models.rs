//! Models for the `reviews` plugin.
//!
//! A `Review` is a developer review of the Umbra framework itself.
//! The public form is the review-submission endpoint; the admin
//! moderates the queue and approves / rejects entries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbra::prelude::*;
use umbra_auth::AuthUser;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ReviewModeration {
    #[default]
    Pending,
    Approved,
    Rejected,
    NeedsFollowup,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ReviewUsageContext {
    #[default]
    SideProject,
    WorkProject,
    InternalTool,
    Library,
    Evaluation,
}

/// A single developer review of Umbra. One active review per
/// `AuthUser` is enforced at the handler layer (not at the DB
/// level — multiple rows can exist during the moderation
/// lifecycle; the unique partial index is a future migration).
///
/// Public-facing fields: rating, title, body, role, company,
/// umbra_version, usage_context. Everything else is
/// `#[umbra(noform)]` and stamped by the admin / framework.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model, umbra::forms::Form)]
#[umbra(soft_delete, plugin = "reviews", display = "Reviews", icon = "star")]
pub struct Review {
    pub id: i64,

    /// The author. `Option` so the form layer's `..Default::default()`
    /// works without a `ForeignKey<AuthUser>` default impl — the
    /// handler fills this from `LoggedIn<AuthUser>`.
    #[umbra(noform, on_delete = "cascade")]
    pub author: Option<ForeignKey<AuthUser>>,

    /// 1..=5 star rating. Rendered as a number input on the form.
    #[form(required)]
    #[umbra(min = 1, max = 5)]
    pub rating: i32,

    #[form(required, length(min = 5, max = 140))]
    pub title: String,

    #[form(required, length(min = 50, max = 5_000))]
    pub body: String,

    /// The reviewer's role / title (e.g. "Staff Engineer",
    /// "Indie developer"). Free-text.
    #[form(optional, length(max = 120))]
    pub role: Option<String>,

    /// The reviewer's company or project type
    /// (e.g. "Acme Inc.", "Indie game studio"). Free-text.
    #[form(optional, length(max = 120))]
    pub company: Option<String>,

    /// The Umbra version the review is grounded in
    /// (e.g. "0.0.1", "main @ a1b2c3"). Free-text for now.
    #[form(optional, length(max = 60))]
    pub umbra_version: Option<String>,

    #[umbra(noform, choices, default = "side_project")]
    pub usage_context: ReviewUsageContext,

    /// Set to true when the author has connected a GitHub account
    /// ≥ 1 year old. Filled by the auth plugin once a GitHub OAuth
    /// plugin lands — for now the form marks it `false` and the
    /// admin / future verifier flips it.
    #[umbra(noform, default = "false")]
    pub verified_developer: bool,

    #[umbra(noform, choices, index, default = "pending")]
    pub moderation: ReviewModeration,

    /// Admin-only flag: highlight on the homepage.
    #[umbra(noform, default = "false", index)]
    pub featured: bool,

    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,

    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,

    #[umbra(noform, index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

impl Default for Review {
    fn default() -> Self {
        Self {
            id: 0,
            author: None,
            rating: 5,
            title: String::new(),
            body: String::new(),
            role: None,
            company: None,
            umbra_version: None,
            usage_context: ReviewUsageContext::default(),
            verified_developer: false,
            moderation: ReviewModeration::default(),
            featured: false,
            created_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
            updated_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
            deleted_at: None,
        }
    }
}
