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

/// Default list-display when the admin model carries no explicit
/// `list_display`.
///
/// Two-state behaviour:
///
/// 1. If at least one field is tagged `#[umbra(string)]`, render
///    just that field — Django-style `__str__()`. The PK is implicit
///    in the row link (every row in the admin opens the sheet on
///    click), so a separate `id` column is redundant.
/// 2. Otherwise show every column. The `string` attribute is the
///    sole opt-in for the compact form; without it the changelist
///    stays at the legacy "show all fields" default, so adding the
///    derive doesn't silently rewrite existing changelists.
pub(crate) fn default_list_display(model: &ModelMeta) -> Vec<String> {
    let str_field = model
        .fields
        .iter()
        .find(|c| c.is_string_repr && !c.primary_key)
        .map(|c| c.name.clone());
    if let Some(s) = str_field {
        return vec![s];
    }
    // No opt-in — show every column. Matches pre-#46 behaviour so
    // existing changelists are unchanged.
    model.fields.iter().map(|c| c.name.clone()).collect()
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
