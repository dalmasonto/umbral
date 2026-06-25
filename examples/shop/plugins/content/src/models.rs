//! Content plugin models.
//!
//! All content-related models live here: blog, pages, FAQ, navigation,
//! marketing, communication, media library, SEO, and site settings.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbral::prelude::*;
use umbral_auth::AuthUser;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ContactStatus {
    /// gaps2 #19 follow-up: `New` is the default so `ContactMessage`
    /// can `#[derive(Default)]` — which the Form derive relies on
    /// to fill server-managed fields via `..Default::default()`.
    /// Inbound submissions always land as `New`; the admin walks
    /// them through the other states.
    #[default]
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
    #[umbral(unique)]
    pub slug: String,
    #[umbral(string)]
    pub name: String,
    pub description: Option<String>,
    pub image: Option<String>,
    pub parent: Option<ForeignKey<Category>>,
    #[umbral(default = "0")]
    pub position: i32,
    #[umbral(default = "true")]
    pub is_active: bool,
    pub test_field: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Tag {
    pub id: i64,
    #[umbral(unique, string)]
    pub name: String,
    #[umbral(unique)]
    pub slug: String,
}

// ---------------------------------------------------------------------------
// Blog
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Post {
    pub id: i64,
    #[umbral(unique)]
    pub slug: String,
    #[umbral(string)]
    pub title: String,
    pub excerpt: Option<String>,
    pub body: String,
    #[umbral(choices)]
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
    #[umbral(default = "false")]
    pub is_featured: bool,
    #[umbral(default = "0")]
    pub reading_minutes: i32,
    #[umbral(default = "0")]
    pub view_count: i64,
    pub seo_title: Option<String>,
    pub seo_description: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
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
    #[umbral(default = "false")]
    pub is_approved: bool,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Pages / CMS
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Page {
    pub id: i64,
    #[umbral(unique)]
    pub slug: String,
    #[umbral(string)]
    pub title: String,
    pub content: String,
    #[umbral(choices)]
    pub template: PageTemplate,
    pub parent: Option<ForeignKey<Page>>,
    #[umbral(default = "0")]
    pub position: i32,
    #[umbral(default = "false")]
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
    #[umbral(default = "0")]
    pub position: i32,
    #[umbral(default = "true")]
    pub is_published: bool,
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Menu {
    pub id: i64,
    #[umbral(unique)]
    pub name: String,
    #[umbral(unique)]
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
    #[umbral(default = "0")]
    pub position: i32,
    #[umbral(default = "_self")]
    pub target: String,
    #[umbral(default = "true")]
    pub is_active: bool,
}

// ---------------------------------------------------------------------------
// Marketing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Banner {
    pub id: i64,
    #[umbral(string)]
    pub title: String,
    pub content: Option<String>,
    pub image: Option<String>,
    pub link_url: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    #[umbral(default = "0")]
    pub position: i32,
    #[umbral(default = "true")]
    pub is_active: bool,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Testimonial {
    pub id: i64,
    #[umbral(string)]
    pub author_name: String,
    pub author_title: Option<String>,
    pub avatar: Option<String>,
    pub quote: String,
    pub rating: Option<i32>,
    #[umbral(default = "false")]
    pub is_featured: bool,
    #[umbral(default = "0")]
    pub position: i32,
}

// ---------------------------------------------------------------------------
// Communication
// ---------------------------------------------------------------------------

/// Single source of truth for the contact-message surface: the
/// persisted Model AND the public form share this declaration.
///
/// The `#[derive(Form)]` (gaps2 #19) sees the existing Model attrs
/// and skips the server-managed fields automatically:
///   - `id`: implicit PK skip (the `id`-named field is always
///     framework-managed)
///   - `status`: `#[umbral(noform)]` — defaults to `ContactStatus::New`
///   - `ip_address`: `#[umbral(noform)]` — handler stamps from the
///     request (currently `None`; future middleware can fill it)
///   - `created_at`: `#[umbral(auto_now_add)]` — ORM stamps on insert
/// The remaining fields (`name`, `email`, `phone`, `subject`,
/// `message`) carry `#[form(...)]` validation declarations.
///
/// `Default` is required for the Form macro to fill the skipped
/// fields via `..Default::default()` in the constructor; the
/// `Choices` derive on `ContactStatus` provides `Default` itself,
/// so the struct-level `Default` derive falls out for free.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Default, Model, umbral::forms::Form)]
#[form(normalize_strings)]
pub struct ContactMessage {
    pub id: i64,
    #[umbral(string)]
    #[form(required, length(min = 1, max = 100))]
    pub name: String,
    #[form(required, email, max_length = 254)]
    pub email: String,
    // E.164 international format (`+<country><subscriber>`) — the
    // regex catches "07065" / "+1 (415) 555-1234" / "abc" and only
    // accepts canonical `+14155551234`-shaped values. The shop
    // demo uses E.164 because it round-trips across every SMS
    // provider; a softer "any digits" check would reject good
    // numbers and accept obviously-wrong ones.
    #[form(optional, phone, max_length = 30)]
    pub phone: Option<String>,
    #[form(required, length(min = 1, max = 200))]
    pub subject: String,
    #[form(required, length(min = 10, max = 5000))]
    pub message: String,
    #[umbral(choices, noform)]
    pub status: ContactStatus,
    #[umbral(noform)]
    pub ip_address: Option<String>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Note {
    pub id: i64,
    #[umbral(string)]
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Subscriber {
    pub id: i64,
    #[umbral(unique)]
    pub email: String,
    #[umbral(default = "false")]
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
    #[umbral(unique)]
    pub from_path: String,
    pub to_path: String,
    #[umbral(choices)]
    pub code: RedirectCode,
    #[umbral(default = "true")]
    pub is_active: bool,
    #[umbral(default = "0")]
    pub hits: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct SiteSetting {
    pub id: i64,
    #[umbral(string)]
    pub site_name: String,
    pub tagline: Option<String>,
    pub logo: Option<String>,
    pub favicon: Option<String>,
    pub contact_email: String,
    pub social_links: serde_json::Value,
    pub default_seo: serde_json::Value,
    pub config: serde_json::Value,
}
