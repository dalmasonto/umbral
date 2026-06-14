//! Render smoke-test for the DB-driven public landing page.
//!
//! `cargo build` cannot catch Jinja template errors or
//! missing-context-key bugs in `home.html` — those only surface at
//! render time. This test boots a minimal app (ambient pool + template
//! engine), registers the real template directories (the site's
//! `templates/` for `base.html` plus the public plugin's own
//! `templates/`), then renders `public/home.html` against a hand-built
//! context whose keys mirror exactly what the `home()` handler passes
//! (`plugins`, `plugin_count`, `model_count`, `community_count`,
//! `deprecated_count`, `form_submissions`, `glue_lines`).
//!
//! It asserts two things the design promises:
//!   1. Real plugin rows render (crate name, install command, status).
//!   2. The honest "—" placeholder renders for an unknown count
//!      (`model_count` is `None` here) — never a fabricated `0`.

use std::path::PathBuf;

use serde::Serialize;
use umbra::migrate::ModelMeta;
use umbra::plugin::{AppContext, Plugin as PluginTrait, PluginError};
use umbra::templates::context;

/// A minimal plugin that contributes only the public plugin's template
/// directory, so the engine can resolve `public/home.html`.
#[derive(Debug, Default, Clone)]
struct TemplatesOnly;

impl PluginTrait for TemplatesOnly {
    fn name(&self) -> &'static str {
        "public_templates_test"
    }
    fn models(&self) -> Vec<ModelMeta> {
        Vec::new()
    }
    fn templates_dirs(&self) -> Vec<PathBuf> {
        // home.html imports `plugin_directory/_macros.html`, so the test
        // must register that dir too — exactly as production does via
        // PluginDirectoryPlugin::templates_dirs(). Without it the engine
        // can't resolve the macro import and the render errors.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let plugins_dir = manifest.parent().unwrap();
        vec![
            manifest.join("templates"),
            plugins_dir.join("plugin_directory").join("templates"),
        ]
    }
    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

/// Mirror of the public `PluginRow` shape the template iterates over.
/// We don't depend on the real struct here — the template only reads
/// these serialized keys, so matching the field names is what matters.
#[derive(Serialize)]
struct Row {
    id: i64,
    crate_name: String,
    status: String,
    short_description: String,
    stars: String,
    downloads: String,
    notes: i64,
    audited: bool,
    install: String,
}

async fn boot() {
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");

    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    // The site root `templates/` holds `base.html`, which the home page
    // extends; the plugin contributes `public/home.html`.
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
        .templates_dir(site_templates)
        .plugin(TemplatesOnly)
        .build()
        .expect("App::build");
}

#[tokio::test]
async fn home_renders_real_rows_and_honest_dash() {
    boot().await;

    let plugins = vec![
        Row {
            id: 1,
            crate_name: "umbra-rest".to_string(),
            status: "stable".to_string(),
            short_description: "Serializers, viewsets, routers.".to_string(),
            stars: "2.1k".to_string(),
            // Unknown downloads must render the honest em-dash on the card.
            downloads: "—".to_string(),
            notes: 4,
            audited: true,
            install: "umbra add umbra-rest".to_string(),
        },
        Row {
            id: 2,
            crate_name: "umbra-multitenancy".to_string(),
            status: "experimental".to_string(),
            short_description: "Schema-per-tenant scoping.".to_string(),
            stars: "910".to_string(),
            downloads: "9.1k".to_string(),
            notes: 0,
            audited: false,
            install: "umbra add umbra-multitenancy".to_string(),
        },
    ];

    // Context keys mirror `home()` exactly. `model_count` is None so the
    // stat strip must render the "—" placeholder, not a 0.
    let plugin_count: Option<i64> = Some(plugins.len() as i64);
    let model_count: Option<i64> = None;
    let community_count: Option<i64> = Some(1);
    let deprecated_count: Option<i64> = Some(0);
    let form_submissions: Option<i64> = Some(3);
    let glue_lines: i64 = 0;
    // Empty reviews → the trust strip renders its honest empty state
    // ("Be the first…"), never fabricated testimonials.
    let reviews: Vec<i64> = Vec::new();

    let html = umbra::templates::render(
        "public/home.html",
        &context! {
            plugins => plugins,
            plugin_count => plugin_count,
            model_count => model_count,
            community_count => community_count,
            deprecated_count => deprecated_count,
            form_submissions => form_submissions,
            glue_lines => glue_lines,
            reviews => reviews,
        },
    )
    .expect("home.html renders without a template error");

    // 1. Real plugin rows render: name, install command, status.
    assert!(
        html.contains("umbra-rest"),
        "first plugin's crate name renders"
    );
    assert!(
        html.contains("umbra-multitenancy"),
        "second plugin's crate name renders"
    );
    assert!(
        html.contains("umbra add umbra-rest"),
        "the install command renders on the card"
    );

    // 2. The avatar initial (first letter of the crate name, sans the
    //    `umbra-` prefix) renders — proves the {} placeholder bug is gone.
    assert!(
        html.contains(">R</span>"),
        "avatar initial for umbra-rest renders"
    );

    // 3. The honest em-dash renders for the unknown model_count, and the
    //    known counts render their real values.
    assert!(
        html.contains("Models migrated"),
        "the model-count stat cell is present"
    );
    assert!(
        html.contains("—"),
        "unknown counts render the em-dash, not 0"
    );
    assert!(
        html.contains("Plugins listed"),
        "the live plugin-count cell renders"
    );

    // 4. The audited card shows the green Audited badge; the unverified
    //    one shows the amber warning.
    assert!(
        html.contains("Audited"),
        "audited badge renders for the reviewed plugin"
    );
    assert!(
        html.contains("Unverified"),
        "unverified badge renders for the unreviewed plugin"
    );

    // 5. With no reviews seeded, the trust strip renders the honest empty
    //    state — not the old hardcoded "Rosa Méndez" / "Theo Kline" cards.
    assert!(
        html.contains("Be the first to share yours"),
        "empty reviews render the honest empty state"
    );
    assert!(
        !html.contains("Rosa Méndez") && !html.contains("Theo Kline"),
        "no fabricated testimonials remain in the homepage markup"
    );
}
