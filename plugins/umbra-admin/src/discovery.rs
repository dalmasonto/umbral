//! Runtime model discovery — walks the framework's plugin registry to
//! produce a `(plugin_name, ModelMeta)` list every handler can iterate
//! without re-implementing the framework's discovery logic.
//!
//! Cheap enough to call per request (it's an in-memory walk, no I/O).
//! Callers that need stable iteration order between calls should
//! collect into a sorted `Vec`.

use umbra::migrate::{Column, ModelMeta};

/// Every registered model, paired with the plugin that owns it.
pub(crate) fn discover_models() -> Vec<(String, ModelMeta)> {
    let mut out: Vec<(String, ModelMeta)> = Vec::new();
    for plugin in umbra::migrate::registered_plugins() {
        for model in umbra::migrate::models_for_plugin(&plugin) {
            out.push((plugin.clone(), model));
        }
    }
    out
}

/// Look up one model by SQL table name. Returns `None` for unknown
/// tables — callers map that to a 404.
pub(crate) fn find_model(table: &str) -> Option<(String, ModelMeta)> {
    discover_models()
        .into_iter()
        .find(|(_, m)| m.table == table)
}

/// Primary-key column descriptor for a model. Every umbra model has a
/// PK by `Model` trait contract, so this is `Option` only because the
/// signature can't express "always Some" without a panic-or-bug branch.
pub(crate) fn pk_column(model: &ModelMeta) -> Option<&Column> {
    model.fields.iter().find(|c| c.primary_key)
}

/// Return the user's saved theme preference (`"dark"` | `"light"` |
/// `"system"`). Falls back to `"dark"` on any error so the page always
/// renders something — this is the server-side read that prevents the
/// theme-flash on first paint.
pub(crate) async fn user_theme(user: &umbra_auth::AuthUser) -> String {
    crate::models::fetch_or_default(user.id)
        .await
        .map(|p| p.theme)
        .unwrap_or_else(|_| "dark".to_string())
}
