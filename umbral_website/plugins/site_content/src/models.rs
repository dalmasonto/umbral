//! Website content models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbral::prelude::*;
use umbral_auth::AuthUser;
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
#[umbral(
    soft_delete,
    plugin = "site_content",
    display = "Content categories",
    icon = "folder"
)]
pub struct ContentCategory {
    pub id: i64,
    #[umbral(unique, string, max_length = 100)]
    pub name: String,
    #[umbral(unique, max_length = 120)]
    pub slug: String,
    pub description: Option<String>,
    #[umbral(on_delete = "set_null")]
    pub parent: Option<ForeignKey<ContentCategory>>,
    #[umbral(default = "0")]
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
#[umbral(
    soft_delete,
    plugin = "site_content",
    display = "Content tags",
    icon = "tag"
)]
pub struct ContentTag {
    pub id: i64,
    #[umbral(unique, string, max_length = 80)]
    pub name: String,
    #[umbral(unique, max_length = 100)]
    pub slug: String,
    pub description: Option<String>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    soft_delete,
    plugin = "site_content",
    display = "Blog posts",
    icon = "newspaper"
)]
pub struct BlogPost {
    pub id: i64,
    pub public_id: Uuid,
    #[umbral(unique, max_length = 160)]
    pub slug: String,
    #[umbral(string, max_length = 180)]
    pub title: String,
    pub excerpt: Option<String>,
    #[umbral(
        widget = "markdown",
        help = "Markdown — the page body. Rendered with `| markdown`."
    )]
    pub body: String,
    #[umbral(choices, index)]
    pub status: PublishStatus,
    #[umbral(choices, index)]
    pub kind: BlogPostKind,
    #[umbral(on_delete = "set_null")]
    pub author: Option<ForeignKey<AuthUser>>,
    #[umbral(on_delete = "set_null")]
    pub category: Option<ForeignKey<ContentCategory>>,
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: M2M<ContentTag>,
    pub cover_image_url: Option<String>,
    pub attachment_url: Option<String>,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    #[umbral(default = "0", min = 0)]
    pub reading_minutes: i32,
    #[umbral(default = "0", min = 0)]
    pub view_count: i64,
    #[umbral(default = "false", index)]
    pub featured: bool,
    pub published_at: Option<DateTime<Utc>>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

impl umbral::orm::Searchable for BlogPost {
    fn kind() -> &'static str {
        "blog"
    }
    // The site routes blog posts by `slug`, not `id`, so `SearchHit.pk` must
    // carry the slug for the `/blog/{slug}` URL the header search builds.
    // `title()` already picks `title`; `body()` keeps every prose column.
    // The column is the `slug` field name (umbral columns are always the field name).
    fn ident() -> &'static str {
        "slug"
    }
    // Only published posts are searchable (soft-deleted rows excluded
    // automatically — `BlogPost` is `#[umbral(soft_delete)]`). Mirrors the old
    // `render_search` filter so drafts never surface in the header search.
    fn filter_sql() -> Option<&'static str> {
        Some("status = 'published'")
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    soft_delete,
    plugin = "site_content",
    display = "Pages",
    icon = "file-text"
)]
pub struct ContentPage {
    pub id: i64,
    #[umbral(unique, max_length = 160)]
    pub slug: String,
    #[umbral(string, max_length = 180)]
    pub title: String,
    #[umbral(
        widget = "markdown",
        help = "Markdown — the post body. Rendered with `| markdown`."
    )]
    pub body: String,
    #[umbral(choices)]
    pub template: PageTemplate,
    #[umbral(choices, index)]
    pub status: PublishStatus,
    #[umbral(on_delete = "set_null")]
    pub parent: Option<ForeignKey<ContentPage>>,
    #[umbral(default = "0")]
    pub display_order: i32,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    soft_delete,
    plugin = "site_content",
    display = "Navigation items",
    icon = "navigation"
)]
pub struct NavigationItem {
    pub id: i64,
    #[umbral(choices, index)]
    pub placement: NavigationPlacement,
    #[umbral(on_delete = "set_null")]
    pub parent: Option<ForeignKey<NavigationItem>>,
    #[umbral(string, max_length = 120)]
    pub label: String,
    pub url: Option<String>,
    #[umbral(on_delete = "set_null")]
    pub page: Option<ForeignKey<ContentPage>>,
    pub icon_key: Option<String>,
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
#[umbral(
    soft_delete,
    plugin = "site_content",
    display = "Media assets",
    icon = "image"
)]
pub struct MediaAsset {
    pub id: i64,
    #[umbral(unique, string, max_length = 180)]
    pub name: String,
    pub url: String,
    pub mime_type: String,
    #[umbral(min = 0)]
    pub byte_size: i64,
    pub checksum: Option<Vec<u8>>,
    pub alt_text: Option<String>,
    #[umbral(on_delete = "set_null")]
    pub uploaded_by: Option<ForeignKey<AuthUser>>,
    pub metadata: Option<serde_json::Value>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model, umbral::forms::Form)]
#[umbral(
    soft_delete,
    plugin = "site_content",
    display = "Contact messages",
    icon = "inbox"
)]
pub struct ContactMessage {
    pub id: i64,

    #[umbral(string, max_length = 120)]
    #[form(required, length(min = 2, max = 120))]
    pub name: String,

    #[umbral(index)]
    #[form(required, email, length(max = 254))]
    pub email: String,

    #[form(required, length(min = 3, max = 200))]
    pub subject: String,

    #[form(required, length(min = 10, max = 5_000))]
    pub message: String,

    #[umbral(noform, choices, index, default = "new")]
    pub status: ContactStatus,

    #[umbral(noform)]
    pub ip_address: Option<String>,

    #[umbral(noform)]
    pub user_agent: Option<String>,

    #[umbral(noform)]
    pub source_path: Option<String>,

    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,

    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,

    #[umbral(noform, index)]
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

/// Whether a [`ChangelogEntry`] is shipped or still planned. Drives the
/// status pill (and lets the page split shipped vs. roadmap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ChangelogKind {
    #[default]
    Released,
    Roadmap,
}

/// A single changelog row — its own table so the `/changelog` page is
/// admin-managed, not hardcoded. Rendered as a table on the public page.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    soft_delete,
    plugin = "site_content",
    display = "Changelog entries",
    icon = "git-commit"
)]
pub struct ChangelogEntry {
    pub id: i64,
    /// Version label, e.g. "0.0.1" or "toward v0.1".
    #[umbral(string, max_length = 60)]
    pub version: String,
    #[umbral(string, max_length = 180)]
    pub title: String,
    #[umbral(
        widget = "markdown",
        help = "Markdown — the highlights for this entry (a bullet list). Rendered with `| markdown`."
    )]
    pub body: String,
    #[umbral(choices, index)]
    pub kind: ChangelogKind,
    /// Highlight this row as the current release (the "Current" pill).
    #[umbral(default = "false", index)]
    pub current: bool,
    /// Release date — `None` for roadmap rows (renders as "—").
    pub released_at: Option<DateTime<Utc>>,
    /// Lower numbers sort first (newest/most-relevant at the top).
    #[umbral(default = "0", index)]
    pub display_order: i32,
    /// Visibility on the public changelog.
    #[umbral(default = "true", index)]
    pub published: bool,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    soft_delete,
    plugin = "site_content",
    display = "Site settings",
    icon = "settings"
)]
pub struct SiteSetting {
    pub id: i64,
    #[umbral(unique, string, max_length = 120)]
    pub key: String,
    pub value: serde_json::Value,
    pub description: Option<String>,
    #[umbral(default = "false")]
    pub public: bool,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}
