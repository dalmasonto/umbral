//! `AdminRegistry` — per-plugin grouping of registered models.
//!
//! The registry records which plugin owns each model so the sidebar
//! nav can be built as a tree: `plugin → [model, model, ...]`.
//!
//! # Usage
//!
//! [`AdminPlugin::register`] calls [`AdminRegistry::register`] internally,
//! passing `Plugin::name()` as the plugin identifier. The rendered shell
//! calls [`AdminRegistry::apps`] to get the sorted, permission-filtered
//! sidebar tree.
//!
//! # Auto-discovery
//!
//! [`AdminRegistry::apps`] synthesises a default [`AdminRegistration`] for
//! every model in the global model registry that does NOT have an explicit
//! registration. The label comes from `ModelMeta::display` (which reflects
//! `Model::DISPLAY`) and the icon from `ModelMeta::icon` (`Model::ICON`).
//! Explicit registrations override the synthesised defaults — same table
//! name means the explicit entry wins.
//!
//! # Permission gating
//!
//! Today, [`AdminRegistry::apps`] passes every entry through for any staff
//! user — matching the current baseline behaviour. When `umbra-permissions`
//! lands (gap 33), add a `view_<table>` permission check per entry and
//! filter out models the viewer may not see.

use std::collections::HashMap;

use umbra_auth::AuthUser;

use crate::AdminModel;

/// One registered model entry: the display config plus metadata.
#[derive(Debug, Clone)]
pub struct AdminRegistration {
    /// The per-model admin configuration.
    pub model: AdminModel,
    /// The name of the plugin that registered this model
    /// (`Plugin::name()`).
    pub plugin: String,
    /// Human-readable label shown in the sidebar. Defaults to the
    /// table name if not supplied.
    pub label: String,
    /// Lucide icon name shown in the sidebar. Defaults to `"database"`.
    pub icon: Option<String>,
}

/// One plugin's group in the sidebar tree.
#[derive(Debug, Clone)]
pub struct App {
    /// The plugin name used as the group header.
    pub plugin: String,
    /// Display label for the group (same as `plugin` today; a future
    /// `verbose_name` field on `Plugin` could override this).
    pub label: String,
    /// Models in this group, sorted by label.
    pub models: Vec<AdminRegistration>,
}

/// Central registry that maps `table_name → AdminRegistration`.
///
/// One instance lives inside [`crate::AdminPlugin`] and is Arc-shared
/// into every route handler via [`crate::AdminState`].
#[derive(Debug, Default, Clone)]
pub struct AdminRegistry {
    // table_name -> AdminRegistration
    entries: HashMap<String, AdminRegistration>,
}

impl AdminRegistry {
    /// Register an [`AdminModel`] under the given plugin name.
    ///
    /// If a model with the same table was already registered, the new
    /// registration wins (last-write-wins, same as Django's
    /// `admin.site.register` on duplicates).
    pub fn register(&mut self, plugin: &str, model: AdminModel) {
        let label = model.label.clone().unwrap_or_else(|| {
            // Default: title-case the table name (replace `_` with space).
            titlecase(&model.table)
        });
        let icon = model.icon.clone();
        let table = model.table.clone();
        self.entries.insert(
            table,
            AdminRegistration {
                model,
                plugin: plugin.to_string(),
                label,
                icon,
            },
        );
    }

    /// Build the sidebar tree for the given viewer.
    ///
    /// Walks the full model registry and synthesises a default
    /// [`AdminRegistration`] for every model not explicitly registered.
    /// Explicit registrations override the synthesised defaults (same
    /// table name = explicit wins).
    ///
    /// Ordering: plugins sorted alphabetically with the implicit `"app"`
    /// bucket rendered last; models within each group sorted by label.
    ///
    /// # Permission filtering
    ///
    /// Currently any staff user sees everything. When `umbra-permissions`
    /// lands, add per-model `view_<table>` permission checks here and
    /// filter `entries` accordingly before grouping.
    pub fn apps(&self, _viewer: &AuthUser) -> Vec<App> {
        // Build the merged map: start with synthesised defaults for every
        // model in the global registry, then overlay explicit registrations.
        let mut merged: HashMap<String, AdminRegistration> = HashMap::new();

        // Walk every plugin known to the migration registry.
        for plugin_name in umbra::migrate::registered_plugins() {
            for meta in umbra::migrate::models_for_plugin(&plugin_name) {
                let label = titlecase(&meta.display);
                let icon = meta.icon.clone();
                let table = meta.table.clone();
                let reg = AdminRegistration {
                    model: AdminModel::new(&table),
                    plugin: plugin_name.clone(),
                    label,
                    icon: Some(icon),
                };
                merged.insert(table, reg);
            }
        }
        // Also pick up models registered via `.model::<T>()` (the implicit
        // `"app"` plugin). These land in `registered_models()` but may not
        // appear in `registered_plugins()` if `"app"` contributed zero models
        // via a Plugin impl.
        for meta in umbra::migrate::registered_models() {
            if !merged.contains_key(&meta.table) {
                let label = titlecase(&meta.display);
                let icon = meta.icon.clone();
                let table = meta.table.clone();
                let reg = AdminRegistration {
                    model: AdminModel::new(&table),
                    plugin: "app".to_string(),
                    label,
                    icon: Some(icon),
                };
                merged.insert(table, reg);
            }
        }
        // Overlay explicit registrations — they always win.
        for (table, explicit) in &self.entries {
            merged.insert(table.clone(), explicit.clone());
        }

        // Group by plugin, sort, and produce the tree.
        let mut by_plugin: HashMap<String, Vec<AdminRegistration>> = HashMap::new();
        for reg in merged.into_values() {
            by_plugin.entry(reg.plugin.clone()).or_default().push(reg);
        }
        let mut apps: Vec<App> = by_plugin
            .into_iter()
            .map(|(plugin, mut models)| {
                models.sort_by(|a, b| a.label.cmp(&b.label));
                let label = plugin.clone();
                App {
                    plugin,
                    label,
                    models,
                }
            })
            .collect();
        // Named plugins alphabetically first; the implicit "app" bucket last.
        apps.sort_by(|a, b| match (a.plugin.as_str(), b.plugin.as_str()) {
            ("app", "app") => std::cmp::Ordering::Equal,
            ("app", _) => std::cmp::Ordering::Greater,
            (_, "app") => std::cmp::Ordering::Less,
            _ => a.plugin.cmp(&b.plugin),
        });
        apps
    }

    /// Look up the registration for a table by name.
    pub fn get(&self, table: &str) -> Option<&AdminRegistration> {
        self.entries.get(table)
    }

    /// Iterate all registrations. Used when building the legacy
    /// `configs` slice that the existing routing code depends on.
    pub fn all(&self) -> impl Iterator<Item = &AdminRegistration> {
        self.entries.values()
    }
}

/// Titlecase a string: replace `_` with space, capitalise the first
/// character of each word (split on `_` and space).
fn titlecase(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    s.split('_')
        .map(|word| {
            let mut c = word.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
