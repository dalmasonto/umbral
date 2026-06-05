//! Content plugin models.
//!
//! All content-related models live here: blog, pages, FAQ, navigation,
//! marketing, communication, media library, SEO, and site settings.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbra::prelude::*;
use umbra_auth::AuthUser;

// ---------------------------------------------------------------------------
// Choice enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PostStatus {
    Draft,
    Published,
    Scheduled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ContactStatus {
    New,
    Read,
    Replied,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum RedirectCode {
    MovedPermanently,
    Found,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PageTemplate {
    Default,
    FullWidth,
    Landing,
}

// ---------------------------------------------------------------------------
// Taxonomy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Category {
    pub id: i64,
    #[umbra(unique)]
    pub slug: String,
    #[umbra(string)]
    pub name: String,
    pub description: Option<String>,
    pub image: Option<String>,
    pub parent: Option<ForeignKey<Category>>,
    #[umbra(default = "0")]
    pub position: i32,
    #[umbra(default = "true")]
    pub is_active: bool,
    pub test_field: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Tag {
    pub id: i64,
    #[umbra(unique, string)]
    pub name: String,
    #[umbra(unique)]
    pub slug: String,
}

// ---------------------------------------------------------------------------
// Blog
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Post {
    pub id: i64,
    #[umbra(unique)]
    pub slug: String,
    #[umbra(string)]
    pub title: String,
    pub excerpt: Option<String>,
    pub body: String,
    #[umbra(choices)]
    pub status: PostStatus,
    pub author: ForeignKey<AuthUser>,
    pub category: Option<ForeignKey<Category>>,
    /// Many-to-many to Tag. The framework auto-creates a junction
    /// table called `post_tags` with `(parent_id, child_id)` columns
    /// at migration time.
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: M2M<Tag>,
    pub cover_image: Option<String>,
    pub attachment: Option<String>,
    #[umbra(default = "false")]
    pub is_featured: bool,
    #[umbra(default = "0")]
    pub reading_minutes: i32,
    #[umbra(default = "0")]
    pub view_count: i64,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Comment {
    pub id: i64,
    pub post: ForeignKey<Post>,
    pub parent: Option<ForeignKey<Comment>>,
    pub author: Option<ForeignKey<AuthUser>>,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub body: String,
    #[umbra(default = "false")]
    pub is_approved: bool,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Pages / CMS
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Page {
    pub id: i64,
    #[umbra(unique)]
    pub slug: String,
    #[umbra(string)]
    pub title: String,
    pub content: String,
    #[umbra(choices)]
    pub template: PageTemplate,
    pub parent: Option<ForeignKey<Page>>,
    #[umbra(default = "0")]
    pub position: i32,
    #[umbra(default = "false")]
    pub is_published: bool,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Faq {
    pub id: i64,
    pub question: String,
    pub answer: String,
    pub category: Option<String>,
    #[umbra(default = "0")]
    pub position: i32,
    #[umbra(default = "true")]
    pub is_published: bool,
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Menu {
    pub id: i64,
    #[umbra(unique)]
    pub name: String,
    #[umbra(unique)]
    pub slug: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct MenuItem {
    pub id: i64,
    pub menu: ForeignKey<Menu>,
    pub parent: Option<ForeignKey<MenuItem>>,
    pub label: String,
    pub url: Option<String>,
    pub page: Option<ForeignKey<Page>>,
    #[umbra(default = "0")]
    pub position: i32,
    #[umbra(default = "_self")]
    pub target: String,
    #[umbra(default = "true")]
    pub is_active: bool,
}

// ---------------------------------------------------------------------------
// Marketing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Banner {
    pub id: i64,
    #[umbra(string)]
    pub title: String,
    pub content: Option<String>,
    pub image: Option<String>,
    pub link_url: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    #[umbra(default = "0")]
    pub position: i32,
    #[umbra(default = "true")]
    pub is_active: bool,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Testimonial {
    pub id: i64,
    #[umbra(string)]
    pub author_name: String,
    pub author_title: Option<String>,
    pub avatar: Option<String>,
    pub quote: String,
    pub rating: Option<i32>,
    #[umbra(default = "false")]
    pub is_featured: bool,
    #[umbra(default = "0")]
    pub position: i32,
}

// ---------------------------------------------------------------------------
// Communication
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct ContactMessage {
    pub id: i64,
    #[umbra(string)]
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
    pub subject: String,
    pub message: String,
    #[umbra(choices)]
    pub status: ContactStatus,
    pub ip_address: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Subscriber {
    pub id: i64,
    #[umbra(unique)]
    pub email: String,
    #[umbra(default = "false")]
    pub is_confirmed: bool,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub source: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Media library
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct MediaAsset {
    pub id: i64,
    pub file: String,
    pub title: Option<String>,
    pub alt_text: Option<String>,
    pub folder: Option<String>,
    pub mime: String,
    pub size_bytes: i64,
    pub uploaded_by: Option<ForeignKey<AuthUser>>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// SEO & config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Redirect {
    pub id: i64,
    #[umbra(unique)]
    pub from_path: String,
    pub to_path: String,
    #[umbra(choices)]
    pub code: RedirectCode,
    #[umbra(default = "true")]
    pub is_active: bool,
    #[umbra(default = "0")]
    pub hits: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct SiteSetting {
    pub id: i64,
    #[umbra(string)]
    pub site_name: String,
    pub tagline: Option<String>,
    pub logo: Option<String>,
    pub favicon: Option<String>,
    pub contact_email: String,
    pub social_links: serde_json::Value,
    pub default_seo: serde_json::Value,
    pub config: serde_json::Value,
}
