//! Plugin directory models for umbra.dev.
//!
//! The directory is a single canonical `Plugin` model (no
//! `OfficialPlugin` prefix). Source is one enum variant
//! ([`PluginSource`]) covering official, community, experimental,
//! and deprecated plugins — there's no parallel schema, just a
//! `source` column. Form derives let the public "Submit a plugin"
//! surface write directly into this table with `source` defaulted
//! to [`PluginSource::Community`] and moderation status driving
//! what the public site renders.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbra::prelude::*;
use umbra_auth::AuthUser;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PluginStatus {
    #[default]
    Shipped,
    Usable,
    Experimental,
    InProgress,
    Planned,
    Deprecated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PluginMaturity {
    #[default]
    Stable,
    Beta,
    Alpha,
    Design,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AuditStatus {
    #[default]
    NotReviewed,
    SelfReviewed,
    UmbraReviewed,
    ThirdPartyReviewed,
    NeedsReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SecurityStatus {
    #[default]
    Normal,
    Watch,
    Advisory,
    Deprecated,
    Blocked,
}

/// Where a plugin in the directory comes from. The only differentiator
/// between "official" and "community" plugins is this enum — the rest
/// of the schema is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PluginSource {
    Official,
    #[default]
    Community,
    Experimental,
    Deprecated,
}

/// Moderation lifecycle for community-submitted rows (a `Plugin`
/// submitted with `source = Community` and `moderation = Pending` is
/// invisible to the public site until an admin approves it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PluginModeration {
    #[default]
    Pending,
    Approved,
    Rejected,
    NeedsChanges,
}

/// What kind of post a [`PluginComment`] is. Drives the comment
/// thread UI's icon + filter and the OpenAPI `enum` schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CommentKind {
    #[default]
    General,
    Question,
    UsageNote,
    CompatibilityNote,
    MigrationNote,
    MaintainerReply,
}

/// Lifecycle for plugin comments. Hidden / flagged / deleted are
/// moderator-set; locked is the maintainer freezing a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CommentModeration {
    #[default]
    Pending,
    Visible,
    Hidden,
    Flagged,
    Deleted,
    Locked,
}

// ---------------------------------------------------------------------------
// Plugin (the canonical row)
// ---------------------------------------------------------------------------

/// One plugin in the directory. Public-facing fields carry `#[form(...)]`
/// validation attrs; server-managed fields carry `#[umbra(noform)]` and
/// are skipped by the Form derive. The macro requires the struct to
/// implement `Default` (it fills the skipped fields with
/// `..Default::default()`), so we derive it here too — each `Choices`
/// enum above has a `#[default]` first variant.
#[derive(
    Debug, Clone, Default, sqlx::FromRow, Serialize, Deserialize, Model, umbra::forms::Form,
)]
#[umbra(soft_delete, plugin = "plugin_directory", display = "Plugins", icon = "package")]
pub struct Plugin {
    pub id: i64,
    #[umbra(noform)]
    pub public_id: Uuid,

    #[umbra(unique, string, max_length = 120)]
    #[form(required, length(min = 2, max = 120))]
    pub name: String,

    #[umbra(unique, max_length = 140)]
    #[form(required, length(min = 2, max = 140))]
    pub slug: String,

    #[umbra(unique, max_length = 140)]
    #[form(required, length(min = 2, max = 140))]
    pub crate_name: String,

    #[form(required, length(min = 2, max = 120))]
    pub author: String,

    #[form(required, length(min = 10, max = 400))]
    pub short_description: String,

    #[form(required, length(min = 20, max = 20_000))]
    pub full_content: String,

    #[form(required, length(min = 1, max = 4_000))]
    pub installation_commands: String,

    #[form(optional, length(max = 4_000))]
    pub setup_notes: Option<String>,

    #[form(optional, url, max_length = 400)]
    pub docs_url: Option<String>,

    #[form(optional, url, max_length = 400)]
    pub source_url: Option<String>,

    #[form(optional, url, max_length = 400)]
    pub issue_tracker_url: Option<String>,

    #[form(optional, length(max = 40))]
    pub version: Option<String>,

    #[form(optional, length(max = 80))]
    pub license: Option<String>,

    #[umbra(noform, choices, index)]
    pub status: PluginStatus,

    #[umbra(noform, choices, index)]
    pub maturity: PluginMaturity,

    #[umbra(noform, choices, index)]
    pub audit_status: AuditStatus,

    #[umbra(noform, choices, index)]
    pub security_status: SecurityStatus,

    // `default = "..."` is the SQL DEFAULT clause and takes the DB
    // literal (the rename_all'd choice value), NOT the Rust path —
    // the string lands verbatim in the migration's DDL. The Rust-side
    // default (`#[default]` on the enum) is separate on purpose:
    // public form submissions construct via `..Default::default()`
    // (source=community, moderation=pending) while rows created
    // without an explicit value at the SQL level get official/approved.
    #[umbra(noform, choices, index, default = "official")]
    pub source: PluginSource,

    #[umbra(noform, choices, index, default = "approved")]
    pub moderation: PluginModeration,

    #[umbra(noform, default = "false", index)]
    pub featured: bool,

    #[umbra(noform, default = "0")]
    pub display_order: i32,

    #[umbra(noform)]
    pub metadata: Option<serde_json::Value>,

    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,

    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,

    #[umbra(noform, index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// Plugin-owned feature tracker (per-plugin sub-features; admin-managed)
// ---------------------------------------------------------------------------

/// A single feature that lives inside a `Plugin` (e.g. "REST: viewsets",
/// "Admin: filters"). Admin-managed only — no public form.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(
    soft_delete,
    plugin = "plugin_directory",
    display = "Plugin features",
    icon = "list-checks"
)]
pub struct PluginFeature {
    pub id: i64,
    #[umbra(on_delete = "cascade")]
    pub plugin: ForeignKey<Plugin>,
    #[umbra(string, max_length = 140)]
    pub name: String,
    #[umbra(unique, max_length = 180)]
    pub slug: String,
    #[umbra(widget = "markdown", help = "Markdown — headings, lists, tables, fenced code. Rendered with `| markdown` on the public page.")]
    pub description: String,
    #[umbra(choices, index)]
    pub status: PluginStatus,
    #[umbra(choices, index)]
    pub maturity: PluginMaturity,
    pub release_target: Option<String>,
    pub docs_url: Option<String>,
    pub example_url: Option<String>,
    #[umbra(default = "0")]
    pub display_order: i32,
    #[umbra(default = "true", index)]
    pub visible: bool,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

/// A `Plugin`'s compatibility declaration (per Umbra version + DB
/// backend). Admin-managed only — no public form.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(
    soft_delete,
    plugin = "plugin_directory",
    display = "Plugin compatibility",
    icon = "badge-check"
)]
pub struct PluginCompatibility {
    pub id: i64,
    #[umbra(on_delete = "cascade")]
    pub plugin: ForeignKey<Plugin>,
    pub umbra_version: String,
    pub supported_database_backends: serde_json::Value,
    pub minimum_rust_version: Option<String>,
    pub notes: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// PluginComment (public form — plugin-threaded discussion)
// ---------------------------------------------------------------------------

/// A comment / discussion thread attached to a `Plugin`. Public
/// form: a visitor submits a body, picks a `kind`, and optionally
/// tags the plugin / Umbra / DB / OS version their note applies to.
/// Moderation status starts at [`CommentModeration::Pending`] and
/// the public site only shows [`CommentModeration::Visible`] rows.
#[derive(
    Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model, umbra::forms::Form,
)]
#[umbra(
    soft_delete,
    plugin = "plugin_directory",
    display = "Plugin comments",
    icon = "message-square"
)]
pub struct PluginComment {
    pub id: i64,

    #[umbra(noform, on_delete = "cascade")]
    pub plugin: ForeignKey<Plugin>,

    #[umbra(noform, on_delete = "set_null")]
    pub author: Option<ForeignKey<AuthUser>>,

    #[form(required, length(min = 5, max = 5_000))]
    #[umbra(widget = "markdown", help = "Markdown supported.")]
    pub body: String,

    // SQL DEFAULT takes the DB literal, not the Rust path (see the
    // matching note on Plugin.source).
    #[umbra(noform, choices, default = "general")]
    pub kind: CommentKind,

    #[umbra(noform, choices, default = "pending")]
    pub moderation: CommentModeration,

    /// Set to true by an Umbra maintainer or the plugin's author to
    /// pin the comment to the top of the thread. Admin-only.
    #[umbra(noform, default = "false")]
    pub pinned: bool,

    /// Optional self-identification ("maintainer of plugin X" / etc.).
    /// Admin-curated; we don't want random visitors claiming it.
    #[umbra(noform, max_length = 120)]
    pub author_label: Option<String>,

    /// Reply-to pointer for nested comments. Top-level comments have
    /// `parent = None`. Admin-managed once visible — the form layer
    /// leaves it null.
    #[umbra(noform, on_delete = "set_null")]
    pub parent: Option<ForeignKey<PluginComment>>,

    /// The plugin version the comment is tagged with (e.g. "1.4.2").
    #[form(optional, length(max = 40))]
    pub plugin_version: Option<String>,

    /// The Umbra version the comment is tagged with (e.g. "0.0.1").
    #[form(optional, length(max = 40))]
    pub umbra_version: Option<String>,

    /// The database backend the comment is tagged with
    /// ("postgres" / "sqlite"). Free-text; moderation can clean.
    #[form(optional, length(max = 40))]
    pub database_backend: Option<String>,

    /// The operating system the comment is tagged with
    /// ("linux" / "macos" / "windows").
    #[form(optional, length(max = 40))]
    pub operating_system: Option<String>,

    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,

    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,

    #[umbra(noform, index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

// `ForeignKey<T>` deliberately doesn't implement `Default` (the
// framework can't guess what zero or null means for an FK), so we
// can't use `#[derive(Default)]` on `PluginComment` — three of its
// fields are FKs. The form macro still requires `Default` (it
// constructs the value with `..Default::default()` and then fills
// in the user-submittable fields), so we hand-roll the impl. The
// FK placeholders are never read because every FK field on this
// struct is `#[umbra(noform)]` — the form layer leaves them alone
// and the handler fills them from the URL/auth context.
impl Default for PluginComment {
    fn default() -> Self {
        Self {
            id: 0,
            plugin: ForeignKey::new(0),
            author: None,
            body: String::new(),
            kind: CommentKind::default(),
            moderation: CommentModeration::default(),
            pinned: false,
            author_label: None,
            parent: None,
            plugin_version: None,
            umbra_version: None,
            database_backend: None,
            operating_system: None,
            created_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
            updated_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
            deleted_at: None,
        }
    }
}
