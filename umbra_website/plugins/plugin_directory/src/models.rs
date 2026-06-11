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
#[umbra(
    soft_delete,
    plugin = "plugin_directory",
    display = "Plugins",
    icon = "package"
)]
pub struct Plugin {
    #[umbra(primary_key)]
    pub id: i64,
    #[umbra(noform)]
    pub public_id: Uuid,

    pub created_by: Option<ForeignKey<AuthUser>>,

    #[umbra(unique, string, max_length = 120)]
    #[form(required, length(min = 2, max = 120))]
    pub name: String,

    #[umbra(unique, max_length = 140)]
    #[form(required, length(min = 2, max = 140))]
    pub slug: String,

    #[form(optional, length(max = 4_000))]
    pub logo: Option<String>,

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

    #[umbra(default = "false", index)]
    pub featured: bool,

    #[umbra(default = "0")]
    pub display_order: i32,

    /// GitHub star count, synced by a maintainer / future sync task.
    /// `None` renders as no segment on the public cards — never a
    /// fabricated number.
    #[umbra(
        noform,
        help = "GitHub stars — maintainer-synced; leave empty if unknown."
    )]
    pub github_stars: Option<i64>,

    /// crates.io download count (see planning/umbra-site.md §Good
    /// features — crates.io exposes per-crate downloads). Same
    /// None-means-hidden rule as `github_stars`.
    #[umbra(
        noform,
        help = "crates.io downloads — maintainer-synced; leave empty if unknown."
    )]
    pub downloads: Option<i64>,

    #[umbra(noform)]
    pub metadata: Option<serde_json::Value>,

    /// Reverse relation to this plugin's discussion notes. Powers the
    /// one-query `annotate_count("comment_set")` on the landing page
    /// (Django's `annotate(n=Count("comments"))`) and
    /// `prefetch_related("comment_set")` when the rows themselves are
    /// needed. Not a column — skipped by sqlx, serde, and migrations.
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbra(noform, reverse_fk = "plugin")]
    pub comment_set: umbra::orm::ReverseSet<PluginComment>,

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
    #[umbra(
        widget = "markdown",
        help = "Markdown — headings, lists, tables, fenced code. Rendered with `| markdown` on the public page."
    )]
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
///
/// The Form derive handles the relations directly: `plugin` is a
/// `ModelChoice` (FK), `kind` a `Select` (choices); `author` /
/// `moderation` / `pinned` / `author_label` / `parent` stay
/// server-managed via `#[umbra(noform)]`. Every remaining field is
/// `Default`-derivable (`ForeignKey<T>: Default` lands the id-0
/// placeholder), so the hand-rolled `Default` is gone.
#[derive(
    Debug, Clone, Default, sqlx::FromRow, Serialize, Deserialize, Model, umbra::forms::Form,
)]
#[umbra(
    soft_delete,
    plugin = "plugin_directory",
    display = "Plugin comments",
    icon = "message-square"
)]
pub struct PluginComment {
    pub id: i64,

    #[umbra(on_delete = "cascade")]
    pub plugin: ForeignKey<Plugin>,

    #[umbra(noform, on_delete = "set_null")]
    pub author: Option<ForeignKey<AuthUser>>,

    #[form(required, length(min = 5, max = 5_000))]
    #[umbra(widget = "markdown", help = "Markdown supported.")]
    pub body: String,

    // SQL DEFAULT takes the DB literal, not the Rust path (see the
    // matching note on Plugin.source). Public form field → a Select.
    #[umbra(choices, default = "general")]
    pub kind: CommentKind,

    // Server-managed: a visitor must not pick their own moderation
    // status. noform keeps it off the public form; the default is
    // `pending` until a moderator acts.
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

#[cfg(test)]
mod form_tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::sync::OnceCell;
    use umbra::forms::FormValidate;
    use umbra::orm::Model;

    fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // The ambient ORM pool is a process-wide OnceLock — both tests must
    // share ONE boot so the FK existence probe (which dispatches through
    // the ambient pool) sees the seeded `plugin` row. Seed id=1; the
    // reject test points at a nonexistent id (9999) so it doesn't need
    // its own DB.
    static BOOT: OnceCell<()> = OnceCell::const_new();
    async fn boot() {
        BOOT.get_or_init(|| async {
            let pool = umbra::db::connect_sqlite("sqlite::memory:").await.unwrap();
            // Force the settings backend to sqlite to match the in-memory
            // pool — the ambient umbra.toml / env may default to postgres.
            let mut settings = umbra::Settings::from_env().unwrap();
            settings.database_url = "sqlite::memory:".to_string();
            umbra::App::builder()
                .settings(settings)
                .database("default", pool.clone())
                .model::<Plugin>()
                .model::<PluginComment>()
                .build()
                .unwrap();
            // Minimal table for the FK existence probe — only the `id`
            // column matters for validate(). The table name is
            // Plugin::TABLE (plugin-name-prefixed by the derive), so the
            // probe's DynQuerySet::for_meta targets the right table.
            let create = format!(
                "CREATE TABLE {t} (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
                t = Plugin::TABLE
            );
            sqlx::query(&create).execute(&pool).await.unwrap();
            let insert = format!(
                "INSERT INTO {t} (id, name) VALUES (1, 'demo')",
                t = Plugin::TABLE
            );
            sqlx::query(&insert).execute(&pool).await.unwrap();
        })
        .await;
    }

    // The acceptance case: PluginComment derives Form directly off the
    // Model struct. `plugin` is a ModelChoice (FK, existence-checked),
    // `kind` a Select (choices), and the server-managed fields are
    // skipped. A submitted comment validates with the FK bound and the
    // choice decoded back into the enum.
    #[tokio::test]
    async fn plugin_comment_form_submits_with_fk_and_choices() {
        boot().await;
        let comment = PluginComment::validate(&data(&[
            ("plugin", "1"),
            ("body", "Great plugin, works on sqlite."),
            ("kind", "usage_note"),
        ]))
        .await
        .expect("comment validates with FK + choices");
        assert_eq!(comment.plugin.id(), 1, "FK bound to the submitted id");
        assert_eq!(
            comment.kind,
            CommentKind::UsageNote,
            "choices decoded back into the enum"
        );
        // Server-managed fields took their defaults (form left them).
        assert_eq!(comment.moderation, CommentModeration::default());
        assert!(comment.author.is_none(), "author filled by the handler");
    }

    // A nonexistent plugin id is rejected: the FK existence probe finds
    // no matching row and keys the error to the `plugin` field.
    #[tokio::test]
    async fn plugin_comment_form_rejects_nonexistent_plugin() {
        boot().await;
        let err = PluginComment::validate(&data(&[
            ("plugin", "9999"),
            ("body", "points at nobody"),
            ("kind", "general"),
        ]))
        .await
        .expect_err("nonexistent plugin FK rejected");
        assert!(
            err.fields.contains_key("plugin"),
            "error keyed to the FK field"
        );
    }
}
