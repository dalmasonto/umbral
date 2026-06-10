//! Website content models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbra::prelude::*;
use umbra_auth::AuthUser;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PublishStatus {
    Draft,
    Published,
    Scheduled,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum BlogPostKind {
    Release,
    Tutorial,
    DesignNote,
    PluginSpotlight,
    SecurityAdvisory,
    Community,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PageTemplate {
    Default,
    FullWidth,
    Landing,
    DocsIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ContactStatus {
    #[default]
    New,
    Triaged,
    Replied,
    Closed,
    Spam,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum NavigationPlacement {
    Header,
    Footer,
    Sidebar,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(soft_delete, plugin = "site_content", display = "Content categories", icon = "folder")]
pub struct ContentCategory {
    pub id: i64,
    #[umbra(unique, string, max_length = 100)]
    pub name: String,
    #[umbra(unique, max_length = 120)]
    pub slug: String,
    pub description: Option<String>,
    #[umbra(on_delete = "set_null")]
    pub parent: Option<ForeignKey<ContentCategory>>,
    #[umbra(default = "0")]
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
#[umbra(soft_delete, plugin = "site_content", display = "Content tags", icon = "tag")]
pub struct ContentTag {
    pub id: i64,
    #[umbra(unique, string, max_length = 80)]
    pub name: String,
    #[umbra(unique, max_length = 100)]
    pub slug: String,
    pub description: Option<String>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(soft_delete, plugin = "site_content", display = "Blog posts", icon = "newspaper")]
pub struct BlogPost {
    pub id: i64,
    pub public_id: Uuid,
    #[umbra(unique, max_length = 160)]
    pub slug: String,
    #[umbra(string, max_length = 180)]
    pub title: String,
    pub excerpt: Option<String>,
    #[umbra(widget = "markdown", help = "Markdown — the page body. Rendered with `| markdown`.")]
    pub body: String,
    #[umbra(choices, index)]
    pub status: PublishStatus,
    #[umbra(choices, index)]
    pub kind: BlogPostKind,
    #[umbra(on_delete = "set_null")]
    pub author: Option<ForeignKey<AuthUser>>,
    #[umbra(on_delete = "set_null")]
    pub category: Option<ForeignKey<ContentCategory>>,
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: M2M<ContentTag>,
    pub cover_image_url: Option<String>,
    pub attachment_url: Option<String>,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    #[umbra(default = "0", min = 0)]
    pub reading_minutes: i32,
    #[umbra(default = "0", min = 0)]
    pub view_count: i64,
    #[umbra(default = "false", index)]
    pub featured: bool,
    pub published_at: Option<DateTime<Utc>>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(soft_delete, plugin = "site_content", display = "Pages", icon = "file-text")]
pub struct ContentPage {
    pub id: i64,
    #[umbra(unique, max_length = 160)]
    pub slug: String,
    #[umbra(string, max_length = 180)]
    pub title: String,
    #[umbra(widget = "markdown", help = "Markdown — the post body. Rendered with `| markdown`.")]
    pub body: String,
    #[umbra(choices)]
    pub template: PageTemplate,
    #[umbra(choices, index)]
    pub status: PublishStatus,
    #[umbra(on_delete = "set_null")]
    pub parent: Option<ForeignKey<ContentPage>>,
    #[umbra(default = "0")]
    pub display_order: i32,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(soft_delete, plugin = "site_content", display = "Navigation items", icon = "navigation")]
pub struct NavigationItem {
    pub id: i64,
    #[umbra(choices, index)]
    pub placement: NavigationPlacement,
    #[umbra(on_delete = "set_null")]
    pub parent: Option<ForeignKey<NavigationItem>>,
    #[umbra(string, max_length = 120)]
    pub label: String,
    pub url: Option<String>,
    #[umbra(on_delete = "set_null")]
    pub page: Option<ForeignKey<ContentPage>>,
    pub icon_key: Option<String>,
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
#[umbra(soft_delete, plugin = "site_content", display = "Media assets", icon = "image")]
pub struct MediaAsset {
    pub id: i64,
    #[umbra(unique, string, max_length = 180)]
    pub name: String,
    pub url: String,
    pub mime_type: String,
    #[umbra(min = 0)]
    pub byte_size: i64,
    pub checksum: Option<Vec<u8>>,
    pub alt_text: Option<String>,
    #[umbra(on_delete = "set_null")]
    pub uploaded_by: Option<ForeignKey<AuthUser>>,
    pub metadata: Option<serde_json::Value>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model, umbra::forms::Form)]
#[umbra(soft_delete, plugin = "site_content", display = "Contact messages", icon = "inbox")]
pub struct ContactMessage {
    pub id: i64,

    #[umbra(string, max_length = 120)]
    #[form(required, length(min = 2, max = 120))]
    pub name: String,

    #[umbra(index)]
    #[form(required, email, length(max = 254))]
    pub email: String,

    #[form(required, length(min = 3, max = 200))]
    pub subject: String,

    #[form(required, length(min = 10, max = 5_000))]
    pub message: String,

    #[umbra(noform, choices, index, default = "new")]
    pub status: ContactStatus,

    #[umbra(noform)]
    pub ip_address: Option<String>,

    #[umbra(noform)]
    pub user_agent: Option<String>,

    #[umbra(noform)]
    pub source_path: Option<String>,

    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,

    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,

    #[umbra(noform, index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

impl Default for ContactMessage {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            email: String::new(),
            subject: String::new(),
            message: String::new(),
            status: ContactStatus::default(),
            ip_address: None,
            user_agent: None,
            source_path: None,
            created_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
            updated_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
            deleted_at: None,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(soft_delete, plugin = "site_content", display = "Site settings", icon = "settings")]
pub struct SiteSetting {
    pub id: i64,
    #[umbra(unique, string, max_length = 120)]
    pub key: String,
    pub value: serde_json::Value,
    pub description: Option<String>,
    #[umbra(default = "false")]
    pub public: bool,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}
