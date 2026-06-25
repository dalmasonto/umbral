//! Models for the `sponsor` plugin.
//!
//! [`Partner`] is an organisation or individual backing Umbral — shown on
//! the public `/sponsor` page. Admin-managed (no public form).
//!
//! [`SponsorInquiry`] is the "Talk to us" lead captured by the public
//! sponsor form, moderated from the admin.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbral::prelude::*;

/// Sponsorship tier for a [`Partner`]. A closed set (the ORM doesn't allow
/// `Option<choices>` — a nullable tier would need a `None` variant, so the
/// default is the catch-all `Community`). Drives the badge on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PartnerTier {
    #[default]
    Community,
    Bronze,
    Silver,
    Gold,
    Platinum,
    Infrastructure,
}

impl PartnerTier {
    /// Human label for the partner card badge.
    pub fn label(self) -> &'static str {
        match self {
            PartnerTier::Community => "Community",
            PartnerTier::Bronze => "Bronze",
            PartnerTier::Silver => "Silver",
            PartnerTier::Gold => "Gold",
            PartnerTier::Platinum => "Platinum",
            PartnerTier::Infrastructure => "Infrastructure",
        }
    }
}

/// How far along a sponsor inquiry is in the pipeline. Admin-set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum InquiryStatus {
    #[default]
    New,
    Contacted,
    InDiscussion,
    Won,
    Declined,
    Spam,
}

/// A partner / sponsor backing Umbral. Public-facing on `/sponsor`, but
/// admin-managed only — there's no public form to create one (sponsors are
/// onboarded by the team, not self-served).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    soft_delete,
    plugin = "sponsor",
    display = "Partners",
    icon = "handshake"
)]
pub struct Partner {
    pub id: i64,

    #[umbral(unique, string, max_length = 120)]
    pub name: String,

    #[umbral(unique, max_length = 140)]
    pub slug: String,

    /// Partner logo. `None` falls back to a monogram on the public card.
    pub logo: Option<ImageField>,

    /// One-line summary shown on the partner card.
    #[umbral(string, max_length = 400)]
    pub description: String,

    /// Optional long-form case study / story (Markdown), rendered with
    /// `| markdown` if a detail surface is added later.
    #[umbral(
        widget = "markdown",
        help = "Markdown — the partner's full story. Optional."
    )]
    pub full_story: Option<String>,

    /// The partner's website. Shown as the card's outbound link.
    pub website_url: Option<String>,

    /// Sponsorship tier — drives the badge shown on the partner card.
    #[umbral(
        choices,
        index,
        default = "community",
        help = "Sponsorship tier. Drives the badge on the public partner card; defaults to Community."
    )]
    pub tier: PartnerTier,

    #[umbral(default = "0", index)]
    pub display_order: i32,

    /// Whether to surface this partner publicly.
    #[umbral(default = "true", index)]
    pub active: bool,

    /// Highlight on the homepage / top of the partners grid.
    #[umbral(default = "false", index)]
    pub featured: bool,

    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,

    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,

    #[umbral(noform, index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

/// A "Talk to us" sponsorship inquiry from the public sponsor form. The
/// public fields carry `#[form(...)]` validation; server-managed fields
/// are `#[umbral(noform)]`.
#[derive(Debug, Clone, Default, sqlx::FromRow, Serialize, Deserialize, Model, umbral::forms::Form)]
#[umbral(
    soft_delete,
    plugin = "sponsor",
    display = "Sponsor inquiries",
    icon = "mail"
)]
pub struct SponsorInquiry {
    pub id: i64,

    #[umbral(string, max_length = 120)]
    #[form(required, length(min = 2, max = 120))]
    pub name: String,

    #[umbral(index)]
    #[form(required, email, length(max = 254))]
    pub email: String,

    #[form(optional, length(max = 160))]
    pub organization: Option<String>,

    /// What kind of sponsorship the lead is interested in (free-text:
    /// "Open source", "Infrastructure credits", "Logo placement", …).
    #[form(optional, length(max = 120))]
    pub interest: Option<String>,

    #[form(required, length(min = 10, max = 5_000))]
    #[umbral(widget = "markdown", help = "Tell us a bit about your goals.")]
    pub message: String,

    #[umbral(noform, choices, index, default = "new")]
    pub status: InquiryStatus,

    #[umbral(noform)]
    pub ip_address: Option<String>,

    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,

    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,

    #[umbral(noform, index)]
    pub deleted_at: Option<DateTime<Utc>>,
}
