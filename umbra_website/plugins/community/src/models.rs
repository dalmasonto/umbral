//! Community, social link, and newsletter configuration models.

use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use umbra::prelude::*;

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
#[umbra(soft_delete, plugin = "community", display = "Social links", icon = "share-2")]
pub struct SocialLink {
    pub id: i64,
    #[umbra(string, max_length = 80)]
    pub name: String,
    #[umbra(unique, max_length = 100)]
    pub slug: String,
    #[umbra(choices, index)]
    pub platform: SocialPlatform,
    pub url: String,
    pub icon_key: String,
    pub description: Option<String>,
    #[umbra(default = "0", index)]
    pub display_order: i32,
    #[umbra(default = "true", index)]
    pub active: bool,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(soft_delete, plugin = "community", display = "Community resources", icon = "network")]
pub struct CommunityResource {
    pub id: i64,
    #[umbra(string, max_length = 120)]
    pub title: String,
    #[umbra(unique, max_length = 140)]
    pub slug: String,
    #[umbra(choices, index)]
    pub kind: CommunityResourceKind,
    pub url: String,
    pub summary: Option<String>,
    #[umbra(default = "false")]
    pub is_featured: bool,
    #[umbra(default = "0")]
    pub display_order: i32,
    pub metadata: Option<serde_json::Value>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(soft_delete, plugin = "community", display = "Newsletter config", icon = "mail")]
pub struct NewsletterConfig {
    pub id: i64,
    #[umbra(string, unique, max_length = 120)]
    pub name: String,
    #[umbra(choices)]
    pub provider: NewsletterProvider,
    pub hosted_subscribe_url: String,
    pub api_endpoint: Option<String>,
    pub list_id: Option<String>,
    pub success_redirect_url: Option<String>,
    pub failure_redirect_url: Option<String>,
    pub daily_digest_time: Option<NaiveTime>,
    #[umbra(default = "true")]
    pub active: bool,
    pub metadata: Option<serde_json::Value>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}
