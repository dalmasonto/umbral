//! Render smoke-tests for the DB-driven plugin directory pages.
//!
//! `cargo build` cannot catch Jinja template errors or
//! missing-context-key bugs — those only surface at render time. This
//! test boots a minimal app against an in-memory SQLite DB, registers
//! the real template directories (the site's `templates/` for
//! `base.html` plus the plugin's own templates), seeds real `Plugin`
//! / `PluginFeature` / `PluginComment` rows through the ORM, then calls
//! the actual view handlers (`render_listing` / `render_detail`) and
//! asserts the rendered HTML contains the seeded values.
//!
//! Test-only raw DDL (the `ensure_tables` helper) is the sanctioned
//! exception to "no raw SQL in plugins": tests bypass `make` / `run`,
//! so they create their schema directly (same pattern as
//! umbra-admin's `ensure_tables_for_tests`). Every row-level read /
//! write the *pages* do still goes through the ORM.

use std::path::PathBuf;

use plugin_directory::models::{
    AuditStatus, CommentKind, CommentModeration, Plugin, PluginComment, PluginCompatibility,
    PluginFeature, PluginMaturity, PluginModeration, PluginSource, PluginStatus, SecurityStatus,
};
use plugin_directory::{
    create_note, create_report, create_submission, render_detail, render_detail_with,
    render_listing, render_prebuilt, render_report, render_search, render_submit,
};
use umbra::forms::ValidationErrors;
use umbra::migrate::ModelMeta;
use umbra::orm::{ForeignKey, Model};
use umbra::plugin::{Plugin as PluginTrait, PluginError};

/// A minimal test plugin that only contributes the directory's template
/// directory — we deliberately do NOT register the real
/// `PluginDirectoryPlugin` because its `on_ready` seeds eight official
/// rows asynchronously, which would race the test's own deterministic
/// seed and make the row counts non-reproducible.
#[derive(Debug, Default, Clone)]
struct TemplatesOnly;

impl PluginTrait for TemplatesOnly {
    fn name(&self) -> &'static str {
        "plugin_directory_templates_test"
    }
    fn models(&self) -> Vec<ModelMeta> {
        Vec::new()
    }
    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }
    /// Declare storage capability so the `field.storage_backend` system
    /// check passes for the `Plugin` model's `logo` / `cover_image` image
    /// fields. The check reads this flag (not the ambient backend); we DID
    /// register a real `TestStorage` ambiently in `boot()`, so this is honest.
    fn provides_storage(&self) -> bool {
        true
    }
    fn on_ready(&self, _ctx: &umbra::plugin::AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

/// A no-op storage backend so the boot-time system check passes — the
/// `Plugin` model declares `logo` / `cover_image` file/image fields, which
/// require a registered `Storage`. The render tests never upload, so this
/// only needs to satisfy the check and resolve `url(key)`.
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

/// Boot the app once: ambient pool + model registry + template engine,
/// then create the tables and seed representative rows.
///
/// Safe to call from multiple `#[tokio::test]` functions in the same binary.
/// `App::build` calls `settings::init` which panics on a second call, so the
/// async Mutex serialises concurrent callers and the AtomicBool lets late
/// arrivals skip the work once the winner has finished.
async fn boot() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Mutex;

    static BOOTED: AtomicBool = AtomicBool::new(false);
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();

    // Fast path: already initialised.
    if BOOTED.load(Ordering::Acquire) {
        return;
    }

    // Slow path: hold the async mutex so only one task runs init.
    let mutex = LOCK.get_or_init(|| Mutex::new(()));
    let _guard = mutex.lock().await;

    // Re-check under the lock — another task may have finished while we waited.
    if BOOTED.load(Ordering::Acquire) {
        return;
    }

    // Register a storage backend before build so the `logo` / `cover_image`
    // image-field system check passes. Set-once / first-wins, so harmless.
    let _ = umbra::storage::set_storage(std::sync::Arc::new(TestStorage));

    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");

    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    // Template dirs: the site root `templates/` holds `base.html` that
    // both pages extend; the plugin contributes its own page templates.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let site_templates = manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("templates");

    umbra::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Plugin>()
        .model::<PluginFeature>()
        .model::<PluginCompatibility>()
        .model::<PluginComment>()
        .templates_dir(site_templates)
        .plugin(TemplatesOnly::default())
        // Real-time push so posting a note fans out to SSE watchers.
        .plugin(umbra_realtime::RealtimePlugin::default())
        .build()
        .expect("App::build");

    ensure_tables(&pool).await;
    seed().await;

    BOOTED.store(true, Ordering::Release);
}

/// Test-only schema. Covers every column the ORM reads/writes for the
/// four models (matching `models.rs`).
async fn ensure_tables(pool: &sqlx::SqlitePool) {
    let stmts = [
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
                umbra_version TEXT NOT NULL,
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

/// Seed two plugins (one featured official, one community), a feature,
/// a compatibility row and two comments — all through the ORM.
async fn seed() {
    let mut rest = Plugin::default();
    rest.name = "Umbra REST".to_string();
    rest.slug = "umbra-rest".to_string();
    rest.crate_name = "umbra-rest".to_string();
    rest.author = "Umbra contributors".to_string();
    rest.short_description = "serializers, viewsets, routers".to_string();
    rest.full_content =
        "## Build APIs\n\nSerializers and viewsets the familiar way. `umbra add umbra-rest`."
            .to_string();
    rest.installation_commands = "umbra add umbra-rest".to_string();
    rest.status = PluginStatus::Usable;
    rest.maturity = PluginMaturity::Beta;
    rest.audit_status = AuditStatus::UmbraReviewed;
    rest.security_status = SecurityStatus::Normal;
    rest.source = PluginSource::Official;
    rest.moderation = PluginModeration::Approved;
    rest.featured = true;
    rest.display_order = 10;
    rest.github_stars = Some(2_140);
    rest.version = Some("0.0.1".to_string());
    rest.docs_url = Some("https://umbra.dev/docs/rest".to_string());
    rest.source_url = Some("https://github.com/umbra/umbra-rest".to_string());
    rest.issue_tracker_url = Some("https://github.com/umbra/umbra-rest/issues".to_string());
    // downloads left None → must render the honest em-dash.
    let rest = Plugin::objects().create(rest).await.expect("create rest");

    let mut tenancy = Plugin::default();
    tenancy.name = "Umbra Multitenancy".to_string();
    tenancy.slug = "umbra-multitenancy".to_string();
    tenancy.crate_name = "umbra-multitenancy".to_string();
    tenancy.author = "@kanto".to_string();
    tenancy.short_description = "schema-per-tenant scoping".to_string();
    tenancy.full_content = "Row- and schema-level tenancy for Umbra apps.".to_string();
    tenancy.installation_commands = "umbra add umbra-multitenancy".to_string();
    tenancy.status = PluginStatus::Experimental;
    tenancy.maturity = PluginMaturity::Alpha;
    tenancy.audit_status = AuditStatus::NotReviewed;
    tenancy.source = PluginSource::Community;
    tenancy.moderation = PluginModeration::Approved;
    Plugin::objects()
        .create(tenancy)
        .await
        .expect("create tenancy");

    let feature = PluginFeature {
        id: 0,
        plugin: ForeignKey::new(rest.id),
        name: "Cursor pagination".to_string(),
        slug: "cursor-pagination".to_string(),
        description: "Stable keyset pagination.".to_string(),
        status: PluginStatus::Shipped,
        maturity: PluginMaturity::Stable,
        release_target: None,
        docs_url: None,
        example_url: None,
        display_order: 1,
        visible: true,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    PluginFeature::objects()
        .create(feature)
        .await
        .expect("create feature");

    let compat = PluginCompatibility {
        id: 0,
        plugin: ForeignKey::new(rest.id),
        umbra_version: "0.0.1".to_string(),
        supported_database_backends: serde_json::json!(["postgres", "sqlite"]),
        minimum_rust_version: Some("1.80".to_string()),
        notes: None,
        verified_at: Some(chrono::Utc::now()),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    PluginCompatibility::objects()
        .create(compat)
        .await
        .expect("create compat");

    let mut visible = PluginComment::default();
    visible.plugin = ForeignKey::new(rest.id);
    visible.body = "The generated schema saved us a week.".to_string();
    visible.kind = CommentKind::UsageNote;
    visible.moderation = CommentModeration::Visible;
    visible.author_label = Some("Amina M.".to_string());
    PluginComment::objects()
        .create(visible)
        .await
        .expect("create comment");

    // A hidden comment that must NOT appear in the rendered page.
    let mut hidden = PluginComment::default();
    hidden.plugin = ForeignKey::new(rest.id);
    hidden.body = "SHOULD_NOT_RENDER pending moderation".to_string();
    hidden.kind = CommentKind::General;
    hidden.moderation = CommentModeration::Pending;
    PluginComment::objects()
        .create(hidden)
        .await
        .expect("create hidden");
}

/// Seed `n` extra approved community plugins so the directory crosses
/// the page-size boundary. Each gets a stable, sortable name/slug so the
/// pagination assertions can address individual rows. Created through the
/// ORM like every other row.
async fn seed_filler(n: usize) {
    for i in 0..n {
        let mut p = Plugin::default();
        p.name = format!("Filler Plugin {i:02}");
        p.slug = format!("filler-{i:02}");
        p.crate_name = format!("umbra-filler-{i:02}");
        p.author = "@filler".to_string();
        p.short_description = format!("filler row {i:02} for pagination");
        p.full_content = "Filler.".to_string();
        p.installation_commands = format!("umbra add umbra-filler-{i:02}");
        p.status = PluginStatus::Usable;
        p.maturity = PluginMaturity::Beta;
        p.audit_status = AuditStatus::NotReviewed;
        p.security_status = SecurityStatus::Normal;
        p.source = PluginSource::Community;
        p.moderation = PluginModeration::Approved;
        // display_order ascending so the fillers paginate in a stable,
        // predictable order after the two featured/base rows.
        p.display_order = 100 + i as i32;
        Plugin::objects().create(p).await.expect("create filler");
    }
}

#[tokio::test]
async fn listing_and_detail_render_real_db_rows() {
    boot().await;

    // The base seed has two approved plugins (one featured official, one
    // community). Add 13 filler community rows so the directory crosses
    // the 12-per-page boundary: 15 approved → page 1 holds 12, page 2
    // holds the remaining 3.
    seed_filler(13).await;
    let total = 15;
    let page_size = 12;

    // --- Listing: page 1 --------------------------------------------------
    let page1 = render_listing(None, false, None, 1)
        .await
        .expect("page 1 renders");
    // The featured official plugin sorts first (featured DESC), so it's on
    // page 1; humanized stars + honest em-dash still render.
    assert!(
        page1.contains("Umbra REST"),
        "featured official is on page 1"
    );
    assert!(page1.contains("2.1k"), "github_stars humanized");
    assert!(
        page1.contains("—"),
        "unknown downloads render the em-dash, not 0"
    );
    // Exactly 12 cards on a full first page (count the card anchor hrefs).
    let page1_cards = page1.matches("class=\"pd-card").count();
    assert_eq!(
        page1_cards, page_size,
        "a full first page shows exactly {page_size} cards, got {page1_cards}"
    );
    // The "Showing X–Y of N" line reflects the real page-1 window.
    assert!(
        page1.contains(&format!(
            "Showing <span class=\"font-semibold text-ink\">1–{page_size}</span> of {total} plugins"
        )),
        "page 1 shows the 1–12 of 15 range line"
    );
    // A Next control points at page 2 (Prev is disabled on page 1).
    assert!(
        page1.contains("href=\"/plugins?page=2\""),
        "page 1 renders a Next link to page 2"
    );
    assert!(
        page1.contains("pd-page-current\" aria-current=\"page\">1<"),
        "page 1 is the highlighted current page"
    );

    // --- Listing: page 2 (the remainder) ---------------------------------
    let page2 = render_listing(None, false, None, 2)
        .await
        .expect("page 2 renders");
    let page2_cards = page2.matches("class=\"pd-card").count();
    assert_eq!(
        page2_cards,
        total - page_size,
        "page 2 holds the remaining {} cards",
        total - page_size
    );
    assert!(
        page2.contains(&format!(
            "Showing <span class=\"font-semibold text-ink\">13–{total}</span> of {total} plugins"
        )),
        "page 2 shows the 13–15 of 15 range line"
    );
    // Prev points back at page 1 and page 2 is the current page.
    assert!(
        page2.contains("href=\"/plugins?page=1\""),
        "page 2 renders a Prev link to page 1"
    );
    assert!(
        page2.contains("pd-page-current\" aria-current=\"page\">2<"),
        "page 2 is the highlighted current page"
    );

    // --- Source facet filter still works (and is preserved in pager) ------
    // 14 approved community rows (1 base + 13 filler) → 2 pages, the
    // official plugin dropped.
    let community = render_listing(Some("community"), false, None, 1)
        .await
        .expect("filtered listing renders");
    assert!(community.contains("Umbra Multitenancy"));
    assert!(
        !community.contains("Umbra REST"),
        "?source=community filters out the official plugin"
    );
    assert!(
        community.contains("of 14 plugins"),
        "the facet count drives the filtered total"
    );
    // The `&` is HTML-escaped to `&amp;` in the rendered href (the
    // browser decodes it back) — assert the escaped form.
    assert!(
        community.contains("href=\"/plugins?page=2&amp;source=community\""),
        "page links preserve the ?source= facet"
    );

    // --- Search (?search=) filters by name/crate/description -------------
    let searched = render_listing(None, false, Some("multitenancy"), 1)
        .await
        .expect("search renders");
    assert!(
        searched.contains("Umbra Multitenancy"),
        "?search=multitenancy matches the multitenancy plugin"
    );
    assert!(
        !searched.contains("Umbra REST"),
        "?search=multitenancy excludes non-matching plugins"
    );
    assert!(
        searched.contains("value=\"multitenancy\""),
        "the search input is pre-filled with the active term"
    );

    // --- Detail -----------------------------------------------------------
    let detail = render_detail("umbra-rest")
        .await
        .expect("detail renders")
        .expect("plugin exists");
    assert!(
        detail.contains("Umbra REST"),
        "detail shows the plugin name"
    );
    // full_content rendered as markdown → the `##` heading becomes <h2>.
    assert!(
        detail.contains("Build APIs"),
        "full_content markdown body is rendered"
    );
    assert!(
        detail.contains("Cursor pagination"),
        "the reverse-loaded feature row renders"
    );
    assert!(
        detail.contains("The generated schema saved us a week."),
        "the visible comment renders"
    );
    assert!(
        !detail.contains("SHOULD_NOT_RENDER"),
        "the pending comment is filtered out by the moderation predicate"
    );
    assert!(
        detail.contains("1 of 1 shipped"),
        "the feature tracker counts shipped features from real rows"
    );
    assert!(
        detail.contains("PostgreSQL"),
        "the compatibility row's backends are summarized"
    );

    // --- Rich detail surfaces (stats, tabs, links, copy, dialog) ----------
    // Stat cards show the humanized star count and the real version.
    assert!(detail.contains("2.1k"), "the stars stat card is humanized");
    assert!(
        detail.contains("0.0.1"),
        "the version stat card shows the real version"
    );
    // Honest em-dash for the unknown downloads — not a fabricated 0.
    assert!(
        detail.contains("—"),
        "unknown downloads render the em-dash, not 0"
    );
    // Feature name + its derived status both render.
    assert!(
        detail.contains("Cursor pagination") && detail.contains("shipped"),
        "a feature renders with its status label"
    );
    // All five tab labels render (incl. Issues and Notes).
    for tab in ["Overview", "Features", "Compatibility", "Notes", "Issues"] {
        assert!(detail.contains(tab), "the `{tab}` tab label renders");
    }
    // The docs link href is rendered honestly from the real field.
    // minijinja autoescape encodes `/` in attribute values as `&#x2f;`
    // (the browser decodes it back) — assert the escaped host so we know
    // the real docs_url, not a placeholder, drives the link.
    assert!(
        detail.contains("umbra.dev&#x2f;docs&#x2f;rest"),
        "the docs link href renders from the real docs_url"
    );
    // The copy-button markup (with the icon swap classes) is present.
    assert!(
        detail.contains("class=\"copy-btn\"") && detail.contains("icon-copy"),
        "the copy button markup is present"
    );
    // The add-note dialog form posts to the new route with a textarea +
    // kind select.
    assert!(
        detail.contains("action=\"/plugins/umbra-rest/notes\""),
        "the note form posts to the new route"
    );
    assert!(
        detail.contains("name=\"body\"") && detail.contains("name=\"kind\""),
        "the note form has a body textarea and a kind select"
    );
    assert!(
        detail.contains("<select") && detail.contains("usage_note"),
        "the kind select renders CommentKind options"
    );
    // No success banner without the ?submitted=1 flag.
    assert!(
        !detail.contains("your note is live"),
        "the success banner is absent on a plain detail render"
    );

    // --- POST a note → a VISIBLE PluginComment exists, broadcast live -----
    // Publish-then-moderate: a posted note is visible at once and fans out
    // over SSE as a fully rendered row. A live-feed watcher subscribes to
    // this plugin's group first (the demo the detail page wires with
    // EventSource), registered directly on the registry (the SSE route's
    // policy gate isn't exercised here).
    let rest_row = Plugin::objects()
        .filter(plugin_directory::models::plugin::SLUG.eq("umbra-rest"))
        .first()
        .await
        .expect("query rest")
        .expect("rest exists");
    let mut watch_groups = std::collections::HashSet::new();
    watch_groups.insert(format!("public:plugin-{}", rest_row.id));
    let (_watch_id, mut watcher) = umbra_realtime::Realtime::registry()
        .register(None, watch_groups, umbra_realtime::DEFAULT_BUFFER)
        .await;

    let created = create_note(
        "umbra-rest",
        "Works great on Postgres 16.",
        "usage_note",
        Some("Reviewer".to_string()),
        None,
    )
    .await
    .expect("note create query ok")
    .expect("create_note returns a payload for an existing plugin");
    // The payload is the rendered row, carrying the body and the new PK.
    assert!(
        created.html.contains("Works great on Postgres 16."),
        "the payload html is the rendered note body; got {}",
        created.html
    );
    assert!(
        created.html.contains(&format!("data-comment-id=\"{}\"", created.id)),
        "the payload html tags the row with its id for client dedupe; got {}",
        created.html
    );

    // The note fanned out over SSE: the watcher got a `note` event carrying
    // the same id + rendered html the AJAX caller received.
    let live = watcher
        .try_recv()
        .expect("posting a note broadcast to the plugin's SSE watchers");
    assert_eq!(live.event, "note");
    let live_data = live.data.to_string();
    assert!(
        live_data.contains("Works great on Postgres 16."),
        "the live note carries the rendered body; got {live_data}"
    );
    assert!(
        live_data.contains(&created.id.to_string()),
        "the live note carries the row id; got {live_data}"
    );

    // The row exists with the submitted body and Visible moderation.
    let posted = PluginComment::objects()
        .filter(plugin_directory::models::plugin_comment::BODY.eq("Works great on Postgres 16."))
        .first()
        .await
        .expect("query the posted note")
        .expect("the posted note row exists");
    assert_eq!(
        posted.moderation,
        CommentModeration::Visible,
        "a posted note is visible immediately (publish-then-moderate)"
    );

    // A note for an unknown slug is a clean 404 (Ok(None)), no row.
    let missing_note = create_note("does-not-exist", "body", "general", None, None)
        .await
        .expect("create_note query ok");
    assert!(
        missing_note.is_none(),
        "create_note returns None for an unknown slug"
    );

    // Re-render with ?submitted=1 → the success banner appears AND the new
    // note is in the visible thread (no admin action needed).
    let after = render_detail_with("umbra-rest", true)
        .await
        .expect("submitted detail renders")
        .expect("plugin exists");
    assert!(
        after.contains("Thanks - your note is live in the thread."),
        "the ?submitted=1 success banner renders"
    );
    assert!(
        after.contains("Works great on Postgres 16."),
        "the posted note shows in the visible thread immediately"
    );

    // A non-existent slug is a clean 404 (Ok(None)), not an error.
    let missing = render_detail("does-not-exist").await.expect("query ok");
    assert!(missing.is_none(), "unknown slug yields None (404)");

    // --- Search fragment --------------------------------------------------
    // A query matching the seeded official plugin (name "Umbra REST",
    // crate "umbra-rest") returns it as a `.pd-search-result` link to its
    // slug, and the non-matching multitenancy plugin is absent.
    let hits = render_search("rest").await.expect("search renders");
    assert!(
        hits.contains("<a class=\"pd-search-result\" href=\"/plugins/umbra-rest\">"),
        "matching plugin is a search-result link to its slug"
    );
    assert!(
        hits.contains("<span class=\"pd-search-name\">Umbra REST</span>"),
        "the matching plugin's name renders inside the result"
    );
    assert!(
        !hits.contains("Umbra Multitenancy"),
        "a non-matching plugin is absent from the results"
    );

    // No match → the empty state names the query.
    let none = render_search("zzznomatch")
        .await
        .expect("empty search renders");
    assert!(
        none.contains("No plugins match \"zzznomatch\""),
        "no-match query renders the empty state with the query echoed"
    );

    // Empty query → the type-to-search hint, no DB hits.
    let blank = render_search("   ").await.expect("blank search renders");
    assert!(
        blank.contains("Type to search plugins…"),
        "empty query renders the hint state"
    );
    assert!(
        !blank.contains("pd-search-result"),
        "empty query produces no result links"
    );

    // --- Report an issue: GET renders the prefilled form -----------------
    // ?plugin=<seeded slug> resolves the plugin name and renders the form
    // (action /report, the category select, the details textarea).
    let report = render_report(
        Some("umbra-rest"),
        false,
        None,
        &std::collections::HashMap::new(),
    )
    .await
    .expect("report form renders");
    assert!(
        report.contains("action=\"/report\""),
        "the report form posts to /report"
    );
    assert!(
        report.contains("name=\"category\"") && report.contains("<select"),
        "the report form has a category select"
    );
    assert!(
        report.contains("name=\"details\"") && report.contains("<textarea"),
        "the report form has a details textarea"
    );
    assert!(
        report.contains("Umbra REST"),
        "the resolved plugin name renders in the report form"
    );
    assert!(
        report.contains("value=\"umbra-rest\""),
        "the plugin slug is carried in the hidden field"
    );

    // --- Report an issue: create_report files a pending PluginComment ----
    create_report(
        Some("umbra-rest"),
        "security",
        "Leaks the session cookie on the login redirect.",
    )
    .await
    .expect("create_report files the report");

    let filed = PluginComment::objects()
        .filter(
            plugin_directory::models::plugin_comment::BODY
                .eq("[security] Leaks the session cookie on the login redirect."),
        )
        .first()
        .await
        .expect("query the filed report")
        .expect("the report row exists");
    assert_eq!(
        filed.moderation,
        CommentModeration::Pending,
        "a filed report awaits moderation"
    );
    assert_eq!(
        filed.author_label.as_deref(),
        Some("Issue report"),
        "a filed report is labelled for the moderation queue"
    );
    assert!(
        filed.body.contains("[security]") && filed.body.contains("Leaks the session cookie"),
        "the report body carries the category prefix + the details"
    );

    // Empty details is rejected with a field-keyed error and NO new row.
    let before = PluginComment::objects().count().await.expect("count ok");
    let err = create_report(Some("umbra-rest"), "bug", "   ")
        .await
        .expect_err("empty details rejected");
    assert!(
        err.fields.contains_key("details"),
        "the empty-details error is keyed to the details field"
    );
    let after = PluginComment::objects().count().await.expect("count ok");
    assert_eq!(before, after, "a rejected report inserts no row");

    // --- Submit a plugin: the GET form renders ---------------------------
    let submit = render_submit(false, None, &std::collections::HashMap::new())
        .await
        .expect("submit form renders");
    assert!(
        submit.contains("action=\"/plugins/submit\""),
        "the submit form posts to /plugins/submit"
    );
    for input in [
        "name=\"name\"",
        "name=\"slug\"",
        "name=\"crate_name\"",
        "name=\"short_description\"",
        "name=\"full_content\"",
        "name=\"installation_commands\"",
    ] {
        assert!(
            submit.contains(input),
            "the submit form has an input for `{input}`"
        );
    }

    // --- Submit a plugin: valid data creates a pending community row -----
    let id = create_submission(&submission_data(&[
        ("name", "Umbra Webhooks"),
        ("slug", "umbra-webhooks"),
        ("crate_name", "umbra-webhooks"),
        ("author", "@webhooks"),
        (
            "short_description",
            "Outbound webhook delivery with retries.",
        ),
        (
            "full_content",
            "## Webhooks\n\nSign, queue and deliver outbound webhooks the Umbra way.",
        ),
        ("installation_commands", "umbra add umbra-webhooks"),
    ]))
    .await
    .expect("valid submission creates a row");

    let created = Plugin::objects()
        .filter(plugin_directory::models::plugin::ID.eq(id))
        .first()
        .await
        .expect("query the submitted plugin")
        .expect("the submitted plugin row exists");
    assert_eq!(
        created.source,
        PluginSource::Community,
        "a public submission is a community row"
    );
    assert_eq!(
        created.moderation,
        PluginModeration::Pending,
        "a public submission awaits moderation"
    );
    assert!(
        created.created_by.is_none(),
        "a public submission is not authored by an arbitrary user"
    );

    // --- Submit a plugin: invalid data → Err, no row, error renders ------
    let before_plugins = Plugin::objects().count().await.expect("count ok");
    let errs: ValidationErrors = create_submission(&submission_data(&[
        // Blank required name.
        ("name", ""),
        ("slug", "umbra-x"),
        ("crate_name", "umbra-x"),
        ("author", "@x"),
        (
            "short_description",
            "A short description that is long enough.",
        ),
        (
            "full_content",
            "Enough body content to satisfy the minimum length.",
        ),
        ("installation_commands", "umbra add umbra-x"),
    ]))
    .await
    .expect_err("blank name is rejected");
    assert!(
        errs.fields.contains_key("name"),
        "the validation error is keyed to the blank `name` field"
    );
    let after_plugins = Plugin::objects().count().await.expect("count ok");
    assert_eq!(
        before_plugins, after_plugins,
        "an invalid submission inserts no row"
    );

    // Re-render the submit form WITH the error context → the red error
    // text renders under the `name` field, and the typed values are kept.
    let resubmit = render_submit(
        false,
        Some(&errs),
        &submission_data(&[("name", ""), ("slug", "umbra-x"), ("crate_name", "umbra-x")]),
    )
    .await
    .expect("submit form re-renders with errors");
    assert!(
        resubmit.contains("pd-field--error"),
        "the errored field carries the error class"
    );
    assert!(
        resubmit.contains(&errs.fields["name"][0]),
        "the field's error message renders under the input"
    );
    assert!(
        resubmit.contains("value=\"umbra-x\""),
        "the previously-entered slug value is repopulated"
    );

    // --- Prebuilt page: official-only, each with its feature tracker ------
    // Loads official, approved plugins with `prefetch_related("feature_set")`
    // (1 parents + 1 children query). The seeded official "Umbra REST" and
    // its "Cursor pagination" feature render; the community plugin and the
    // deleted "More official plugins" strip do not.
    let prebuilt = render_prebuilt().await.expect("prebuilt renders");
    assert!(
        prebuilt.contains("umbra.rest"),
        "the official plugin's dotted crate name renders"
    );
    // The install line renders the dotted crate name (the surrounding
    // quotes are HTML-escaped by autoescape, so match the unescaped part).
    assert!(
        prebuilt.contains("plugins += ["),
        "the install line renders"
    );
    assert!(
        prebuilt.contains("Cursor pagination"),
        "the prefetched feature row renders"
    );
    assert!(
        prebuilt.contains("shipped"),
        "the feature's derived status label renders"
    );
    assert!(
        prebuilt.contains("/plugins/umbra-rest"),
        "the card links to the plugin detail page"
    );
    assert!(
        !prebuilt.contains("Umbra Multitenancy"),
        "community plugins are excluded from the official-only page"
    );
    assert!(
        !prebuilt.contains("More official plugins"),
        "the hardcoded 'More official plugins' strip was removed"
    );
}

#[tokio::test]
async fn create_note_threads_replies_under_a_visible_top_level_note() {
    boot().await;

    let note = create_note("umbra-rest", "Parent note body.", "general", None, None)
        .await
        .expect("note create ok")
        .expect("a payload for an existing plugin");
    assert!(note.parent_id.is_none(), "a top-level note has no parent_id");

    let reply = create_note("umbra-rest", "A reply body.", "general", None, Some(note.id))
        .await
        .expect("reply create ok")
        .expect("a payload for a valid parent");
    assert_eq!(
        reply.parent_id,
        Some(note.id),
        "the reply payload carries the parent note id"
    );

    let row = PluginComment::objects()
        .filter(plugin_directory::models::plugin_comment::BODY.eq("A reply body."))
        .first()
        .await
        .expect("query the reply")
        .expect("the reply row exists");
    assert_eq!(row.parent.as_ref().map(|fk| fk.id()), Some(note.id));
    assert_eq!(row.moderation, CommentModeration::Visible);

    let nested = create_note("umbra-rest", "Nested.", "general", None, Some(reply.id))
        .await
        .expect("create ok");
    assert!(nested.is_none(), "replying to a reply is rejected (depth-1)");

    let bad = create_note("umbra-rest", "Orphan.", "general", None, Some(999_999))
        .await
        .expect("create ok");
    assert!(bad.is_none(), "an unknown parent id is rejected");
}

/// Build a form-data map for the submission tests.
fn submission_data(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}
