//! Render smoke-test for the self-service moderation account pages.
//!
//! `cargo build` cannot catch minijinja template errors (a bad template is
//! silently skipped by the loader — see `templates.rs` `add_template(...).is_ok()`
//! — so the page 500s only at render time). This boots the real template engine
//! (site `templates/` for `base.html` + the plugin's own templates) and renders
//! `account/plugins.html` and `account/plugin_manage.html` with representative
//! context, asserting the HTML comes back with the seeded values. No DB rows are
//! needed: the pages take a plain context, so this isolates the template layer.

use std::path::PathBuf;

use plugin_directory::models::{Plugin, PluginComment, PluginCompatibility, PluginFeature};
use umbral::migrate::ModelMeta;
use umbral::plugin::Plugin as PluginTrait;
use umbral::templates::context;

/// Contributes only the plugin's template dir (no routes/models) so the engine
/// resolves `account/*.html`.
#[derive(Debug, Default, Clone)]
struct TemplatesOnly;
impl PluginTrait for TemplatesOnly {
    fn name(&self) -> &'static str {
        "plugin_directory_account_render_test"
    }
    fn models(&self) -> Vec<ModelMeta> {
        vec![]
    }
    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }
    fn provides_storage(&self) -> bool {
        true
    }
}

/// A no-op Storage so the image-field system check passes at build.
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

async fn boot() {
    let _ = umbral::storage::set_storage(std::sync::Arc::new(TestStorage));
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    let site_templates = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("templates");

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<Plugin>()
        .model::<PluginFeature>()
        .model::<PluginCompatibility>()
        .model::<PluginComment>()
        .templates_dir(site_templates)
        .plugin(TemplatesOnly)
        .build()
        .expect("App::build");
}

#[tokio::test]
async fn account_pages_render() {
    boot().await;

    // --- Overview page ------------------------------------------------------
    let rows = serde_json::json!([
        {
            "slug": "umbral-auth", "name": "Umbral Auth", "icon": "UA", "role": "Owner",
            "status": "Shipped", "moderation": "Approved",
            "open_issues": 3, "hidden": 1, "pending_notes": 2
        },
        {
            "slug": "my-cache", "name": "My Cache", "icon": "MC", "role": "Moderator",
            "status": "Usable", "moderation": "Approved",
            "open_issues": 0, "hidden": 0, "pending_notes": 0
        }
    ]);
    let html = umbral::templates::render("account/plugins.html", &context! { rows => rows })
        .expect("account/plugins.html renders");
    assert!(html.contains("Umbral Auth"), "owned plugin name present");
    assert!(html.contains("3 open issues"), "plural open-issue count");
    assert!(
        html.contains("No open issues"),
        "zero-issue plugin shows the calm state"
    );
    assert!(
        html.contains("/account/plugins/umbral-auth"),
        "Manage link present"
    );

    // Empty overview → empty state, not a crash.
    let empty = umbral::templates::render(
        "account/plugins.html",
        &context! { rows => serde_json::json!([]) },
    )
    .expect("empty overview renders");
    assert!(
        empty.contains("haven't listed any plugins"),
        "empty state shown"
    );

    // --- Per-plugin manage page --------------------------------------------
    let plugin = serde_json::json!({
        "slug": "umbral-auth", "name": "Umbral Auth", "icon": "UA", "role": "Owner",
        "next": "/account/plugins/umbral-auth", "detail_url": "/plugins/umbral-auth",
        "open_issues": [
            {
                "id": 7, "body": "Panics on **empty** password.", "kind": "Question",
                "author": "Anonymous", "created": "Jul 5, 2026",
                "is_resolved": false, "moderation": "visible", "hidden": false, "pending": false
            }
        ],
        "resolved_issues": [
            {
                "id": 8, "body": "Fixed typo", "kind": "General", "author": "maintainer",
                "created": "Jul 1, 2026",
                "is_resolved": true, "moderation": "visible", "hidden": false, "pending": false
            }
        ],
        "notes": [
            {
                "id": 9, "body": "Awaiting review", "kind": "Usage Note", "author": "Anonymous",
                "created": "Jul 4, 2026",
                "is_resolved": false, "moderation": "pending", "hidden": false, "pending": true
            }
        ]
    });
    let manage =
        umbral::templates::render("account/plugin_manage.html", &context! { plugin => plugin })
            .expect("account/plugin_manage.html renders");
    assert!(
        manage.contains("Open issues"),
        "open-issues section present"
    );
    // Markdown filter ran on the issue body.
    assert!(
        manage.contains("<strong>empty</strong>"),
        "issue body rendered as markdown"
    );
    // Action forms target the existing moderation endpoints with a `next`.
    assert!(
        manage.contains("/plugins/umbral-auth/issues/7/resolve"),
        "resolve form targets the real endpoint"
    );
    assert!(
        manage.contains("/plugins/umbral-auth/issues/8/reopen"),
        "reopen form present for a resolved issue"
    );
    // The `next` value is a single interpolation, so autoescaping turns its
    // slashes into `&#x2f;` (security-by-default). The browser decodes that
    // back to `/` when submitting, so the server receives the real local path.
    assert!(
        manage.contains("name=\"next\" value=\"&#x2f;account&#x2f;plugins&#x2f;umbral-auth\""),
        "hidden next field points back to the manage page"
    );
    assert!(manage.contains("Approve"), "pending note offers Approve");
}
