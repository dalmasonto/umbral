//! Authorization test for the per-plugin moderation grant (Task A).
//!
//! Boots a minimal app against an in-memory SQLite DB, seeds real rows
//! through the ORM (a `Plugin` owned by user A, a `PluginModerator`
//! grant for user B), then drives `can_moderate` across the three
//! roles:
//!
//! * the owner (A, the plugin's `created_by`) → true
//! * a granted moderator (B, a `PluginModerator` row) → true
//! * an unrelated user (C) → false
//!
//! Real rows + the actual public `can_moderate` path. The test-only raw
//! DDL is the sanctioned exception to "no raw SQL in plugins" (tests
//! bypass `make` / `run`); every row-level read/write `can_moderate`
//! itself does still goes through the ORM.

use plugin_directory::can_moderate;
use plugin_directory::models::{
    AuditStatus, Plugin, PluginComment, PluginModeration, PluginModerator, PluginSource,
    PluginStatus, SecurityStatus,
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
        "plugin_directory_moderation_test"
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
/// seed the rows the authz cases need.
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

/// Test-only schema for the three tables this test touches. Only the
/// columns the ORM reads/writes here are declared.
async fn ensure_tables(pool: &sqlx::SqlitePool) {
    let stmts = [
        // `auth_user` is the FK target for `PluginModerator.user` /
        // `.added_by` and `Plugin.created_by`; `create` existence-probes
        // FKs, so the referenced rows must exist.
        "CREATE TABLE auth_user (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL
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
    ];
    for sql in stmts {
        sqlx::query(&sql).execute(pool).await.expect("CREATE TABLE");
    }
}

/// Seed three users (A owner, B moderator, C unrelated), a plugin owned
/// by A, and a `PluginModerator` grant for B — the plugin/grant through
/// the ORM; the raw `auth_user` rows directly (no AuthUser model
/// registered here, and tests bypass `make`/`run`).
async fn seed(pool: &sqlx::SqlitePool) {
    // User rows: the FK existence probe on `create` reads these.
    for (id, name) in [(1, "alice"), (2, "bob"), (3, "carol")] {
        sqlx::query("INSERT INTO auth_user (id, username) VALUES (?, ?)")
            .bind(id)
            .bind(name)
            .execute(pool)
            .await
            .expect("seed user");
    }

    // Plugin owned by user A (id 1).
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

#[tokio::test]
async fn can_moderate_grants_owner_and_moderator_but_not_strangers() {
    boot().await;

    let plugin = Plugin::objects()
        .filter(plugin_directory::models::plugin::SLUG.eq("owned-plugin"))
        .first()
        .await
        .expect("query plugin")
        .expect("the owned plugin exists");

    // Grant user B (id 2) moderation rights, added by the owner A (id 1).
    let grant = PluginModerator {
        id: 0,
        plugin: ForeignKey::new(plugin.id),
        user: ForeignKey::new(2),
        added_by: Some(ForeignKey::new(1)),
        created_at: chrono::Utc::now(),
    };
    PluginModerator::objects()
        .create(grant)
        .await
        .expect("create moderator grant");

    // Owner (A) moderates implicitly via `created_by`.
    assert!(
        can_moderate(&plugin, 1).await,
        "the plugin owner (created_by) can moderate"
    );

    // Granted moderator (B) moderates via the PluginModerator row.
    assert!(
        can_moderate(&plugin, 2).await,
        "a user with a PluginModerator grant can moderate"
    );

    // Unrelated user (C) cannot moderate.
    assert!(
        !can_moderate(&plugin, 3).await,
        "a user who is neither owner nor moderator cannot moderate"
    );

    // The grant is real: a second `(plugin, user)` row is rejected by the
    // UNIQUE constraint, proving uniqueness is enforced at the schema.
    let dup = PluginModerator {
        id: 0,
        plugin: ForeignKey::new(plugin.id),
        user: ForeignKey::new(2),
        added_by: Some(ForeignKey::new(1)),
        created_at: chrono::Utc::now(),
    };
    assert!(
        PluginModerator::objects().create(dup).await.is_err(),
        "a duplicate (plugin, user) grant is rejected by the UNIQUE constraint"
    );
}
