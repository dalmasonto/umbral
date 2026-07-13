//! Render tests for the moderation UI (Task C).
//!
//! `cargo build` cannot catch Jinja template errors or visibility-gating
//! bugs — those only surface at render time. This boots a minimal app
//! against an in-memory SQLite DB, registers the real template dirs (the
//! site's `templates/` for `base.html` plus the plugin's own templates),
//! seeds an owner / a moderator / a stranger plus notes and issues through
//! the ORM, then renders `/plugins/{slug}` via `render_detail_for` as:
//!
//! * the OWNER → the moderator-management section, the per-note moderation
//!   actions, and the issue Resolve control all appear.
//! * an ANONYMOUS visitor → none of those controls appear, but the notes
//!   and the issue statuses still render (read-only).
//!
//! Test-only raw DDL (`ensure_tables`) is the sanctioned exception to "no
//! raw SQL in plugins" (tests bypass `make` / `run`). Every row-level
//! read/write the page does still goes through the ORM.

use std::path::PathBuf;

use plugin_directory::models::{
    AuditStatus, CommentKind, CommentModeration, Plugin, PluginComment, PluginCompatibility,
    PluginFeature, PluginMaturity, PluginModeration, PluginModerator, PluginSource, PluginStatus,
    SecurityStatus,
};
use plugin_directory::{add_moderator_logic, render_detail_for};
use umbral::migrate::ModelMeta;
use umbral::orm::{ForeignKey, Model};
use umbral::plugin::{Plugin as PluginTrait, PluginError};

/// A no-op storage backend so the boot-time system check passes — the
/// `Plugin` model declares `logo` / `cover_image` image fields, which
/// require a registered `Storage`. This test never uploads.
struct TestStorage;

#[umbral::storage::async_trait]
impl umbral::storage::Storage for TestStorage {
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

/// Contributes the plugin's template directory + declares storage so the
/// `Plugin` image-field system check passes.
#[derive(Debug, Default, Clone)]
struct TemplatesOnly;

impl PluginTrait for TemplatesOnly {
    fn name(&self) -> &'static str {
        "plugin_directory_moderation_render_test"
    }
    fn models(&self) -> Vec<ModelMeta> {
        Vec::new()
    }
    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }
    fn provides_storage(&self) -> bool {
        true
    }
    fn on_ready(&self, _ctx: &umbral::plugin::AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

/// Boot the ambient pool + model registry + template engine once, create
/// the tables and seed the rows the render cases need.
async fn boot() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Mutex;

    static BOOTED: AtomicBool = AtomicBool::new(false);
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();

    if BOOTED.load(Ordering::Acquire) {
        return;
    }
    let mutex = LOCK.get_or_init(|| Mutex::new(()));
    let _guard = mutex.lock().await;
    if BOOTED.load(Ordering::Acquire) {
        return;
    }

    let _ = umbral::storage::set_storage(std::sync::Arc::new(TestStorage));

    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    // The site root `templates/` holds `base.html` that the page extends;
    // the plugin contributes its own page templates.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let site_templates = manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("templates");

    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Plugin>()
        .model::<PluginFeature>()
        .model::<PluginCompatibility>()
        .model::<PluginComment>()
        .model::<PluginModerator>()
        .templates_dir(site_templates)
        .plugin(TemplatesOnly::default())
        .plugin(umbral_realtime::RealtimePlugin::default())
        .build()
        .expect("App::build");

    ensure_tables(&pool).await;
    seed(&pool).await;

    BOOTED.store(true, Ordering::Release);
}

/// Test-only schema for the tables this render path touches.
async fn ensure_tables(pool: &sqlx::SqlitePool) {
    let stmts = [
        // `auth_user` is the FK target for `PluginModerator.user` + the
        // username the roster renders. The render joins moderators to it.
        "CREATE TABLE auth_user (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            email TEXT NOT NULL DEFAULT '',
            password_hash TEXT NOT NULL DEFAULT '',
            is_active INTEGER NOT NULL DEFAULT 1,
            is_staff INTEGER NOT NULL DEFAULT 0,
            is_superuser INTEGER NOT NULL DEFAULT 0,
            date_joined TEXT NOT NULL DEFAULT '',
            last_login TEXT,
            email_verified_at TEXT
        )"
        .to_string(),
        format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                public_id TEXT NOT NULL,
                created_by INTEGER,
                name TEXT NOT NULL,
                slug TEXT NOT NULL,
                logo TEXT,
                cover_image TEXT,
                crate_name TEXT NOT NULL,
                author TEXT NOT NULL,
                short_description TEXT NOT NULL,
                full_content TEXT NOT NULL,
                installation_commands TEXT NOT NULL,
                setup_notes TEXT,
                docs_url TEXT,
                source_url TEXT,
                issue_tracker_url TEXT,
                version TEXT,
                license TEXT,
                status TEXT NOT NULL,
                maturity TEXT NOT NULL,
                audit_status TEXT NOT NULL,
                security_status TEXT NOT NULL,
                source TEXT NOT NULL,
                moderation TEXT NOT NULL,
                featured INTEGER NOT NULL DEFAULT 0,
                display_order INTEGER NOT NULL DEFAULT 0,
                github_stars INTEGER,
                downloads INTEGER,
                metadata TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            )",
            t = Plugin::TABLE
        ),
        format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                plugin INTEGER NOT NULL REFERENCES {pt}(id),
                name TEXT NOT NULL,
                slug TEXT NOT NULL,
                description TEXT NOT NULL,
                status TEXT NOT NULL,
                maturity TEXT NOT NULL,
                release_target TEXT,
                docs_url TEXT,
                example_url TEXT,
                display_order INTEGER NOT NULL DEFAULT 0,
                visible INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            )",
            t = PluginFeature::TABLE,
            pt = Plugin::TABLE
        ),
        format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                plugin INTEGER NOT NULL REFERENCES {pt}(id),
                umbral_version TEXT NOT NULL,
                supported_database_backends TEXT NOT NULL,
                minimum_rust_version TEXT,
                notes TEXT,
                verified_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            )",
            t = PluginCompatibility::TABLE,
            pt = Plugin::TABLE
        ),
        format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                plugin INTEGER NOT NULL REFERENCES {pt}(id),
                user INTEGER NOT NULL REFERENCES auth_user(id),
                added_by INTEGER REFERENCES auth_user(id),
                created_at TEXT NOT NULL,
                deleted_at TEXT,
                UNIQUE (plugin, user)
            )",
            t = PluginModerator::TABLE,
            pt = Plugin::TABLE
        ),
        format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                plugin INTEGER NOT NULL REFERENCES {pt}(id),
                author INTEGER,
                body TEXT NOT NULL,
                kind TEXT NOT NULL,
                moderation TEXT NOT NULL,
                pinned INTEGER NOT NULL DEFAULT 0,
                author_label TEXT,
                parent INTEGER,
                plugin_version TEXT,
                umbral_version TEXT,
                database_backend TEXT,
                operating_system TEXT,
                is_issue INTEGER NOT NULL DEFAULT 0,
                is_public INTEGER NOT NULL DEFAULT 1,
                is_resolved INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            )",
            t = PluginComment::TABLE,
            pt = Plugin::TABLE
        ),
    ];
    for sql in stmts {
        sqlx::query(&sql).execute(pool).await.expect("CREATE TABLE");
    }
}

/// Seed an owner (alice, id 1), a moderator-to-be (bob, id 2), an approved
/// plugin owned by alice, a visible community note, and one open issue.
async fn seed(pool: &sqlx::SqlitePool) {
    let now = chrono::Utc::now().to_rfc3339();
    for (id, name) in [(1, "alice"), (2, "bob")] {
        sqlx::query(
            "INSERT INTO auth_user (id, username, email, is_active, date_joined) \
             VALUES (?, ?, ?, 1, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(format!("{name}@example.com"))
        .bind(&now)
        .execute(pool)
        .await
        .expect("seed user");
    }

    let mut p = Plugin::default();
    p.name = "Moderated Plugin".to_string();
    p.slug = "moderated-plugin".to_string();
    p.crate_name = "umbral-moderated".to_string();
    p.author = "@alice".to_string();
    p.short_description = "a plugin alice owns and moderates".to_string();
    p.full_content = "## Moderated\n\nThe moderation UI mounts on this page.".to_string();
    p.installation_commands = "umbral add umbral-moderated".to_string();
    p.status = PluginStatus::Usable;
    p.maturity = PluginMaturity::Beta;
    p.audit_status = AuditStatus::NotReviewed;
    p.security_status = SecurityStatus::Normal;
    p.source = PluginSource::Community;
    p.moderation = PluginModeration::Approved;
    p.created_by = Some(ForeignKey::new(1));
    let p = Plugin::objects().create(p).await.expect("create plugin");

    // A visible top-level discussion note.
    let mut note = PluginComment::default();
    note.plugin = ForeignKey::new(p.id);
    note.body = "MODERATION_NOTE_MARKER works on Postgres.".to_string();
    note.kind = CommentKind::UsageNote;
    note.moderation = CommentModeration::Visible;
    note.author_label = Some("Visitor".to_string());
    note.is_issue = false;
    PluginComment::objects()
        .create(note)
        .await
        .expect("create note");

    // An open, public issue (a bug report).
    let mut issue = PluginComment::default();
    issue.plugin = ForeignKey::new(p.id);
    issue.body = "[bug] MODERATION_ISSUE_MARKER crash on migrate.".to_string();
    issue.kind = CommentKind::General;
    issue.moderation = CommentModeration::Pending;
    issue.author_label = Some("Issue report".to_string());
    issue.is_issue = true;
    issue.is_public = true;
    issue.is_resolved = false;
    PluginComment::objects()
        .create(issue)
        .await
        .expect("create issue");

    // Grant bob (id 2) a moderator seat so the roster renders a row for the
    // owner view.
    let plugin = Plugin::objects()
        .filter(plugin_directory::models::plugin::SLUG.eq("moderated-plugin"))
        .first()
        .await
        .expect("query plugin")
        .expect("plugin exists");
    add_moderator_logic(&plugin, "bob", 1)
        .await
        .expect("grant bob a moderator seat");
}

// (a) Rendered as the OWNER: the moderator-management section, the
// per-note moderation actions, and the issue Resolve control all appear.
#[tokio::test]
async fn owner_view_shows_full_moderation_ui() {
    boot().await;

    let html = render_detail_for("moderated-plugin", false, Some(1))
        .await
        .expect("render ok")
        .expect("the plugin exists");

    // The page still renders the normal content.
    assert!(html.contains("Moderated Plugin"), "the plugin name renders");
    assert!(
        html.contains("MODERATION_NOTE_MARKER"),
        "the visible note still renders for the owner"
    );

    // --- Moderator roster (owner-only) ---
    assert!(
        html.contains("data-moderator-panel"),
        "the owner sees the moderator-management panel"
    );
    // The add-moderator form POSTs to the Task B add route. Static slashes in
    // the template render literally; only an interpolated value is escaped.
    assert!(
        html.contains("action=\"/plugins/moderated-plugin/moderators\""),
        "the add-moderator form targets the /moderators route"
    );
    assert!(
        html.contains("name=\"username\""),
        "the add-moderator form has a username input"
    );
    // The seeded moderator bob is listed with a remove control pointing at
    // the per-user remove route.
    assert!(
        html.contains("@bob"),
        "the seeded moderator is listed in the roster"
    );
    assert!(
        html.contains("action=\"/plugins/moderated-plugin/moderators/2/remove\""),
        "the roster renders a remove control for the moderator"
    );

    // --- Per-note moderation actions ---
    assert!(
        html.contains("data-mod-action=\"hide\""),
        "the owner sees the per-note Hide action"
    );
    assert!(
        html.contains("data-mod-action=\"flag\""),
        "the owner sees the per-note Flag action"
    );
    assert!(
        html.contains("/comments/") && html.contains("/moderate\""),
        "the note action POSTs to the moderate route"
    );

    // --- Issues tab: the issue + its status + the Resolve control ---
    assert!(
        html.contains("MODERATION_ISSUE_MARKER"),
        "the reported issue renders in the Issues tab"
    );
    assert!(
        html.contains("data-issue-status=\"open\""),
        "the open issue shows its open status"
    );
    assert!(
        html.contains("data-issue-action=\"resolve\""),
        "the owner sees a Resolve control on the open issue"
    );
    assert!(
        html.contains("action=\"/plugins/moderated-plugin/issues/2/resolve\""),
        "the Resolve control POSTs to the resolve route"
    );
}

// (b) Rendered as an ANONYMOUS / non-moderator visitor: none of the
// moderation controls appear, but the notes + issue statuses still render.
#[tokio::test]
async fn stranger_view_hides_all_moderation_controls() {
    boot().await;

    let html = render_detail_for("moderated-plugin", false, None)
        .await
        .expect("render ok")
        .expect("the plugin exists");

    // The read-only content still renders.
    assert!(html.contains("Moderated Plugin"), "the plugin name renders");
    assert!(
        html.contains("MODERATION_NOTE_MARKER"),
        "the visible note renders for the stranger"
    );
    assert!(
        html.contains("MODERATION_ISSUE_MARKER"),
        "the public issue renders for the stranger"
    );
    assert!(
        html.contains("data-issue-status=\"open\""),
        "the issue status still renders read-only"
    );

    // --- None of the controls appear ---
    // (Assert on route + form markup, not bare attribute substrings: the
    // page's <style> block legitimately mentions `data-mod-action="flag"` in
    // a CSS selector, so a substring check there would false-positive.)
    assert!(
        !html.contains("data-moderator-panel"),
        "the moderator-management panel is hidden from a stranger"
    );
    assert!(
        !html.contains("/moderators\""),
        "no /moderators route surfaces for a stranger"
    );
    assert!(
        !html.contains("/moderate\""),
        "no per-note moderate route surfaces for a stranger"
    );
    assert!(
        !html.contains("data-mod-action=\"hide\""),
        "no per-note Hide action surfaces for a stranger"
    );
    assert!(
        !html.contains("data-issue-action="),
        "no issue Resolve / Reopen controls surface for a stranger"
    );
}
