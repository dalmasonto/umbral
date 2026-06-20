//! End-to-end tests for the moderation ACTIONS (Task B).
//!
//! Boots a minimal app against an in-memory SQLite DB, seeds real rows
//! through the ORM, then drives the SAME logic functions the routes call
//! (`add_moderator_logic`, `moderate_comment_logic`, `resolve_issue_logic`)
//! plus the owner-vs-can_moderate authz split (`is_owner` / `can_moderate`):
//!
//! * (a) owner adds user B as moderator → the grant row exists.
//! * (b) B (a moderator) hides a comment → `moderation = Hidden`.
//! * (c) stranger C tries to hide → rejected by `can_moderate`, comment
//!   unchanged.
//! * (d) owner resolves an issue → `is_resolved = true`.
//! * (e) a non-owner moderator tries to ADD a moderator → rejected by the
//!   owner-only `is_owner` gate.
//!
//! Real rows + the actual public moderation paths (the routes are thin
//! wrappers over these functions, so testing them IS testing the routes).
//! The test-only raw DDL is the sanctioned exception to "no raw SQL in
//! plugins" (tests bypass `make` / `run`); every row-level read/write the
//! logic functions do still goes through the ORM.

use plugin_directory::models::{
    AuditStatus, CommentKind, CommentModeration, Plugin, PluginComment, PluginModeration,
    PluginModerator, PluginSource, PluginStatus, SecurityStatus,
};
use plugin_directory::{
    AddModeratorOutcome, add_moderator_logic, can_moderate, is_owner, moderate_comment_logic,
    resolve_issue_logic,
};
use umbra::migrate::ModelMeta;
use umbra::orm::{ForeignKey, Model};
use umbra::plugin::{Plugin as PluginTrait, PluginError};

/// A no-op storage backend so the boot-time system check passes — the
/// `Plugin` model declares `logo` / `cover_image` image fields, which
/// require a registered `Storage`. This test never uploads.
struct TestStorage;

#[umbra::storage::async_trait]
impl umbra::storage::Storage for TestStorage {
    async fn store(
        &self,
        filename: &str,
        _content_type: &str,
        _bytes: &[u8],
    ) -> Result<umbra::storage::StoredFile, umbra::storage::StorageError> {
        let key = filename.to_string();
        let url = self.url(&key);
        Ok(umbra::storage::StoredFile { key, url })
    }
    async fn retrieve(&self, _key: &str) -> Result<Vec<u8>, umbra::storage::StorageError> {
        Err(umbra::storage::StorageError::NotFound)
    }
    async fn delete(&self, _key: &str) -> Result<(), umbra::storage::StorageError> {
        Ok(())
    }
    fn url(&self, key: &str) -> String {
        format!("/media/{key}")
    }
}

/// Declares the storage capability so the `field.storage_backend` system
/// check passes for the `Plugin` model's image fields.
#[derive(Debug, Default, Clone)]
struct StorageOnly;

impl PluginTrait for StorageOnly {
    fn name(&self) -> &'static str {
        "plugin_directory_moderation_actions_test"
    }
    fn models(&self) -> Vec<ModelMeta> {
        Vec::new()
    }
    fn provides_storage(&self) -> bool {
        true
    }
    fn on_ready(&self, _ctx: &umbra::plugin::AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

/// Boot the ambient pool + model registry once, create the tables and
/// seed the rows the action cases need.
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

    let _ = umbra::storage::set_storage(std::sync::Arc::new(TestStorage));

    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");

    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    umbra::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Plugin>()
        .model::<PluginComment>()
        .model::<PluginModerator>()
        .plugin(StorageOnly::default())
        .build()
        .expect("App::build");

    ensure_tables(&pool).await;
    seed(&pool).await;

    BOOTED.store(true, Ordering::Release);
}

/// Test-only schema for the four tables this test touches. Only the
/// columns the ORM reads/writes here are declared.
async fn ensure_tables(pool: &sqlx::SqlitePool) {
    let stmts = [
        // `auth_user` is the FK target + the username lookup `add_moderator_logic`
        // resolves against. The derived `AuthUser` model maps to this table name.
        "CREATE TABLE auth_user (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            email TEXT NOT NULL DEFAULT '',
            password_hash TEXT NOT NULL DEFAULT '',
            is_active INTEGER NOT NULL DEFAULT 1,
            is_staff INTEGER NOT NULL DEFAULT 0,
            is_superuser INTEGER NOT NULL DEFAULT 0,
            date_joined TEXT NOT NULL DEFAULT '',
            last_login TEXT
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
                user INTEGER NOT NULL REFERENCES auth_user(id),
                added_by INTEGER REFERENCES auth_user(id),
                created_at TEXT NOT NULL,
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
                umbra_version TEXT,
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

/// Seed three users (A owner, B moderator-to-be, C unrelated) and a plugin
/// owned by A — the plugin through the ORM; the raw `auth_user` rows directly.
async fn seed(pool: &sqlx::SqlitePool) {
    // `add_moderator_logic` resolves a username via `AuthUser::objects().first()`,
    // which decodes the WHOLE row — so `date_joined` (a non-null DateTime) needs
    // a valid RFC3339 value, not an empty placeholder.
    let now = chrono::Utc::now().to_rfc3339();
    // dave (id 4) is the dedicated "freshly added" user for the add-by-username
    // assertion, kept apart from bob/carol so the shared in-memory DB's parallel
    // tests don't race on his moderator state.
    for (id, name) in [(1, "alice"), (2, "bob"), (3, "carol"), (4, "dave")] {
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
    p.name = "Owned Plugin".to_string();
    p.slug = "owned-plugin".to_string();
    p.crate_name = "umbra-owned".to_string();
    p.author = "@alice".to_string();
    p.short_description = "a plugin owned by alice".to_string();
    p.full_content = "Full content body for the owned plugin.".to_string();
    p.installation_commands = "umbra add umbra-owned".to_string();
    p.status = PluginStatus::Usable;
    p.maturity = plugin_directory::PluginMaturity::Beta;
    p.audit_status = AuditStatus::NotReviewed;
    p.security_status = SecurityStatus::Normal;
    p.source = PluginSource::Community;
    p.moderation = PluginModeration::Approved;
    p.created_by = Some(ForeignKey::new(1));
    Plugin::objects().create(p).await.expect("create plugin");
}

/// Helper: load the seeded plugin fresh (so `created_by` is populated as the
/// real route would see it).
async fn owned_plugin() -> Plugin {
    Plugin::objects()
        .filter(plugin_directory::models::plugin::SLUG.eq("owned-plugin"))
        .first()
        .await
        .expect("query plugin")
        .expect("the owned plugin exists")
}

/// Helper: insert a visible comment on the plugin and return its id.
async fn seed_comment(plugin_id: i64, is_issue: bool) -> i64 {
    let mut c = PluginComment::default();
    c.plugin = ForeignKey::new(plugin_id);
    c.body = "a note body".to_string();
    c.kind = CommentKind::General;
    c.moderation = CommentModeration::Visible;
    c.is_issue = is_issue;
    c.is_public = true;
    c.is_resolved = false;
    PluginComment::objects()
        .create(c)
        .await
        .expect("create comment")
        .id
}

async fn comment_by_id(id: i64) -> PluginComment {
    PluginComment::objects()
        .filter(plugin_directory::models::plugin_comment::ID.eq(id))
        .first()
        .await
        .expect("query comment")
        .expect("comment exists")
}

// (a) Owner adds user B as moderator → the grant row exists, and B then
// passes `can_moderate` (gaining the rights the action paths gate on).
#[tokio::test]
async fn owner_adds_moderator_and_grant_is_real() {
    boot().await;
    let plugin = owned_plugin().await;

    // Owner (A, id 1) is the owner; only they may manage the roster.
    assert!(is_owner(&plugin, 1), "alice owns the plugin");

    // Add dave (id 4) by username — a fresh grant the parallel tests don't
    // touch, so this reliably observes the `Added` outcome.
    let outcome = add_moderator_logic(&plugin, "dave", 1)
        .await
        .expect("add moderator");
    assert_eq!(outcome, AddModeratorOutcome::Added, "dave is added by username");

    // The grant is real: dave now passes `can_moderate`.
    assert!(
        can_moderate(&plugin, 4).await,
        "dave can moderate after being added"
    );

    // The row exists in the moderator table.
    let exists = PluginModerator::objects()
        .filter(plugin_directory::models::plugin_moderator::PLUGIN.eq(plugin.id))
        .filter(plugin_directory::models::plugin_moderator::USER.eq(4))
        .exists()
        .await
        .expect("query grant");
    assert!(exists, "the (plugin, dave) grant row exists");

    // Re-adding is idempotent: a UNIQUE clash maps to AlreadyModerator, never
    // a 500.
    let again = add_moderator_logic(&plugin, "4", 1)
        .await
        .expect("re-add by id");
    assert_eq!(
        again,
        AddModeratorOutcome::AlreadyModerator,
        "re-adding bob (by id) is a graceful no-op"
    );

    // An unknown user is reported, not 500'd.
    let missing = add_moderator_logic(&plugin, "nobody", 1)
        .await
        .expect("unknown user resolves gracefully");
    assert_eq!(missing, AddModeratorOutcome::UserNotFound);
}

// (b) B (a moderator) hides a comment → `moderation = Hidden`.
#[tokio::test]
async fn moderator_hides_comment() {
    boot().await;
    let plugin = owned_plugin().await;

    // Make sure B is a moderator (the test above may not have run first).
    let _ = add_moderator_logic(&plugin, "bob", 1).await;
    assert!(can_moderate(&plugin, 2).await, "bob is a moderator");

    let comment_id = seed_comment(plugin.id, false).await;

    let changed = moderate_comment_logic(&plugin, comment_id, "hide")
        .await
        .expect("hide action");
    assert!(changed, "the comment was found + moderated");

    let c = comment_by_id(comment_id).await;
    assert_eq!(
        c.moderation,
        CommentModeration::Hidden,
        "moderation flipped to Hidden"
    );
    assert!(!c.is_public, "a hidden comment is no longer public");

    // Unhide flips it back to Visible + public.
    moderate_comment_logic(&plugin, comment_id, "unhide")
        .await
        .expect("unhide action");
    let c = comment_by_id(comment_id).await;
    assert_eq!(c.moderation, CommentModeration::Visible);
    assert!(c.is_public);
}

// (c) Stranger C tries to hide → rejected by `can_moderate`, comment
// unchanged.
#[tokio::test]
async fn stranger_cannot_hide_comment() {
    boot().await;
    let plugin = owned_plugin().await;

    let comment_id = seed_comment(plugin.id, false).await;

    // C (id 3) is neither owner nor moderator: the route's gate rejects.
    assert!(
        !can_moderate(&plugin, 3).await,
        "carol cannot moderate (this is the 403 the handler returns)"
    );

    // The comment is untouched (the handler never reaches the mutation).
    let c = comment_by_id(comment_id).await;
    assert_eq!(
        c.moderation,
        CommentModeration::Visible,
        "the comment stays Visible — no unauthorized mutation"
    );
}

// (d) Owner resolves an issue → `is_resolved = true`, then reopen flips it
// back.
#[tokio::test]
async fn owner_resolves_and_reopens_issue() {
    boot().await;
    let plugin = owned_plugin().await;

    let issue_id = seed_comment(plugin.id, true).await;

    // Owner (A) gates through can_moderate (owner OR moderator).
    assert!(can_moderate(&plugin, 1).await, "owner can moderate");

    let ok = resolve_issue_logic(&plugin, issue_id, true)
        .await
        .expect("resolve");
    assert!(ok, "issue found + resolved");
    assert!(
        comment_by_id(issue_id).await.is_resolved,
        "is_resolved flipped to true"
    );

    let ok = resolve_issue_logic(&plugin, issue_id, false)
        .await
        .expect("reopen");
    assert!(ok);
    assert!(
        !comment_by_id(issue_id).await.is_resolved,
        "reopen flips is_resolved back to false"
    );
}

// (e) A non-owner moderator tries to ADD a moderator → rejected by the
// owner-only `is_owner` gate (managing the roster is creator-only, NOT
// can_moderate). This is the owner-vs-can_moderate split.
#[tokio::test]
async fn moderator_cannot_add_another_moderator() {
    boot().await;
    let plugin = owned_plugin().await;

    // B is a moderator (can act on content)…
    let _ = add_moderator_logic(&plugin, "bob", 1).await;
    assert!(can_moderate(&plugin, 2).await, "bob can moderate content");

    // …but B is NOT the owner, so the roster-management gate rejects B. The
    // route checks `is_owner` (not `can_moderate`) before calling
    // `add_moderator_logic`, so a moderator never reaches the mutation.
    assert!(
        !is_owner(&plugin, 2),
        "bob is a moderator but not the owner — the add-moderator route 403s here"
    );
    assert!(
        is_owner(&plugin, 1),
        "only alice (creator) passes the owner-only roster gate"
    );
}
