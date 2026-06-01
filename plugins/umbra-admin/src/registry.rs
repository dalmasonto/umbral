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
            let t = model.table.replace('_', " ");
            let mut c = t.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
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
    /// Returns plugins sorted by plugin name; models within each group
    /// sorted by label.
    ///
    /// # Permission filtering
    ///
    /// Currently any staff user sees everything. When `umbra-permissions`
    /// lands, add per-model `view_<table>` permission checks here and
    /// filter `entries` accordingly before grouping.
    pub fn apps(&self, _viewer: &AuthUser) -> Vec<App> {
        // Group by plugin.
        let mut by_plugin: HashMap<String, Vec<AdminRegistration>> = HashMap::new();
        for reg in self.entries.values() {
            by_plugin
                .entry(reg.plugin.clone())
                .or_default()
                .push(reg.clone());
        }
        // Sort each group by label, then sort groups by plugin name.
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
        apps.sort_by(|a, b| a.plugin.cmp(&b.plugin));
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
