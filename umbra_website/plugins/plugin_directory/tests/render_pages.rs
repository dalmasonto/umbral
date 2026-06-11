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
use plugin_directory::{render_detail, render_listing, render_search};
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
    fn on_ready(&self, _ctx: &umbra::plugin::AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

/// Boot the app once: ambient pool + model registry + template engine,
/// then create the tables and seed representative rows.
async fn boot() {
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
        .build()
        .expect("App::build");

    ensure_tables(&pool).await;
    seed().await;
}

/// Test-only schema. Covers every column the ORM reads/writes for the
/// four models (matching `models.rs`).
async fn ensure_tables(pool: &sqlx::SqlitePool) {
    let stmts = [
        format!(
            "CREATE TABLE {t} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                public_id TEXT NOT NULL,
                name TEXT NOT NULL,
                slug TEXT NOT NULL,
                logo TEXT,
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
    let page1 = render_listing(None, 1).await.expect("page 1 renders");
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
    let page2 = render_listing(None, 2).await.expect("page 2 renders");
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
    let community = render_listing(Some("community"), 1)
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
}
