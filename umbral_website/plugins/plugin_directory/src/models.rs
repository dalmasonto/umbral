//! Plugin directory models for umbral.dev.
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
use umbral::prelude::*;
use umbral_auth::AuthUser;
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
    UmbralReviewed,
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
/// validation attrs; server-managed fields carry `#[umbral(noform)]` and
/// are skipped by the Form derive. The macro requires the struct to
/// implement `Default` (it fills the skipped fields with
/// `..Default::default()`), so we derive it here too — each `Choices`
/// enum above has a `#[default]` first variant.
#[derive(
    Debug, Clone, Default, sqlx::FromRow, Serialize, Deserialize, Model, umbral::forms::Form,
)]
#[umbral(
    soft_delete,
    plugin = "plugin_directory",
    display = "Plugins",
    icon = "package"
)]
pub struct Plugin {
    #[umbral(primary_key)]
    pub id: i64,
    #[umbral(noform)]
    pub public_id: Uuid,

    pub created_by: Option<ForeignKey<AuthUser>>,

    #[umbral(unique, string, max_length = 120)]
    #[form(required, length(min = 2, max = 120))]
    pub name: String,

    #[umbral(unique, max_length = 140)]
    #[form(required, length(min = 2, max = 140))]
    pub slug: String,

    pub logo: Option<ImageField>,

    pub cover_image: Option<ImageField>,

    #[umbral(unique, max_length = 140)]
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

    #[umbral(noform, choices, index)]
    pub status: PluginStatus,

    #[umbral(noform, choices, index)]
    pub maturity: PluginMaturity,

    #[umbral(noform, choices, index)]
    pub audit_status: AuditStatus,

    #[umbral(noform, choices, index)]
    pub security_status: SecurityStatus,

    // `default = "..."` is the SQL DEFAULT clause and takes the DB
    // literal (the rename_all'd choice value), NOT the Rust path —
    // the string lands verbatim in the migration's DDL. The Rust-side
    // default (`#[default]` on the enum) is separate on purpose:
    // public form submissions construct via `..Default::default()`
    // (source=community, moderation=pending) while rows created
    // without an explicit value at the SQL level get official/approved.
    #[umbral(noform, choices, index, default = "official")]
    pub source: PluginSource,

    #[umbral(noform, choices, index, default = "approved")]
    pub moderation: PluginModeration,

    #[umbral(default = "false", index)]
    pub featured: bool,

    #[umbral(default = "0")]
    pub display_order: i32,

    /// GitHub star count, synced by a maintainer / future sync task.
    /// `None` renders as no segment on the public cards — never a
    /// fabricated number.
    #[umbral(
        noform,
        help = "GitHub stars — maintainer-synced; leave empty if unknown."
    )]
    pub github_stars: Option<i64>,

    /// crates.io download count (see planning/umbral-site.md §Good
    /// features — crates.io exposes per-crate downloads). Same
    /// None-means-hidden rule as `github_stars`.
    #[umbral(
        noform,
        help = "crates.io downloads — maintainer-synced; leave empty if unknown."
    )]
    pub downloads: Option<i64>,

    #[umbral(noform)]
    pub metadata: Option<serde_json::Value>,

    /// Reverse relation to this plugin's discussion notes. Powers the
    /// one-query `annotate_count("comment_set")` on the landing page
    /// (`annotate(n=Count("comments"))`) and
    /// `prefetch_related("comment_set")` when the rows themselves are
    /// needed. Not a column — skipped by sqlx, serde, and migrations.
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(noform, reverse_fk = "plugin")]
    pub comment_set: umbral::orm::ReverseSet<PluginComment>,

    /// Reverse relation to this plugin's feature tracker rows. Powers the
    /// `/prebuilt` page's per-plugin feature grid in one batched query via
    /// `prefetch_related("feature_set")` instead of an N+1 per-plugin
    /// reverse lookup. Not a column — skipped by sqlx, serde, migrations.
    #[sqlx(skip)]
    #[serde(skip)]
    #[umbral(noform, reverse_fk = "plugin")]
    pub feature_set: umbral::orm::ReverseSet<PluginFeature>,

    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,

    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,

    #[umbral(noform, index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

impl umbral::orm::Searchable for Plugin {
    fn kind() -> &'static str {
        "plugin"
    }
    // The directory routes plugins by `slug`, so `SearchHit.pk` carries the
    // slug for the `/plugins/{slug}` URL. The column is the `slug` field name
    // (umbral columns are always the struct field name). `title()` picks `name`;
    // `body()` searches the prose columns (name / crate_name / short_description
    // / full_content / …), dropping the `moderation` choices column.
    fn ident() -> &'static str {
        "slug"
    }
    // Only approved plugins are searchable (soft-deleted rows are excluded
    // automatically — `Plugin` is `#[umbral(soft_delete)]`). Mirrors the old
    // `render_search` filter so unapproved submissions never surface.
    fn filter_sql() -> Option<&'static str> {
        Some("moderation = 'approved'")
    }
}

// ---------------------------------------------------------------------------
// Plugin-owned feature tracker (per-plugin sub-features; admin-managed)
// ---------------------------------------------------------------------------

/// A single feature that lives inside a `Plugin` (e.g. "REST: viewsets",
/// "Admin: filters"). Admin-managed only — no public form.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    soft_delete,
    plugin = "plugin_directory",
    display = "Plugin features",
    icon = "list-checks"
)]
pub struct PluginFeature {
    pub id: i64,
    #[umbral(on_delete = "cascade")]
    pub plugin: ForeignKey<Plugin>,
    #[umbral(string, max_length = 140)]
    pub name: String,
    #[umbral(unique, max_length = 180)]
    pub slug: String,
    #[umbral(
        widget = "markdown",
        help = "Markdown — headings, lists, tables, fenced code. Rendered with `| markdown` on the public page."
    )]
    pub description: String,
    #[umbral(choices, index)]
    pub status: PluginStatus,
    #[umbral(choices, index)]
    pub maturity: PluginMaturity,
    pub release_target: Option<String>,
    pub docs_url: Option<String>,
    pub example_url: Option<String>,
    #[umbral(default = "0")]
    pub display_order: i32,
    #[umbral(default = "true", index)]
    pub visible: bool,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

/// A `Plugin`'s compatibility declaration (per Umbral version + DB
/// backend). Admin-managed only — no public form.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    soft_delete,
    plugin = "plugin_directory",
    display = "Plugin compatibility",
    icon = "badge-check"
)]
pub struct PluginCompatibility {
    pub id: i64,
    #[umbral(on_delete = "cascade")]
    pub plugin: ForeignKey<Plugin>,
    pub umbral_version: String,
    pub supported_database_backends: serde_json::Value,
    pub minimum_rust_version: Option<String>,
    pub notes: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// PluginComment (public form — plugin-threaded discussion)
// ---------------------------------------------------------------------------

/// A comment / discussion thread attached to a `Plugin`. Public
/// form: a visitor submits a body, picks a `kind`, and optionally
/// tags the plugin / Umbral / DB / OS version their note applies to.
/// Moderation status starts at [`CommentModeration::Pending`] and
/// the public site only shows [`CommentModeration::Visible`] rows.
///
/// The Form derive handles the relations directly: `plugin` is a
/// `ModelChoice` (FK), `kind` a `Select` (choices); `author` /
/// `moderation` / `pinned` / `author_label` / `parent` stay
/// server-managed via `#[umbral(noform)]`. Every remaining field is
/// `Default`-derivable (`ForeignKey<T>: Default` lands the id-0
/// placeholder), so the hand-rolled `Default` is gone.
#[derive(
    Debug, Clone, Default, sqlx::FromRow, Serialize, Deserialize, Model, umbral::forms::Form,
)]
#[umbral(
    soft_delete,
    plugin = "plugin_directory",
    display = "Plugin comments",
    icon = "message-square"
)]
pub struct PluginComment {
    pub id: i64,

    #[umbral(on_delete = "cascade")]
    pub plugin: ForeignKey<Plugin>,

    #[umbral(noform, on_delete = "set_null")]
    pub author: Option<ForeignKey<AuthUser>>,

    #[form(required, length(min = 5, max = 5_000))]
    #[umbral(widget = "markdown", help = "Markdown supported.")]
    pub body: String,

    // SQL DEFAULT takes the DB literal, not the Rust path (see the
    // matching note on Plugin.source). Public form field → a Select.
    #[umbral(choices, default = "general")]
    pub kind: CommentKind,

    // Server-managed: a visitor must not pick their own moderation
    // status. noform keeps it off the public form; the default is
    // `pending` until a moderator acts.
    #[umbral(noform, choices, default = "pending")]
    pub moderation: CommentModeration,

    /// Set to true by an Umbral maintainer or the plugin's author to
    /// pin the comment to the top of the thread. Admin-only.
    #[umbral(noform, default = "false")]
    pub pinned: bool,

    /// Optional self-identification ("maintainer of plugin X" / etc.).
    /// Admin-curated; we don't want random visitors claiming it.
    #[umbral(noform, max_length = 120)]
    pub author_label: Option<String>,

    /// Reply-to pointer for nested comments. Top-level comments have
    /// `parent = None`. Admin-managed once visible — the form layer
    /// leaves it null.
    #[umbral(noform, on_delete = "set_null")]
    pub parent: Option<ForeignKey<PluginComment>>,

    /// The plugin version the comment is tagged with (e.g. "1.4.2").
    #[form(optional, length(max = 40))]
    pub plugin_version: Option<String>,

    /// The Umbral version the comment is tagged with (e.g. "0.0.1").
    #[form(optional, length(max = 40))]
    pub umbral_version: Option<String>,

    /// The database backend the comment is tagged with
    /// ("postgres" / "sqlite"). Free-text; moderation can clean.
    #[form(optional, length(max = 40))]
    pub database_backend: Option<String>,

    /// The operating system the comment is tagged with
    /// ("linux" / "macos" / "windows").
    #[form(optional, length(max = 40))]
    pub operating_system: Option<String>,

    /// Whether this comment is an issue (a bug / problem report) rather
    /// than a plain discussion note. Issues are moderatable and
    /// resolvable; notes are not. Server-managed (the report path sets
    /// it, the note path leaves it false).
    #[umbral(noform, default = "false", index)]
    pub is_issue: bool,

    /// Whether this comment is publicly visible in the thread. Defaults
    /// to true; a moderator can flip it to hide an issue/note without a
    /// hard delete. Distinct from `moderation` (the queue status).
    #[umbral(noform, default = "true", index)]
    pub is_public: bool,

    /// Whether this issue has been resolved. Only meaningful when
    /// `is_issue` is true; a moderator marks an issue resolved once the
    /// underlying problem is fixed.
    #[umbral(noform, default = "false", index)]
    pub is_resolved: bool,

    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,

    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,

    #[umbral(noform, index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

// ---------------------------------------------------------------------------
// PluginModerator (per-plugin moderation grant)
// ---------------------------------------------------------------------------

/// A user granted moderation rights over a single `Plugin`'s Notes and
/// Issues. The plugin's `created_by` owner moderates implicitly; this
/// table lists the *additional* users the owner has added as
/// moderators. `(plugin, user)` is unique so a user can't be added
/// twice. Admin / owner-managed only — no public form.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    plugin = "plugin_directory",
    display = "Plugin moderators",
    icon = "shield-check",
    unique_together = [["plugin", "user"]]
)]
pub struct PluginModerator {
    #[umbral(primary_key)]
    pub id: i64,

    #[umbral(on_delete = "cascade")]
    pub plugin: ForeignKey<Plugin>,

    #[umbral(on_delete = "cascade")]
    pub user: ForeignKey<AuthUser>,

    /// The user who granted this moderation right (the plugin owner or
    /// another moderator). `set_null` so removing the granter's account
    /// doesn't revoke the grant.
    #[umbral(on_delete = "set_null")]
    pub added_by: Option<ForeignKey<AuthUser>>,

    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod form_tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::sync::OnceCell;
    use umbral::forms::FormValidate;
    use umbral::orm::Model;

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
    // No-op storage so the boot-time `field.storage_backend` system check
    // passes — `Plugin` declares `logo` / `cover_image` image fields,
    // which require a registered Storage. These form tests never upload;
    // this only satisfies the check (same pattern as render_pages.rs's
    // TestStorage).
    struct NoopStorage;

    #[umbral::storage::async_trait]
    impl umbral::storage::Storage for NoopStorage {
        async fn store(
            &self,
            filename: &str,
            _content_type: &str,
            _bytes: &[u8],
        ) -> Result<umbral::storage::StoredFile, umbral::storage::StorageError> {
            let key = filename.to_string();
            let url = self.url(&key);
            Ok(umbral::storage::StoredFile { key, url, size: 0 })
        }
        async fn retrieve(&self, _key: &str) -> Result<Vec<u8>, umbral::storage::StorageError> {
            Err(umbral::storage::StorageError::NotFound)
        }
        async fn delete(&self, _key: &str) -> Result<(), umbral::storage::StorageError> {
            Ok(())
        }
        fn url(&self, key: &str) -> String {
            format!("/media/{key}")
        }
    }

    // A storage-declaring plugin: the `field.storage_backend` system
    // check reads the registered plugins' `provides_storage()` flag (not
    // the ambient global), so satisfying the check for `Plugin`'s image
    // fields needs a plugin returning `true` here — registering the
    // backend alone isn't enough.
    #[derive(Debug, Default, Clone)]
    struct StorageOnly;

    impl umbral::plugin::Plugin for StorageOnly {
        fn name(&self) -> &'static str {
            "plugin_directory_form_test"
        }
        fn models(&self) -> Vec<umbral::migrate::ModelMeta> {
            Vec::new()
        }
        fn provides_storage(&self) -> bool {
            true
        }
    }

    static BOOT: OnceCell<()> = OnceCell::const_new();
    async fn boot() {
        BOOT.get_or_init(|| async {
            let pool = umbral::db::connect_sqlite("sqlite::memory:").await.unwrap();
            // Force the settings backend to sqlite to match the in-memory
            // pool — the ambient umbral.toml / env may default to postgres.
            let mut settings = umbral::Settings::from_env().unwrap();
            settings.database_url = "sqlite::memory:".to_string();
            // Register a real storage backend (resolves `url(key)`) AND a
            // plugin that declares the capability, so the image-field
            // system check for `Plugin.logo` / `cover_image` passes.
            let _ = umbral::storage::set_storage(std::sync::Arc::new(NoopStorage));
            umbral::App::builder()
                .settings(settings)
                .database("default", pool.clone())
                .model::<Plugin>()
                .model::<PluginComment>()
                .plugin(StorageOnly::default())
                .build()
                .unwrap();
            // Minimal table for the FK existence probe — only the `id`
            // column matters for validate(). The table name is
            // Plugin::TABLE (plugin-name-prefixed by the derive), so the
            // probe's DynQuerySet::for_meta targets the right table.
            // `Plugin` is `#[umbral(soft_delete)]`, so the FK existence
            // probe scopes its lookup with `WHERE deleted_at IS NULL`.
            // The probe table therefore needs a `deleted_at` column (left
            // NULL on the seed row) or the lookup finds no live record.
            let create = format!(
                "CREATE TABLE {t} (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, deleted_at TEXT)",
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
