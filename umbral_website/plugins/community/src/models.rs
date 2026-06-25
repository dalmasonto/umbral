//! Community, social link, and newsletter configuration models.

use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use umbral::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum SocialPlatform {
    GitHub,
    Discord,
    Reddit,
    X,
    Rss,
    Docs,
    Newsletter,
    YouTube,
    LinkedIn,
    Mastodon,
    Bluesky,
    Matrix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CommunityResourceKind {
    Documentation,
    Repository,
    Chat,
    Forum,
    Social,
    Newsletter,
    Support,
    Roadmap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum NewsletterProvider {
    Sentinmail,
    External,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(soft_delete, plugin = "community", display = "Social links", icon = "share-2")]
pub struct SocialLink {
    pub id: i64,
    #[umbral(string, max_length = 80)]
    pub name: String,
    #[umbral(unique, max_length = 100)]
    pub slug: String,
    #[umbral(choices, index)]
    pub platform: SocialPlatform,
    pub url: String,
    pub icon_key: String,
    pub description: Option<String>,
    /// Brand colour (CSS, e.g. `#5865F2`) for the card icon + `--brand` hover
    /// accent. `None` falls back to the theme accent.
    pub color: Option<String>,
    /// Render the muted "Coming soon" card instead of a clickable link.
    #[umbral(default = "false", index)]
    pub coming_soon: bool,
    #[umbral(default = "0", index)]
    pub display_order: i32,
    #[umbral(default = "true", index)]
    pub active: bool,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(soft_delete, plugin = "community", display = "Community resources", icon = "network")]
pub struct CommunityResource {
    pub id: i64,
    #[umbral(string, max_length = 120)]
    pub title: String,
    #[umbral(unique, max_length = 140)]
    pub slug: String,
    #[umbral(choices, index)]
    pub kind: CommunityResourceKind,
    pub url: String,
    pub summary: Option<String>,
    #[umbral(default = "false")]
    pub is_featured: bool,
    #[umbral(default = "0")]
    pub display_order: i32,
    pub metadata: Option<serde_json::Value>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(soft_delete, plugin = "community", display = "Newsletter config", icon = "mail")]
pub struct NewsletterConfig {
    pub id: i64,
    #[umbral(string, unique, max_length = 120)]
    pub name: String,
    #[umbral(choices)]
    pub provider: NewsletterProvider,
    pub hosted_subscribe_url: String,
    pub api_endpoint: Option<String>,
    pub list_id: Option<String>,
    pub success_redirect_url: Option<String>,
    pub failure_redirect_url: Option<String>,
    pub daily_digest_time: Option<NaiveTime>,
    #[umbral(default = "true")]
    pub active: bool,
    pub metadata: Option<serde_json::Value>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}
