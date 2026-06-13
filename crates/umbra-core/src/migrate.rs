//! The migration engine — the north star.
//!
//! Implements the **declare → migrate → change → migrate** cycle from
//! `arch.md §0`. Users declare or change a model, run `migrate`, and the
//! framework either generates the missing migration file (via `make`)
//! or applies pending migration files (via `run`).
//!
//! At M5 (this milestone) the surface ships:
//!
//! - A process-wide [`ModelRegistry`] populated by
//!   `App::builder().model::<T>()`.
//! - A [`Snapshot`] of every registered model's metadata, JSON-
//!   serialisable so it can live inside a migration file's
//!   `snapshot_after`.
//! - An [`Operation`] enum with the two minimum-viable ops:
//!   [`Operation::CreateTable`] and [`Operation::DropTable`]. Column-
//!   level ops (`AddColumn`, `DropColumn`, `AlterColumn`) land at M8
//!   alongside rename detection (per `arch.md §7` and
//!   `docs/specs/06-migration-engine.md`). The "M5.1" label in the
//!   `UnsupportedChange` error message is shorthand for the same slot.
//! - A [`MigrationFile`] format (one JSON file per migration) carrying
//!   `id`, `operations`, and `snapshot_after`.
//! - The `umbra_migrations` tracking table (one row per applied
//!   migration, keyed by `(plugin, name)`).
//! - High-level entry points: [`make`], [`run`], [`show`].
//!
//! Reserved for M5.1+:
//!
//! - Column-level ops (`AddColumn`, `DropColumn`, `AlterColumn`).
//! - Rename-detection vs drop+add disambiguation (spec 06 §M8).
//! - `RunSql` / `RunCode` data-migration ops.
//! - Squashing, `--fake`, `--fake-initial` (PRD F-MIG-6 P2).
//! - Cross-plugin migration dependencies (needs M7 Plugin contract).
//!
//! See `docs/specs/06-migration-engine.md` for the full target shape.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::backend::DatabaseBackend;
use crate::orm::{FieldSpec, Model, SqlType};

/// Per-process model registry. Published by `AppBuilder::build()`
/// after `.model::<T>()` calls and `.plugin(...)` registrations
/// collected metadata into the builder.
///
/// Stored as a flat vector of `(plugin_name, model)` pairs so M5's
/// existing `registered_models()` keeps working (drop the plugin
/// names) and the M7 plugin-aware walks (`registered_plugins`,
/// `models_for_plugin`) can read the same source of truth without a
/// second registry. The plugin name `"app"` covers models registered
/// via `.model::<T>()`; every other name is a real Plugin's.
static REGISTRY: OnceLock<Vec<(String, ModelMeta)>> = OnceLock::new();

/// Initialize the registry with one entry per plugin.
///
/// `App::build()` calls this after collecting `.model::<T>()` into the
/// implicit `"app"` plugin and walking every registered plugin's
/// `Plugin::models()`. Plugins missing from the map contribute zero
/// models (default-noop `models()` returns an empty vec; the entry
/// can be omitted).
pub(crate) fn init_plugins(per_plugin: std::collections::HashMap<String, Vec<ModelMeta>>) {
    let mut flat: Vec<(String, ModelMeta)> = Vec::new();
    let mut plugin_names: Vec<String> = per_plugin.keys().cloned().collect();
    plugin_names.sort();
    for plugin in plugin_names {
        for m in per_plugin.get(&plugin).cloned().unwrap_or_default() {
            flat.push((plugin.clone(), m));
        }
    }
    REGISTRY
        .set(flat)
        .expect("umbra::migrate::init_plugins called more than once");
}

/// Return every registered model, flat. Drops the per-plugin grouping;
/// useful when the caller only needs the model set (e.g. M5's `make`
/// when the codebase only had a single `"app"` plugin).
///
/// # Panics
///
/// Panics if `App::build()` hasn't run.
pub fn registered_models() -> Vec<ModelMeta> {
    REGISTRY
        .get()
        .expect("umbra: model registry not initialised — did you call App::build()?")
        .iter()
        .map(|(_, m)| m.clone())
        .collect()
}

/// Whether the model registry has been initialised. False before
/// `App::build()` has run; true after the phase-3 `init_plugins`
/// call publishes the per-plugin map. Used by system checks that
/// walk the registry — they return an empty result when the
/// registry isn't ready rather than panicking (so low-level tests
/// that drive `check::run_all` without booting an App keep working).
pub fn is_initialised() -> bool {
    REGISTRY.get().is_some()
}

/// PK lift Pass E — cached `(pk_column_name, pk_sql_type)` lookup
/// keyed by table name. Used by the FK decode path
/// (`fk_target_pk_sql_type` in `orm/dynamic.rs`) and the
/// select_related hydrators, both of which previously cloned the
/// full `Vec<ModelMeta>` per call and linear-scanned for the
/// target's PK column.
///
/// REGISTRY is a `OnceLock` set once during `App::build`; this cache
/// reads from it the first time anyone asks for a PK lookup AFTER
/// initialisation, then serves from a `HashMap` for every
/// subsequent call. Eliminates the per-row `registered_models()`
/// clone in hot decode loops.
///
/// Returns `None` when the registry isn't initialised (the cache
/// stays uninstantiated so a follow-up call after `App::build`
/// gets the real table set), OR when the named table isn't in the
/// registry (orphan / system / typo).
pub fn pk_meta_for_table(table: &str) -> Option<(String, crate::orm::SqlType)> {
    if !is_initialised() {
        // Defer cache init until App::build has populated REGISTRY.
        // The cache MUST NOT memoize an empty map; otherwise
        // post-init callers would see no PK metadata forever.
        return None;
    }
    static CACHE: std::sync::OnceLock<
        std::collections::HashMap<String, (String, crate::orm::SqlType)>,
    > = std::sync::OnceLock::new();
    let map = CACHE.get_or_init(|| {
        let mut out = std::collections::HashMap::new();
        for m in registered_models() {
            if let Some(pk) = m.pk_column() {
                out.insert(m.table.clone(), (pk.name.clone(), pk.ty));
            }
        }
        out
    });
    map.get(table).cloned()
}

/// Return the registered plugin names that contributed at least one
/// model. Sorted deterministically. Used as a fallback when no
/// topological order is published; the M7 walk used this directly,
/// and M8 prefers [`plugin_order`] when it's been set.
pub fn registered_plugins() -> Vec<String> {
    let mut names: Vec<String> = REGISTRY
        .get()
        .expect("umbra: model registry not initialised — did you call App::build()?")
        .iter()
        .map(|(p, _)| p.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The topological plugin order published by `App::build()` after its
/// phase 1.5 sort. `None` until that runs; the CLI subcommands
/// (`makemigrations`, `migrate`, `showmigrations`) call `App::build()`
/// via `boot_for_management` before reaching the migration engine.
static PLUGIN_ORDER: OnceLock<Vec<String>> = OnceLock::new();

/// Per-model database alias (`Model::NAME -> alias`) published by
/// `App::build()` after walking each registered plugin's
/// `Plugin::database()`. Models whose plugin returned `None` are
/// absent from the map; QuerySet's `resolve_pool` falls back to the
/// `"default"` alias for those. Lookup is `O(1)` on a `HashMap`.
static MODEL_ALIASES: OnceLock<std::collections::HashMap<String, String>> = OnceLock::new();

/// Publish the topological plugin order. Called by `App::build()` once
/// the phase 1.5 sort has produced the order. Must include the
/// implicit `"app"` plugin even when no real plugins are registered.
pub(crate) fn init_plugin_order(order: Vec<String>) {
    PLUGIN_ORDER
        .set(order)
        .expect("umbra::migrate::init_plugin_order called more than once");
}

/// Return the topological plugin order if `App::build()` published
/// one; otherwise fall back to [`registered_plugins`] (sorted by
/// name). The fallback keeps existing M5 / M6 tests working without
/// requiring them to wire a full plugin sort.
pub fn plugin_order() -> Vec<String> {
    PLUGIN_ORDER
        .get()
        .cloned()
        .unwrap_or_else(registered_plugins)
}

/// The client-facing API endpoints every registered plugin advertised
/// via `Plugin::api_endpoints()`, collected by `App::build()`. `None`
/// until that runs; an app with no advertising plugins publishes an
/// empty vec.
static API_ENDPOINTS: OnceLock<Vec<crate::plugin::ApiEndpoint>> = OnceLock::new();

/// Publish the collected `Plugin::api_endpoints()`. Called once by
/// `App::build()` after walking every registered plugin.
pub(crate) fn init_api_endpoints(endpoints: Vec<crate::plugin::ApiEndpoint>) {
    let _ = API_ENDPOINTS.set(endpoints);
}

/// Every callable endpoint registered plugins advertised for service
/// discovery, in plugin-registration order. Empty until `App::build()`
/// has run. A REST API root (or any discovery surface) reads this to
/// list plugin endpoints without depending on those plugins' crates.
pub fn registered_api_endpoints() -> Vec<crate::plugin::ApiEndpoint> {
    API_ENDPOINTS.get().cloned().unwrap_or_default()
}

/// Publish the per-model alias routing. Called by `App::build()`
/// during phase 3 after walking every plugin's `Plugin::database()`.
/// Plugins that returned `None` contribute no entries; only the
/// explicit overrides land here.
pub(crate) fn init_model_aliases(map: std::collections::HashMap<String, String>) {
    MODEL_ALIASES
        .set(map)
        .expect("umbra::migrate::init_model_aliases called more than once");
}

/// Look up the database alias for a SQL table name — the reverse of
/// the `Model::NAME → alias` lookup that [`model_alias`] does. Walks
/// the registered model metas to find the one whose `table` matches
/// (snake_case of the struct name + any `#[umbra(table = "...")]`
/// override) and returns its alias if set. Falls back to `"default"`
/// when no model owns the table (e.g. orphan schema, the
/// `umbra_migrations` table itself) — those land on the main pool.
///
/// Used by the migration engine's per-DB dispatch in [`run_in`] to
/// route each operation to the right pool.
pub fn table_alias(table_name: &str) -> String {
    for meta in registered_models() {
        if meta.table == table_name {
            return model_alias(&meta.name).unwrap_or_else(|| "default".to_string());
        }
    }
    "default".to_string()
}

/// Look up the database alias for one model. Returns `None` if the
/// model isn't routed explicitly (the caller falls back to the
/// `"default"` pool); returns `None` even when the alias map hasn't
/// been initialised so low-level tests that drive `init_plugins`
/// directly don't have to wire a second call.
pub fn model_alias(model_name: &str) -> Option<String> {
    MODEL_ALIASES.get()?.get(model_name).cloned()
}

/// Return the models registered against a specific plugin. Empty if
/// no plugin by that name registered models.
pub fn models_for_plugin(plugin: &str) -> Vec<ModelMeta> {
    REGISTRY
        .get()
        .expect("umbra: model registry not initialised — did you call App::build()?")
        .iter()
        .filter(|(p, _)| p == plugin)
        .map(|(_, m)| m.clone())
        .collect()
}

/// Static metadata for one registered model, copied off the `Model`
/// trait's `const`s when the user calls `App::builder().model::<T>()`.
///
/// Owned (no lifetimes) so the registry can hold an arbitrary number
/// without the lifetime contortions a slice of trait references would
/// need. The cost is one Vec at `App::build` time; the win is
/// `registered_models()` having a plain `&'static [ModelMeta]` signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMeta {
    /// The struct name (`Model::NAME`). Identifies the model across
    /// snapshot diffs even if the table is renamed.
    pub name: String,
    /// The SQL table name (`Model::TABLE`).
    pub table: String,
    /// One owned column descriptor per field, in declaration order.
    /// Owned (`Column`, not the underlying static `FieldSpec`) so the
    /// snapshot round-trips cleanly through serde.
    pub fields: Vec<Column>,
    /// Human-readable display name from `Model::DISPLAY`. Defaults to
    /// `Model::NAME` when no `#[umbra(display = "...")]` is present.
    #[serde(default)]
    pub display: String,
    /// Lucide icon slug from `Model::ICON`. Defaults to `"database"`.
    #[serde(default = "default_icon")]
    pub icon: String,
    /// Database alias from `Model::DATABASE`, when set. `None` means
    /// "fall back to the owning plugin's `Plugin::database()`, then
    /// the `default` pool." Captured here so `App::build`'s alias
    /// routing can read it without re-reaching into the trait at a
    /// later phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// Mirrors `Model::SINGLETON`. Closes BUG-9 in
    /// `bugs/tests/testBugs.md`. Default `false`; admin renderers
    /// read it to auto-redirect list-view to the edit form.
    #[serde(default, skip_serializing_if = "is_false")]
    pub singleton: bool,
    /// Mirrors `Model::UNIQUE_TOGETHER`. Composite-UNIQUE constraints,
    /// each inner `Vec<String>` listing the columns of one constraint.
    /// Closes BUG-6.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unique_together: Vec<Vec<String>>,
    /// Mirrors `Model::INDEXES`. Each inner `Vec<String>` lists the
    /// columns of one multi-column index. Closes BUG-7.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<Vec<String>>,
    /// Mirrors `Model::ORDERING`. Each tuple is `(column, descending)`
    /// — `descending == true` lowers to `ORDER BY col DESC`. Closes
    /// BUG-8.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ordering: Vec<(String, bool)>,
    /// Mirrors `Model::M2M_RELATIONS`. Many-to-many relations declared
    /// on this model. The migration engine uses this to auto-generate
    /// junction tables. Closes BUG-16.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub m2m_relations: Vec<M2MRelation>,
    /// Mirrors `Model::SOFT_DELETE` (`#[umbra(soft_delete)]`). The
    /// dynamic / annotate paths read this to auto-exclude
    /// `deleted_at IS NULL` children from correlated counts and to
    /// drive trash-aware admin views without re-reaching into the
    /// typed trait. Shared enabler for gaps2 #35 + #39a.
    #[serde(default, skip_serializing_if = "is_false")]
    pub soft_delete: bool,
}

/// Owned mirror of `orm::M2MRelationSpec` so `ModelMeta` can be
/// serialised into migration JSON without lifetimes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct M2MRelation {
    pub field_name: String,
    pub target_table: String,
    pub target_name: String,
}

fn default_icon() -> String {
    "database".to_string()
}

/// Serde default for [`Operation::CreateM2MTable`]'s `parent_ty` /
/// `child_ty` fields. Older snapshot files (pre-phase-2) had no
/// per-side PK type and assumed `BigInt` on both ends — this keeps
/// them round-tripping without rewrites.
fn default_bigint() -> crate::orm::SqlType {
    crate::orm::SqlType::BigInt
}

impl ModelMeta {
    /// The primary-key column on this model. Every umbra model
    /// has exactly one PK by construction (the derive enforces
    /// it), but the lookup is `Option`-shaped because nothing
    /// stops a hand-written `ModelMeta` (test fixtures, etc.)
    /// from omitting it.
    pub fn pk_column(&self) -> Option<&Column> {
        self.fields.iter().find(|c| c.primary_key)
    }

    /// Read static metadata off `T: Model` into an owned `ModelMeta`.
    /// Called from `AppBuilder::model::<T>()`.
    pub fn for_<T: Model>() -> Self {
        Self {
            name: T::NAME.to_string(),
            table: T::TABLE.to_string(),
            fields: T::FIELDS.iter().map(Column::from).collect(),
            display: T::DISPLAY.to_string(),
            icon: T::ICON.to_string(),
            database: T::DATABASE.map(|s| s.to_string()),
            singleton: T::SINGLETON,
            unique_together: T::UNIQUE_TOGETHER
                .iter()
                .map(|group| group.iter().map(|s| s.to_string()).collect())
                .collect(),
            indexes: T::INDEXES
                .iter()
                .map(|group| group.iter().map(|s| s.to_string()).collect())
                .collect(),
            ordering: T::ORDERING
                .iter()
                .map(|(col, desc)| (col.to_string(), *desc))
                .collect(),
            m2m_relations: T::M2M_RELATIONS
                .iter()
                .map(|r| M2MRelation {
                    field_name: r.field_name.to_string(),
                    target_table: r.target_table.to_string(),
                    target_name: r.target_name.to_string(),
                })
                .collect(),
            soft_delete: T::SOFT_DELETE,
        }
    }
}

/// A snapshot of every registered model at a point in time.
///
/// Serialised into the `snapshot_after` field of a migration file so
/// future `makemigrations` runs can diff against it without replaying
/// every prior migration's operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Snapshot {
    /// Models sorted by name so the JSON is deterministic and the
    /// snapshot_hash is stable across runs that produce equivalent
    /// content.
    pub models: Vec<ModelMeta>,
}

impl Snapshot {
    /// Build a snapshot from the live registry (the current state of
    /// the application's models, post-`App::build`).
    pub fn current() -> Self {
        let mut models = registered_models().to_vec();
        models.sort_by(|a, b| a.name.cmp(&b.name));
        Self { models }
    }

    /// Build a snapshot containing only the models registered
    /// against the given plugin. Used by `make_in` to diff each
    /// plugin's migrations independently against its own prior
    /// snapshot, so cross-plugin model sets don't bleed into one
    /// migration file.
    pub fn current_for(plugin: &str) -> Self {
        let mut models = models_for_plugin(plugin);
        models.sort_by(|a, b| a.name.cmp(&b.name));
        Self { models }
    }

    /// Compute the snapshot's SHA-256 hash, hex-encoded. Stored in the
    /// `umbra_migrations.snapshot_hash` column for drift detection.
    pub fn hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let json = serde_json::to_string(self).expect("Snapshot serializes");
        let digest = Sha256::digest(json.as_bytes());
        hex(&digest[..])
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// One operation inside a migration. The migration engine renders each
/// operation to SQL via the active backend (M4 `DatabaseBackend::
/// map_type`) and runs them in declaration order inside one
/// transaction per migration file.
///
/// M5 v1 shipped table-level ops; M8 v1 adds `AddColumn` and
/// `DropColumn`. `AlterColumn`, index / constraint ops, and
/// `RunSql` / `RunCode` are deferred (see `docs/specs/06-migration-
/// engine.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Operation {
    /// Create a new table. `columns` is in declaration order; the
    /// engine builds a sea-query `Table::create()` over them and runs
    /// the rendered DDL. `unique_together` lowers to inline
    /// `UNIQUE (col1, col2)` clauses; `indexes` lowers to follow-up
    /// `CREATE INDEX` statements after the table is created. Both
    /// default to empty for backward-compat with older snapshots.
    CreateTable {
        table: String,
        columns: Vec<Column>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unique_together: Vec<Vec<String>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        indexes: Vec<Vec<String>>,
    },
    /// Drop an existing table.
    DropTable { table: String },
    /// Add a new column to an existing table. Rendered as
    /// `ALTER TABLE x ADD COLUMN y TYPE [NOT NULL]`. SQLite refuses a
    /// non-nullable add against a populated table without a default;
    /// the engine surfaces that as a sqlx error at apply time (M8 v1).
    /// A future op `AddColumnWithDefault` lifts the restriction once
    /// the `#[umbra(default = ...)]` attribute lands.
    AddColumn { table: String, column: Column },
    /// Drop a column from an existing table. Rendered as
    /// `ALTER TABLE x DROP COLUMN y`. SQLite 3.35+ and Postgres
    /// support this natively; older SQLite would need a table-
    /// recreation dance the engine doesn't implement.
    DropColumn { table: String, column: String },
    /// Alter a column's nullable flag (the only safe in-place change
    /// the engine ships at M5.1). Self-contained: carries the full
    /// new column list so the SQLite table-recreation dance can
    /// rebuild the schema without re-reading the snapshot. The
    /// `column` field names the specific column that triggered the
    /// alter (used for the filename suffix and diagnostics); the
    /// `new_columns` list is the post-change schema.
    AlterColumn {
        table: String,
        column: String,
        new_columns: Vec<Column>,
        /// Snapshot of the table's columns *before* this alter. Carried
        /// so the Postgres renderer can decide per-column whether it
        /// needs a TYPE/USING clause vs a SET/DROP NOT NULL — without
        /// re-walking the snapshot file. `Option` + `serde(default)`
        /// keeps older on-disk migrations deserialising cleanly; ops
        /// produced before this field existed get `None` and fall back
        /// to the legacy nullable-only Postgres path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prev_columns: Option<Vec<Column>>,
    },
    /// Rename an existing table. Emitted by `diff` when a model's table
    /// name changes but its `Model::NAME` (the Rust struct name) stays
    /// the same (first-pass detection), or when the column shapes are
    /// bit-identical and the struct name changed too (second-pass
    /// heuristic detection). Both SQLite and Postgres render as
    /// `ALTER TABLE "<from>" RENAME TO "<to>"`.
    ///
    /// The migration tracking table records `(plugin, name)` of each
    /// applied migration — it is not affected by a table rename inside
    /// the migration.
    RenameTable { from: String, to: String },
    /// Create a many-to-many junction table. Auto-emitted when a model
    /// gains an `M2M<T>` field. Closes BUG-16 phase 2.
    ///
    /// The junction table name is `parent_table_field_name`. Columns:
    /// `parent_id` (FK to parent), `child_id` (FK to target), both with
    /// `ON DELETE CASCADE`. Composite PK `(parent_id, child_id)`.
    ///
    /// `parent_ty` and `child_ty` carry the SQL types of the
    /// referenced PK columns — `BigInt` for an `i64` PK, `Text` for a
    /// `String` slug, `Uuid` for a `uuid::Uuid`. The renderer maps
    /// these to the right column type per backend; without them the
    /// junction's `child_id INTEGER` would reject a string codename
    /// at insert time. `#[serde(default)]` keeps older snapshot files
    /// (pre-phase-2) round-tripping — they default to `BigInt`,
    /// matching the original i64-only behaviour.
    CreateM2MTable {
        junction_table: String,
        parent_table: String,
        parent_col: String,
        child_table: String,
        child_col: String,
        #[serde(default = "default_bigint")]
        parent_ty: crate::orm::SqlType,
        #[serde(default = "default_bigint")]
        child_ty: crate::orm::SqlType,
    },
    /// Drop a many-to-many junction table. Auto-emitted when an `M2M<T>`
    /// field is removed from a model.
    DropM2MTable { junction_table: String },
    /// Gap 88: rename a column on an existing table. Emitted by the
    /// diff engine when a single column with one shape was dropped
    /// and one with the same shape was added in the same diff —
    /// the heuristic match for "the user renamed `title` to
    /// `headline`." Both SQLite (3.25+) and Postgres render as
    /// `ALTER TABLE "<t>" RENAME COLUMN "<from>" TO "<to>"`.
    ///
    /// `column` carries the post-rename column shape so the
    /// snapshot stays in sync. The migration only renames; never
    /// alters other column attributes — a rename combined with a
    /// type change emits a RenameColumn AND a follow-on AlterColumn
    /// against the new name.
    RenameColumn {
        table: String,
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column: Option<Column>,
    },
}

impl Operation {
    /// The primary table this operation targets. For `RenameTable`,
    /// returns the source name (the post-rename `to` lives in the new
    /// snapshot, but routing decisions look up the model meta by its
    /// pre-rename `from`).
    ///
    /// Used by `run_in`'s per-DB dispatch loop to route each op to the
    /// pool where its table actually lives.
    pub fn table_name(&self) -> &str {
        match self {
            Operation::CreateTable { table, .. }
            | Operation::DropTable { table }
            | Operation::AddColumn { table, .. }
            | Operation::DropColumn { table, .. }
            | Operation::AlterColumn { table, .. }
            | Operation::RenameColumn { table, .. } => table,
            Operation::RenameTable { from, .. } => from,
            Operation::CreateM2MTable { junction_table, .. }
            | Operation::DropM2MTable { junction_table } => junction_table,
        }
    }
}

/// One column inside a [`Operation::CreateTable`].
///
/// Mirrors the structure of [`FieldSpec`] but is fully owned for
/// serialisation. Reconstructed from a `FieldSpec` at diff time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub ty: SqlType,
    pub primary_key: bool,
    pub nullable: bool,
    /// For `SqlType::ForeignKey` columns: the SQL table name of the
    /// referenced model. `None` for all non-FK columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fk_target: Option<String>,
    /// When `true`, this field is never shown on any admin form (create or
    /// edit). Propagated from `FieldSpec::noform`.
    #[serde(default)]
    pub noform: bool,
    /// When `true`, this field appears on the edit form as read-only.
    /// Propagated from `FieldSpec::noedit`.
    #[serde(default)]
    pub noedit: bool,
    /// Django-style `__str__` marker — propagated from
    /// `FieldSpec::is_string_repr`. The admin uses the first column
    /// with this flag as the default `list_display` label when no
    /// explicit one is configured.
    #[serde(default)]
    pub is_string_repr: bool,
    /// Display truncation cap — propagated from `FieldSpec::max_length`.
    /// `0` means no truncation.
    #[serde(default)]
    pub max_length: u32,
    /// Closed-set DB values for a choices column. Propagated from
    /// `FieldSpec::choices`. Non-empty when the model field carries
    /// `#[umbra(choices)]`; the migration engine emits a Postgres
    /// `CHECK (col IN (...))` constraint when this slice is non-empty.
    /// Empty for every non-choices column.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// Human labels matching `choices` position-for-position. Carried
    /// alongside `choices` so the admin's `<select>` widget has labels
    /// without the runtime needing to reflect on the model type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choice_labels: Vec<String>,
    /// SQL `DEFAULT` value — propagated from `FieldSpec::default`.
    /// Empty string means no default. The migration engine reads this
    /// at DDL-emit time for both `CREATE TABLE` and `ALTER TABLE ADD
    /// COLUMN`. Set via `#[umbra(default = "...")]` on the model field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default: String,
    /// Distinguishes a multi-valued [`MultiChoice<E>`] column from a
    /// single-valued choices column. Both share `ty: Text` plus the same
    /// `choices` / `choice_labels` metadata; this flag is the only
    /// signal that the value is a CSV. Empty / false for every other
    /// column.
    ///
    /// [`MultiChoice<E>`]: crate::orm::MultiChoice
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_multichoice: bool,

    /// Carries `FieldSpec::unique` into the migration snapshot. The
    /// DDL builders emit a `UNIQUE` clause on this column at
    /// `CREATE TABLE` time when set. Default `false` keeps existing
    /// migration JSON files round-tripping unchanged (the field is
    /// omitted on serialise when default).
    #[serde(default, skip_serializing_if = "is_false")]
    pub unique: bool,

    /// Carries `FieldSpec::on_delete` into the migration snapshot.
    /// FK columns only — the DDL builders emit
    /// `ON DELETE <action>` when this is anything other than
    /// `NoAction`. Default `NoAction` is omitted from JSON so
    /// existing migration files round-trip without churn.
    #[serde(default, skip_serializing_if = "is_no_action")]
    pub on_delete: crate::orm::FkAction,

    /// Carries `FieldSpec::on_update` into the migration snapshot.
    /// Same shape as `on_delete`; emits `ON UPDATE <action>`.
    #[serde(default, skip_serializing_if = "is_no_action")]
    pub on_update: crate::orm::FkAction,

    /// Carries `FieldSpec::index` into the migration snapshot. The
    /// CreateTable + AddColumn render paths emit a matching
    /// `CREATE INDEX idx_<table>_<col>` for every column whose
    /// flag is set. Default `false` keeps existing migration JSON
    /// round-tripping unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub index: bool,

    /// Carries `FieldSpec::auto_now_add` into the migration
    /// snapshot. The dynamic write path (`DynQuerySet::insert_json`)
    /// auto-populates the column with `Utc::now()` when the body
    /// omits it. Default `false` so existing migration JSON
    /// round-trips unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto_now_add: bool,

    /// Carries `FieldSpec::auto_now` into the migration snapshot.
    /// Same shape as `auto_now_add` but fires on update too.
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto_now: bool,

    /// Carries `FieldSpec::help` into the migration snapshot.
    /// Default empty string is omitted from JSON so existing
    /// migration files round-trip unchanged.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub help: String,

    /// Carries `FieldSpec::example` into the migration snapshot.
    /// Same shape as `help`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub example: String,

    /// Carries `FieldSpec::widget` into the migration snapshot — the
    /// form-renderer presentation hint (features.md #4). Presentation
    /// only, no DB effect, so it's excluded from the schema diff the
    /// same way `help` / `example` are. `None` is omitted from JSON so
    /// existing migration files round-trip unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget: Option<String>,

    /// Carries `FieldSpec::supported_backends` into the migration
    /// snapshot. When non-empty, the boot system check rejects the
    /// model on any backend not listed. Closes IMP-5 from
    /// `bugs/tests/testBugs.md`. Default empty (works on every
    /// backend); JSON skip-when-empty so existing migration files
    /// don't churn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_backends: Vec<String>,

    /// IMP-3: numeric lower bound. `None` means "no minimum"; the
    /// DDL emits a `CHECK (col >= N)` constraint when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,

    /// IMP-3: numeric upper bound. Same shape as `min`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,

    /// BUG-11/12/13: constrained-text marker. `None` is plain text;
    /// `Some("slug" | "email" | "url")` flags the column as a
    /// `Slug` / `Email` / `Url` wrapper. OpenAPI emits the
    /// corresponding `format` / `pattern`; the REST plugin
    /// pre-validates the body via `validate_text_format`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_format: Option<String>,

    /// Gap 109: auto-derive source. When `Some("title")`, the slug is
    /// computed from the row's `title` column at write time if the
    /// slug column itself is empty / missing on the body. Pure
    /// runtime behaviour — has no DDL effect, so the diff engine
    /// ignores changes to this field. `#[serde(default)]` keeps
    /// older snapshots round-tripping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug_from: Option<String>,
}

fn is_no_action(a: &crate::orm::FkAction) -> bool {
    matches!(a, crate::orm::FkAction::NoAction)
}

/// Build a portable `CREATE INDEX IF NOT EXISTS idx_<table>_<col>
/// ON "<table>" ("<col>")` statement. Same DDL on SQLite and
/// Postgres — both accept `CREATE INDEX IF NOT EXISTS` and the
/// `idx_<table>_<col>` name convention is unique enough that the
/// migration engine can re-emit it idempotently on subsequent
/// applies. Used by [`render_operation_sqlite`] / `_postgres`
/// after a `CreateTable` or `AddColumn` op whose column carries
/// the `#[umbra(index)]` flag. Closes BUG-4.
fn create_index_stmt(table: &str, column: &str) -> String {
    let t = table.replace('"', "\"\"");
    let c = column.replace('"', "\"\"");
    format!(
        "CREATE INDEX IF NOT EXISTS \"idx_{table}_{column}\" ON \"{t}\" (\"{c}\")",
        table = table.replace('"', ""),
        column = column.replace('"', ""),
    )
}

/// Multi-column variant of [`create_index_stmt`]. Closes BUG-7.
/// Renders `CREATE INDEX IF NOT EXISTS idx_<table>_<col1>_<col2>
/// ON "<table>" ("<col1>", "<col2>")`. Both backends accept the
/// same form. Empty groups render no statement (defensive — the
/// macro layer rejects them before the engine sees them, but the
/// helper still returns a no-op SQL string to keep the caller
/// simple).
fn create_multi_index_stmt(table: &str, columns: &[String]) -> String {
    if columns.is_empty() {
        return String::new();
    }
    let t = table.replace('"', "");
    let name_suffix = columns
        .iter()
        .map(|c| c.replace('"', ""))
        .collect::<Vec<_>>()
        .join("_");
    let col_list = columns
        .iter()
        .map(|c| format!("\"{}\"", c.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "CREATE INDEX IF NOT EXISTS \"idx_{t}_{name_suffix}\" ON \"{t}\" ({col_list})",
        t = t.replace('"', "\"\""),
    )
}

/// Lower an M2M junction column's PK type into the SQLite column
/// declaration string used inside the raw `CREATE TABLE` template.
/// SQLite has affinity types: every integer width stores as `INTEGER`
/// (one ROWID-aliased column), and TEXT covers `String` / `Uuid`.
/// Closes BUG-16 phase 2.
fn m2m_pk_sql_type_sqlite(ty: crate::orm::SqlType) -> &'static str {
    use crate::orm::SqlType;
    match ty {
        SqlType::SmallInt | SqlType::Integer | SqlType::BigInt | SqlType::ForeignKey => "INTEGER",
        SqlType::Text | SqlType::Uuid => "TEXT",
        // The macro-side classifier only sets these for PK columns
        // when the user wrote a non-standard PK type. If we ever
        // see one here that doesn't make sense as a junction column
        // (Boolean, Date, Real, …), TEXT is the safest catch-all
        // affinity — SQLite will accept it and the rest of the
        // ORM will surface the deeper "this can't be a PK" error
        // through the system check.
        _ => "TEXT",
    }
}

/// Lower an M2M junction column's PK type into the Postgres column
/// declaration string. Postgres is strict about types — `BIGINT` for
/// 64-bit integers, `INTEGER` for 32-bit, `SMALLINT` for 16-bit,
/// `TEXT` for `String`, `UUID` for `uuid::Uuid`. Mirrors the choices
/// `build_column_def_postgres` makes for the same `SqlType` variants.
fn m2m_pk_sql_type_postgres(ty: crate::orm::SqlType) -> &'static str {
    use crate::orm::SqlType;
    match ty {
        SqlType::SmallInt => "SMALLINT",
        SqlType::Integer => "INTEGER",
        SqlType::BigInt | SqlType::ForeignKey => "BIGINT",
        SqlType::Text => "TEXT",
        SqlType::Uuid => "UUID",
        _ => "TEXT",
    }
}

/// Build the ` ON DELETE <action> ON UPDATE <action>` suffix for a
/// FK column. Each half is emitted only when its action is anything
/// other than `NoAction` — keeps the generated DDL minimal and
/// matches the SQL standard's default (NO ACTION when the clause is
/// omitted).
///
/// Closes gap #68. Shared between the SQLite and Postgres builders
/// because the REFERENCES tail syntax is identical on both.
fn fk_action_suffix(col: &Column) -> String {
    let mut s = String::new();
    if let Some(kw) = col.on_delete.sql_keyword() {
        s.push_str(" ON DELETE ");
        s.push_str(kw);
    }
    if let Some(kw) = col.on_update.sql_keyword() {
        s.push_str(" ON UPDATE ");
        s.push_str(kw);
    }
    s
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl From<&FieldSpec> for Column {
    fn from(f: &FieldSpec) -> Self {
        Self {
            name: f.name.to_string(),
            ty: f.ty,
            primary_key: f.primary_key,
            nullable: f.nullable,
            fk_target: f.fk_target.map(|s| s.to_string()),
            noform: f.noform,
            noedit: f.noedit,
            is_string_repr: f.is_string_repr,
            max_length: f.max_length,
            choices: f.choices.iter().map(|s| s.to_string()).collect(),
            choice_labels: f.choice_labels.iter().map(|s| s.to_string()).collect(),
            default: f.default.to_string(),
            is_multichoice: f.is_multichoice,
            unique: f.unique,
            on_delete: f.on_delete,
            on_update: f.on_update,
            index: f.index,
            auto_now_add: f.auto_now_add,
            auto_now: f.auto_now,
            help: f.help.to_string(),
            example: f.example.to_string(),
            widget: f.widget.map(|s| s.to_string()),
            supported_backends: f.supported_backends.iter().map(|s| s.to_string()).collect(),
            min: f.min,
            max: f.max,
            text_format: f.text_format.map(|s| s.to_string()),
            slug_from: f.slug_from.map(|s| s.to_string()),
        }
    }
}

/// The on-disk shape of one migration. Files in `migrations/<plugin>/`
/// deserialize into this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationFile {
    /// Stable id, matches the filename minus `.json`.
    pub id: String,
    /// The plugin that owns this migration. M5 hardcodes `"app"` for
    /// the user's binary; M7 generalises to one directory per plugin.
    pub plugin: String,
    /// Predecessor migrations, in `(plugin, id)` form. Within-plugin
    /// predecessors are implicit (the prior numeric file); cross-
    /// plugin predecessors land at M7.
    #[serde(default)]
    pub depends_on: Vec<MigrationRef>,
    /// Ordered operations applied when this migration runs.
    pub operations: Vec<Operation>,
    /// The full snapshot of every model after this migration has run.
    /// Source of truth for the next `make` to diff against.
    pub snapshot_after: Snapshot,
}

/// A pointer to one (plugin, migration_id) pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationRef {
    pub plugin: String,
    pub migration: String,
}

/// At M5 every migration belongs to a single placeholder plugin. M7's
/// Plugin contract replaces this with `Plugin::name()`.
pub const APP_PLUGIN_NAME: &str = "app";

/// Default directory for migration files. `make` writes into
/// `migrations/<plugin>/`; `run` reads from the same place. Override
/// with `--migrations-dir` once the CLI grows real arg parsing (M5+).
pub const MIGRATIONS_DIR: &str = "migrations";

/// The state of a single migration from the perspective of drift detection.
/// Returned inside [`DriftReport`] so callers can decide how to handle each
/// state independently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationStatus {
    /// The migration is recorded in the tracking table AND the file
    /// exists on disk. Normal applied state.
    Applied,
    /// The migration is recorded in the tracking table BUT the
    /// corresponding file is missing from disk. The database is ahead
    /// of what version control has; recovering requires restoring the
    /// file or running with `--allow-drift`.
    AppliedButMissing,
    /// The migration file exists on disk AND its sequence number is
    /// lower than the highest applied migration for this plugin, but it
    /// is not recorded in the tracking table. Looks like someone dropped
    /// a migration file back into a directory after a teammate already
    /// applied later ones. Should warn, not error.
    OutOfOrder,
    /// Normal pending state: the file is on disk and its sequence number
    /// is higher than anything applied. Ready to apply.
    Pending,
}

/// Per-migration entry inside a [`DriftReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationEntry {
    pub plugin: String,
    pub name: String,
    pub status: MigrationStatus,
}

/// The output of [`detect_drift`]: one entry per migration (applied or
/// on-disk), categorised into the four states above.
///
/// The caller inspects `has_critical_drift()` to decide whether to abort
/// before applying migrations. Surfaced by `show_in_with_drift` for
/// `showmigrations` and checked by `run_in_with_drift_check` before
/// executing any SQL.
#[derive(Debug, Clone, Default)]
pub struct DriftReport {
    pub entries: Vec<MigrationEntry>,
}

impl DriftReport {
    /// Returns true when at least one migration is `AppliedButMissing`.
    /// This state means the tracking table references a file that no
    /// longer exists on disk — the operator needs to act before it is
    /// safe to continue applying new migrations.
    pub fn has_critical_drift(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.status == MigrationStatus::AppliedButMissing)
    }

    /// All migrations with `AppliedButMissing` status. Convenience
    /// accessor for building the error message.
    pub fn missing_on_disk(&self) -> Vec<&MigrationEntry> {
        self.entries
            .iter()
            .filter(|e| e.status == MigrationStatus::AppliedButMissing)
            .collect()
    }
}

/// Errors the migration engine can produce.
#[derive(Debug)]
pub enum MigrateError {
    /// IO error reading or writing a migration file or directory.
    Io(std::io::Error),
    /// JSON parse error on a migration file.
    Json(serde_json::Error),
    /// sqlx error executing a migration's SQL or touching the
    /// tracking table.
    Sqlx(sqlx::Error),
    /// `make` ran but found no differences against the latest snapshot,
    /// so there's nothing to write. Surfaced so the CLI can print
    /// "no changes detected" instead of an empty migration file.
    NoChanges,
    /// The current models diverge from the snapshot in a way M5 v1
    /// can't represent yet (anything other than create/drop table).
    /// M5.1 lifts this when column-level ops land.
    UnsupportedChange(String),
    /// A column-level change the engine can't apply automatically:
    /// type change, or a nullable flip on a populated SQLite table.
    /// Surfaces from `diff` so the build stops before producing a
    /// migration that would lose data or fail to apply. The user
    /// resolves by hand-writing the migration with the appropriate
    /// data-preserving steps. Carries the model / column / reason.
    UnsafeAlter {
        model: String,
        column: String,
        reason: String,
    },
    /// The tracking table records migrations that no longer have
    /// corresponding files on disk. Carries the list of missing names.
    /// The operator must either restore the files from VCS or run with
    /// `--allow-drift` to proceed despite the inconsistency.
    DriftDetected { missing: Vec<(String, String)> },
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrateError::Io(e) => write!(f, "umbra migrate: io: {e}"),
            MigrateError::Json(e) => write!(f, "umbra migrate: json: {e}"),
            MigrateError::Sqlx(e) => write!(f, "umbra migrate: sqlx: {e}"),
            MigrateError::NoChanges => write!(
                f,
                "umbra migrate: no changes detected; declare or change a model first"
            ),
            MigrateError::UnsupportedChange(msg) => {
                write!(f, "umbra migrate: unsupported change at M5 v1: {msg}")
            }
            MigrateError::UnsafeAlter {
                model,
                column,
                reason,
            } => write!(
                f,
                "umbra migrate: unsafe column change on `{model}.{column}`: {reason}; \
                 hand-write the migration with a data-preserving step"
            ),
            MigrateError::DriftDetected { missing } => {
                let names: Vec<String> = missing
                    .iter()
                    .map(|(plugin, name)| format!("{plugin}/{name}"))
                    .collect();
                write!(
                    f,
                    "umbra migrate: drift detected — the following migrations are recorded in \
                     the tracking table but their files are missing from disk:\n  {}\n\
                     Restore the files from VCS or run `umbra migrate --allow-drift` to \
                     proceed despite the inconsistency.",
                    names.join("\n  ")
                )
            }
        }
    }
}

impl std::error::Error for MigrateError {}

impl From<std::io::Error> for MigrateError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for MigrateError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<sqlx::Error> for MigrateError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

// =========================================================================
// Top-level entry points.
// =========================================================================

/// Generate one migration file per registered plugin that has changes,
/// diffing each plugin's current model set against the latest snapshot
/// in `migrations/<plugin>/`. Each new file lands inside its own
/// plugin directory with the next sequence number and a `_<short_name>`
/// suffix derived from the dominant operation.
///
/// Returns the paths of every file written, one per plugin that had a
/// non-empty diff. Returns `MigrateError::NoChanges` if no plugin
/// produced any changes at all.
pub async fn make() -> Result<Vec<PathBuf>, MigrateError> {
    make_in(Path::new(MIGRATIONS_DIR)).await
}

/// Same as [`make`] but takes an explicit base directory. Used by
/// tests to avoid touching the cwd.
///
/// Iterates [`plugin_order`], which is the topological order
/// published by `App::build()`'s phase 1.5 sort. Cross-plugin FKs
/// land in dependency order this way (a plugin's `CreateTable` for
/// the FK target runs before the dependent plugin's `CreateTable`).
/// Falls back to [`registered_plugins`] when no order has been
/// published (e.g. low-level tests that init the registry directly).
pub async fn make_in(dir: &Path) -> Result<Vec<PathBuf>, MigrateError> {
    let mut written: Vec<PathBuf> = Vec::new();

    for plugin in plugin_order() {
        let plugin_dir = dir.join(&plugin);

        // The previous snapshot is the `snapshot_after` of the highest-
        // numbered migration file (filenames are zero-padded so lexical
        // sort matches numeric order). An empty or missing directory
        // means "no prior state", the first-run case for this plugin.
        let existing = list_migration_files(&plugin_dir)?;
        let previous = match existing.last() {
            Some(path) => read_migration_file(path)?.snapshot_after,
            None => Snapshot::default(),
        };

        let current = Snapshot::current_for(&plugin);
        let operations = diff(&previous, &current)?;
        if operations.is_empty() {
            continue;
        }

        let seq = (existing.len() + 1) as u32;
        let suffix = suffix_for(&operations);
        let id = format!("{seq:04}_{suffix}");
        let filename = format!("{id}.json");

        let file = MigrationFile {
            id: id.clone(),
            plugin: plugin.clone(),
            depends_on: Vec::new(),
            operations,
            snapshot_after: current,
        };

        std::fs::create_dir_all(&plugin_dir)?;
        let path = plugin_dir.join(filename);
        let json = serde_json::to_string_pretty(&file)?;
        std::fs::write(&path, json)?;
        written.push(path);
    }

    if written.is_empty() {
        return Err(MigrateError::NoChanges);
    }
    Ok(written)
}

/// Apply every pending migration across every registered plugin's
/// `migrations/<plugin>/` directory to the ambient pool. Reads the
/// `umbra_migrations` tracking table to determine "pending"; each
/// migration runs in its own transaction along with its tracking-table
/// insert.
///
/// Returns the total number of migrations applied (zero if every
/// plugin's migrations were already in the tracking table).
///
/// This variant performs a drift check before executing any SQL. If
/// any migration is `AppliedButMissing` (in the DB but not on disk),
/// the call returns [`MigrateError::DriftDetected`] listing the
/// missing names. Pass `allow_drift = true` (via [`run_checked_in`])
/// to suppress the error and proceed anyway (with a warning printed to
/// stderr).
pub async fn run() -> Result<u64, MigrateError> {
    run_checked(false).await
}

/// Same as [`run`] but controls drift handling.
/// `allow_drift = true` corresponds to the `--allow-drift` CLI flag:
/// the command logs a warning and proceeds even if some applied
/// migrations are missing on disk.
pub async fn run_checked(allow_drift: bool) -> Result<u64, MigrateError> {
    run_checked_in(Path::new(MIGRATIONS_DIR), allow_drift).await
}

/// Same as [`run_checked`] but takes an explicit base directory.
pub async fn run_checked_in(dir: &Path, allow_drift: bool) -> Result<u64, MigrateError> {
    let mut total: u64 = 0;
    // Walk every registered DB. Drift-detection on the default pool
    // is the dominant flow; secondary pools currently use the same
    // tracking-table-vs-disk comparison but only against the
    // migration files whose ops actually targeted that DB. A future
    // pass can teach `detect_all_drift` to be alias-aware so drift
    // warnings name the offending pool — today it warns once per
    // checked DB if the issue is present in any.
    for alias in crate::db::registered_aliases() {
        match crate::db::pool_for_dispatched(&alias) {
            crate::db::DbPool::Sqlite(p) => {
                total += run_in_sqlite_checked(dir, p, allow_drift, &alias).await?
            }
            crate::db::DbPool::Postgres(p) => {
                total += run_in_postgres_checked(dir, p, allow_drift, &alias).await?
            }
        }
    }
    Ok(total)
}

/// Same as [`run`] but takes an explicit base directory. Used by
/// tests to avoid touching the cwd.
///
/// Iterates `registered_plugins()` in sorted-by-name order. M7 v1
/// accepts this as a limitation: cross-plugin FK ordering wants
/// topological order across plugins (the FK target's `CreateTable`
/// applies before the dependent plugin's `CreateTable`), but the
/// engine doesn't see `Plugin::dependencies()` from inside this
/// standalone function. M8 lifts the limitation via a registry that
/// remembers the toposorted order computed at `App::build()` time.
///
/// This legacy entry point does NOT perform drift checking so the
/// existing tests (which bypass drift by design) keep passing. New
/// callers should prefer [`run_checked_in`].
pub async fn run_in(dir: &Path) -> Result<u64, MigrateError> {
    let mut total: u64 = 0;
    // Walk every registered DB so each pool gets its own
    // `umbra_migrations` table and runs only the operations targeting
    // tables routed to it. Order is alphabetical for determinism;
    // the "default" pool is always present.
    for alias in crate::db::registered_aliases() {
        match crate::db::pool_for_dispatched(&alias) {
            crate::db::DbPool::Sqlite(p) => {
                total += run_in_sqlite_for_alias(dir, &alias, p).await?
            }
            crate::db::DbPool::Postgres(p) => {
                total += run_in_postgres_for_alias(dir, &alias, p).await?
            }
        }
    }
    Ok(total)
}

/// Predicate: does `op` target a table that lives on `alias`?
///
/// Routing rule: look up the table → alias mapping via
/// [`table_alias`]. Tables not owned by any registered model fall
/// through to `"default"` so the migration engine's own
/// `umbra_migrations` book-keeping stays in the main DB.
fn op_targets_alias(op: &Operation, alias: &str) -> bool {
    table_alias(op.table_name()) == alias
}

/// SQLite per-alias variant. Same shape as the legacy `run_in_sqlite`
/// but: filters ops to those routed to `alias`; skips files whose op
/// list contains nothing for this DB (so we don't stuff orphan
/// tracking rows into pools that didn't run any SQL).
async fn run_in_sqlite_for_alias(
    dir: &Path,
    alias: &str,
    pool: &sqlx::SqlitePool,
) -> Result<u64, MigrateError> {
    ensure_tracking_table_sqlite(pool).await?;
    let applied = applied_names_sqlite(pool).await?;

    let mut applied_count: u64 = 0;
    for plugin in plugin_order() {
        let plugin_dir = dir.join(&plugin);
        let paths = list_migration_files(&plugin_dir)?;

        for path in paths {
            let file = read_migration_file(&path)?;
            if applied.contains(&(file.plugin.clone(), file.id.clone())) {
                continue;
            }

            let ops_for_this_db: Vec<&Operation> = file
                .operations
                .iter()
                .filter(|op| op_targets_alias(op, alias))
                .collect();
            if ops_for_this_db.is_empty() {
                // File's content all targets some other DB. Don't
                // record it here — re-runs will re-evaluate cleanly
                // once the right DB picks it up. The tracking rows
                // per-DB stay accurate to "what actually ran here."
                continue;
            }

            let mut tx = pool.begin().await?;
            for op in &ops_for_this_db {
                for sql in render_operation(op) {
                    sqlx::query(&sql).execute(&mut *tx).await?;
                }
            }
            let snapshot_hash = file.snapshot_after.hash();
            let applied_at = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO umbra_migrations (plugin, name, applied_at, snapshot_hash) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&file.plugin)
            .bind(&file.id)
            .bind(&applied_at)
            .bind(&snapshot_hash)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            applied_count += 1;
        }
    }
    Ok(applied_count)
}

/// Postgres per-alias variant. Mirror of `run_in_sqlite_for_alias`.
async fn run_in_postgres_for_alias(
    dir: &Path,
    alias: &str,
    pool: &sqlx::PgPool,
) -> Result<u64, MigrateError> {
    ensure_tracking_table_postgres(pool).await?;
    let applied = applied_names_postgres(pool).await?;

    let mut applied_count: u64 = 0;
    for plugin in plugin_order() {
        let plugin_dir = dir.join(&plugin);
        let paths = list_migration_files(&plugin_dir)?;

        for path in paths {
            let file = read_migration_file(&path)?;
            if applied.contains(&(file.plugin.clone(), file.id.clone())) {
                continue;
            }

            let ops_for_this_db: Vec<&Operation> = file
                .operations
                .iter()
                .filter(|op| op_targets_alias(op, alias))
                .collect();
            if ops_for_this_db.is_empty() {
                continue;
            }

            let mut tx = pool.begin().await?;
            for op in &ops_for_this_db {
                for sql in render_operation(op) {
                    sqlx::query(&sql).execute(&mut *tx).await?;
                }
            }
            let snapshot_hash = file.snapshot_after.hash();
            let applied_at = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO umbra_migrations (plugin, name, applied_at, snapshot_hash) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(&file.plugin)
            .bind(&file.id)
            .bind(&applied_at)
            .bind(&snapshot_hash)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            applied_count += 1;
        }
    }
    Ok(applied_count)
}

/// SQLite drift-checking path for `run_checked_in`.
///
/// Reads the applied set, runs `detect_all_drift`, and either errors
/// (if `allow_drift = false` and critical drift is found) or logs a
/// warning and proceeds (if `allow_drift = true`). Then delegates to
/// `run_in_sqlite` for the actual apply loop.
async fn run_in_sqlite_checked(
    dir: &Path,
    pool: &sqlx::SqlitePool,
    allow_drift: bool,
    alias: &str,
) -> Result<u64, MigrateError> {
    ensure_tracking_table_sqlite(pool).await?;
    let applied = applied_names_sqlite(pool).await?;
    let report = detect_all_drift(&applied, dir)?;

    if report.has_critical_drift() {
        if allow_drift {
            let missing = report.missing_on_disk();
            for entry in &missing {
                eprintln!(
                    "warning: umbra migrate --allow-drift: migration {}/{} is recorded in \
                     the tracking table but the file is missing from disk; proceeding.",
                    entry.plugin, entry.name
                );
            }
        } else {
            let missing: Vec<(String, String)> = report
                .missing_on_disk()
                .iter()
                .map(|e| (e.plugin.clone(), e.name.clone()))
                .collect();
            return Err(MigrateError::DriftDetected { missing });
        }
    }

    // Emit warnings for out-of-order files.
    for entry in report
        .entries
        .iter()
        .filter(|e| e.status == MigrationStatus::OutOfOrder)
    {
        eprintln!(
            "warning: umbra migrate: migration {}/{} is on disk but appears before the \
             last applied migration for this plugin; it looks like a file was restored \
             after a teammate already applied later ones.",
            entry.plugin, entry.name
        );
    }

    run_in_sqlite_for_alias(dir, alias, pool).await
}

/// Postgres drift-checking path for `run_checked_in`. Same logic as
/// `run_in_sqlite_checked` but uses the Postgres applied-set reader.
async fn run_in_postgres_checked(
    dir: &Path,
    pool: &sqlx::PgPool,
    allow_drift: bool,
    alias: &str,
) -> Result<u64, MigrateError> {
    ensure_tracking_table_postgres(pool).await?;
    let applied = applied_names_postgres(pool).await?;
    let report = detect_all_drift(&applied, dir)?;

    if report.has_critical_drift() {
        if allow_drift {
            let missing = report.missing_on_disk();
            for entry in &missing {
                eprintln!(
                    "warning: umbra migrate --allow-drift: migration {}/{} is recorded in \
                     the tracking table but the file is missing from disk; proceeding.",
                    entry.plugin, entry.name
                );
            }
        } else {
            let missing: Vec<(String, String)> = report
                .missing_on_disk()
                .iter()
                .map(|e| (e.plugin.clone(), e.name.clone()))
                .collect();
            return Err(MigrateError::DriftDetected { missing });
        }
    }

    for entry in report
        .entries
        .iter()
        .filter(|e| e.status == MigrationStatus::OutOfOrder)
    {
        eprintln!(
            "warning: umbra migrate: migration {}/{} is on disk but appears before the \
             last applied migration for this plugin; it looks like a file was restored \
             after a teammate already applied later ones.",
            entry.plugin, entry.name
        );
    }

    run_in_postgres_for_alias(dir, alias, pool).await
}

/// Record a migration as applied in the `umbra_migrations` tracking
/// table without running its operations. The "mark as applied" path
/// `inspectdb --mark-applied` uses to register the introspected
/// `0001_initial` against an already-populated database. Idempotent:
/// if the `(plugin, name)` row already exists, the call is a no-op.
pub async fn record_applied(
    plugin: &str,
    name: &str,
    snapshot_hash: &str,
) -> Result<(), MigrateError> {
    let applied_at = chrono::Utc::now().to_rfc3339();
    match crate::db::pool_dispatched() {
        crate::db::DbPool::Sqlite(pool) => {
            ensure_tracking_table_sqlite(pool).await?;
            sqlx::query(
                "INSERT OR IGNORE INTO umbra_migrations \
                 (plugin, name, applied_at, snapshot_hash) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(plugin)
            .bind(name)
            .bind(&applied_at)
            .bind(snapshot_hash)
            .execute(pool)
            .await?;
        }
        crate::db::DbPool::Postgres(pool) => {
            ensure_tracking_table_postgres(pool).await?;
            sqlx::query(
                "INSERT INTO umbra_migrations \
                 (plugin, name, applied_at, snapshot_hash) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (plugin, name) DO NOTHING",
            )
            .bind(plugin)
            .bind(name)
            .bind(&applied_at)
            .bind(snapshot_hash)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

// =========================================================================
// Drift detection — gap 24.
// =========================================================================

/// Compute the drift report for a single plugin directory. Compares the
/// set of `(plugin, name)` pairs recorded in the tracking table against
/// the migration files present on disk and classifies each into one of
/// the four [`MigrationStatus`] states.
///
/// `applied` is the full set of `(plugin, name)` tuples already read
/// from the tracking table (shared across plugins to avoid extra DB
/// round-trips). `plugin_dir` is the on-disk directory for this plugin;
/// an absent directory is treated the same as an empty one.
///
/// # Classification
///
/// - File present + in DB → `Applied`
/// - File absent + in DB → `AppliedButMissing`
/// - File present + not in DB + seq ≤ max_applied_seq → `OutOfOrder`
/// - File present + not in DB + seq > max_applied_seq → `Pending`
///
/// The sequence number is the numeric prefix of the migration name
/// (e.g. `0001` in `0001_create_post`). Absence of any applied
/// migration for this plugin means `max_applied_seq = 0`.
pub fn detect_drift(
    plugin: &str,
    applied: &std::collections::HashSet<(String, String)>,
    plugin_dir: &Path,
) -> Result<Vec<MigrationEntry>, MigrateError> {
    // Collect on-disk migration names (the id, not the full path).
    let paths = list_migration_files(plugin_dir)?;
    let mut on_disk: Vec<String> = Vec::new();
    for path in &paths {
        let file = read_migration_file(path)?;
        on_disk.push(file.id.clone());
    }

    // Pull every tracking-table entry for this plugin.
    let plugin_applied: Vec<&str> = applied
        .iter()
        .filter(|(p, _)| p == plugin)
        .map(|(_, n)| n.as_str())
        .collect();

    // Highest sequence number among applied migrations for this plugin.
    let max_applied_seq: u32 = plugin_applied
        .iter()
        .filter_map(|name| name.split('_').next()?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);

    let on_disk_set: std::collections::HashSet<&str> = on_disk.iter().map(|s| s.as_str()).collect();

    let mut entries: Vec<MigrationEntry> = Vec::new();

    // Walk on-disk files in order.
    for name in &on_disk {
        let key = (plugin.to_string(), name.clone());
        let status = if applied.contains(&key) {
            MigrationStatus::Applied
        } else {
            // Determine this migration's sequence number.
            let seq: u32 = name
                .split('_')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if seq <= max_applied_seq && max_applied_seq > 0 {
                MigrationStatus::OutOfOrder
            } else {
                MigrationStatus::Pending
            }
        };
        entries.push(MigrationEntry {
            plugin: plugin.to_string(),
            name: name.clone(),
            status,
        });
    }

    // Walk applied entries not present on disk.
    for name in &plugin_applied {
        if !on_disk_set.contains(*name) {
            entries.push(MigrationEntry {
                plugin: plugin.to_string(),
                name: (*name).to_string(),
                status: MigrationStatus::AppliedButMissing,
            });
        }
    }

    // Sort: applied-but-missing entries bubble after their expected
    // position is not determinable; sort all entries by name for a
    // deterministic order. In practice, applied-but-missing names
    // are still prefixed with the numeric sequence so lexical sort
    // yields the right display order.
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(entries)
}

/// Detect drift across every registered plugin and return a combined
/// [`DriftReport`]. Called by `run_in_checked` before executing SQL
/// and by `show_in` when displaying the four-state list.
///
/// `applied` is already fetched from the DB; `dir` is the migrations
/// root directory.
pub fn detect_all_drift(
    applied: &std::collections::HashSet<(String, String)>,
    dir: &Path,
) -> Result<DriftReport, MigrateError> {
    let mut all_entries: Vec<MigrationEntry> = Vec::new();

    // Also surface any tracking-table entries whose plugin directory
    // doesn't appear in the registered-plugins list — a plugin was
    // removed entirely but its DB rows remain.
    let mut seen_plugins: std::collections::HashSet<String> = std::collections::HashSet::new();

    for plugin in plugin_order() {
        seen_plugins.insert(plugin.clone());
        let plugin_dir = dir.join(&plugin);
        let entries = detect_drift(&plugin, applied, &plugin_dir)?;
        all_entries.extend(entries);
    }

    // Any applied entries whose plugin is not in the registered set at
    // all — treat them as AppliedButMissing (the whole plugin is gone).
    for (plugin, name) in applied {
        if !seen_plugins.contains(plugin.as_str()) {
            all_entries.push(MigrationEntry {
                plugin: plugin.clone(),
                name: name.clone(),
                status: MigrationStatus::AppliedButMissing,
            });
        }
    }

    Ok(DriftReport {
        entries: all_entries,
    })
}

/// Record a migration as applied in the tracking table WITHOUT running
/// its SQL operations. The `--fake` recovery path: the schema already
/// exists (e.g. the migration was run outside umbra, or the DB was
/// bootstrapped from a dump) and the operator wants to bring the
/// tracking table into sync without re-executing the DDL.
///
/// Idempotent: if `(plugin, name)` is already in the table the call
/// is a no-op (same behaviour as `record_applied`).
///
/// The snapshot hash is derived from the migration file on disk.
/// Returns `MigrateError::Io` if the file can't be found (the caller
/// should verify the name before calling this).
pub async fn fake_apply(plugin: &str, name: &str) -> Result<(), MigrateError> {
    fake_apply_in(plugin, name, Path::new(MIGRATIONS_DIR)).await
}

/// Same as [`fake_apply`] but takes an explicit migrations base dir.
/// Used by tests and by the CLI when `--migrations-dir` is passed.
pub async fn fake_apply_in(plugin: &str, name: &str, dir: &Path) -> Result<(), MigrateError> {
    let path = dir.join(plugin).join(format!("{name}.json"));
    let file = read_migration_file(&path)?;
    let snapshot_hash = file.snapshot_after.hash();
    record_applied(plugin, name, &snapshot_hash).await
}

/// For every registered plugin's first migration (`0001_*`), check
/// whether the tables that migration would create already exist in the
/// database. If they do, fake-apply the migration (mark it applied
/// without running its SQL).
///
/// This is Django's `--fake-initial` path: the operator has a database
/// bootstrapped outside umbra (a dump restore, a manual `CREATE TABLE`,
/// or a previous schema manager) and wants to bring the tracking table
/// into sync so subsequent `migrate` calls apply only the genuine
/// deltas.
///
/// Returns the number of plugins whose `0001_*` migration was
/// fake-applied. Zero means either no `0001_*` file exists or the
/// target tables were absent (in which case normal `migrate` should be
/// run to create them).
pub async fn fake_initial() -> Result<u64, MigrateError> {
    fake_initial_in(Path::new(MIGRATIONS_DIR)).await
}

/// Same as [`fake_initial`] but takes an explicit migrations base dir.
pub async fn fake_initial_in(dir: &Path) -> Result<u64, MigrateError> {
    match crate::db::pool_dispatched() {
        crate::db::DbPool::Sqlite(pool) => fake_initial_sqlite(dir, pool).await,
        crate::db::DbPool::Postgres(pool) => fake_initial_postgres(dir, pool).await,
    }
}

/// SQLite path for [`fake_initial_in`].
async fn fake_initial_sqlite(dir: &Path, pool: &sqlx::SqlitePool) -> Result<u64, MigrateError> {
    ensure_tracking_table_sqlite(pool).await?;
    let applied = applied_names_sqlite(pool).await?;
    let mut count: u64 = 0;

    for plugin in plugin_order() {
        let plugin_dir = dir.join(&plugin);
        let paths = list_migration_files(&plugin_dir)?;

        // Find the first migration file (lowest sequence number).
        let first = paths.first();
        let first = match first {
            Some(p) => p,
            None => continue,
        };
        let file = read_migration_file(first)?;

        // Skip if already applied.
        if applied.contains(&(file.plugin.clone(), file.id.clone())) {
            continue;
        }

        // Check whether the tables the first migration would create
        // already exist in the database.
        let tables_to_create: Vec<&str> = file
            .operations
            .iter()
            .filter_map(|op| match op {
                Operation::CreateTable { table, .. } => Some(table.as_str()),
                _ => None,
            })
            .collect();

        if tables_to_create.is_empty() {
            continue;
        }

        // All tables present → fake-apply.
        let mut all_present = true;
        for table in &tables_to_create {
            let exists: Option<(String,)> =
                sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
                    .bind(*table)
                    .fetch_optional(pool)
                    .await?;
            if exists.is_none() {
                all_present = false;
                break;
            }
        }

        if all_present {
            let snapshot_hash = file.snapshot_after.hash();
            let applied_at = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT OR IGNORE INTO umbra_migrations \
                 (plugin, name, applied_at, snapshot_hash) VALUES (?, ?, ?, ?)",
            )
            .bind(&file.plugin)
            .bind(&file.id)
            .bind(&applied_at)
            .bind(&snapshot_hash)
            .execute(pool)
            .await?;
            count += 1;
        }
    }

    Ok(count)
}

/// Postgres path for [`fake_initial_in`].
async fn fake_initial_postgres(dir: &Path, pool: &sqlx::PgPool) -> Result<u64, MigrateError> {
    ensure_tracking_table_postgres(pool).await?;
    let applied = applied_names_postgres(pool).await?;
    let mut count: u64 = 0;

    for plugin in plugin_order() {
        let plugin_dir = dir.join(&plugin);
        let paths = list_migration_files(&plugin_dir)?;

        let first = paths.first();
        let first = match first {
            Some(p) => p,
            None => continue,
        };
        let file = read_migration_file(first)?;

        if applied.contains(&(file.plugin.clone(), file.id.clone())) {
            continue;
        }

        let tables_to_create: Vec<&str> = file
            .operations
            .iter()
            .filter_map(|op| match op {
                Operation::CreateTable { table, .. } => Some(table.as_str()),
                _ => None,
            })
            .collect();

        if tables_to_create.is_empty() {
            continue;
        }

        let mut all_present = true;
        for table in &tables_to_create {
            let exists: Option<(String,)> = sqlx::query_as(
                "SELECT table_name FROM information_schema.tables \
                 WHERE table_schema = 'public' AND table_name = $1",
            )
            .bind(*table)
            .fetch_optional(pool)
            .await?;
            if exists.is_none() {
                all_present = false;
                break;
            }
        }

        if all_present {
            let snapshot_hash = file.snapshot_after.hash();
            let applied_at = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO umbra_migrations \
                 (plugin, name, applied_at, snapshot_hash) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (plugin, name) DO NOTHING",
            )
            .bind(&file.plugin)
            .bind(&file.id)
            .bind(&applied_at)
            .bind(&snapshot_hash)
            .execute(pool)
            .await?;
            count += 1;
        }
    }

    Ok(count)
}

/// Print the per-migration state, applied or pending. Output goes to
/// stdout; the return value is the count of pending migrations so a
/// CLI can `exit(n)` on need.
pub async fn show() -> Result<u64, MigrateError> {
    show_in(Path::new(MIGRATIONS_DIR)).await
}

/// Same as [`show`] but takes an explicit base directory. Walks every
/// registered plugin in sorted-by-name order, printing one section per
/// plugin that owns at least one migration file; empty plugins are
/// skipped silently rather than emitting a bare header.
///
/// Four-state output (gap 24):
///
/// - `[X]` applied and file present on disk (normal)
/// - `[ ]` pending (on disk, not yet applied, sequence after last applied)
/// - `[!]` applied but missing on disk (drift — tracking table ahead of VCS)
/// - `[?]` on disk but out of order (sequence before last applied, not in DB)
pub async fn show_in(dir: &Path) -> Result<u64, MigrateError> {
    let applied = match crate::db::pool_dispatched() {
        crate::db::DbPool::Sqlite(pool) => {
            ensure_tracking_table_sqlite(pool).await?;
            applied_names_sqlite(pool).await?
        }
        crate::db::DbPool::Postgres(pool) => {
            ensure_tracking_table_postgres(pool).await?;
            applied_names_postgres(pool).await?
        }
    };

    let report = detect_all_drift(&applied, dir)?;

    // Group by plugin for display.
    let mut by_plugin: std::collections::BTreeMap<&str, Vec<&MigrationEntry>> =
        std::collections::BTreeMap::new();
    for entry in &report.entries {
        by_plugin
            .entry(entry.plugin.as_str())
            .or_default()
            .push(entry);
    }

    let mut pending: u64 = 0;
    for (plugin, entries) in &by_plugin {
        if entries.is_empty() {
            continue;
        }
        println!("# plugin: {plugin}");
        for entry in entries {
            let marker = match entry.status {
                MigrationStatus::Applied => "[X]",
                MigrationStatus::Pending => {
                    pending += 1;
                    "[ ]"
                }
                MigrationStatus::AppliedButMissing => "[!]",
                MigrationStatus::OutOfOrder => "[?]",
            };
            println!("{marker} {}/{}", entry.plugin, entry.name);
        }
    }
    Ok(pending)
}

/// Safety classification for a single pending migration operation.
///
/// Feature #65 (blue-green / zero-downtime). The `checkmigrations`
/// command walks every pending operation and tags it so an operator
/// deploying without a maintenance window can tell which changes are safe
/// under a rolling deploy (old and new code serving traffic at once) and
/// which need the expand-contract dance. This is advisory triage — the
/// engine still *applies* every op exactly as written; nothing here gates
/// `migrate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpSafety {
    /// Additive and backward-compatible — safe while old code still runs.
    Safe,
    /// Applies cleanly but can break still-running old code, lock a large
    /// table, or fail against unexpected production data. Review first.
    Warning(String),
    /// Destroys data or is irreversible; old code referencing the dropped
    /// surface errors immediately.
    Unsafe(String),
}

impl OpSafety {
    /// The advisory reason for a `Warning` / `Unsafe`; empty for `Safe`.
    pub fn reason(&self) -> &str {
        match self {
            OpSafety::Safe => "",
            OpSafety::Warning(r) | OpSafety::Unsafe(r) => r,
        }
    }

    /// True for the destructive / irreversible tier only.
    pub fn is_unsafe(&self) -> bool {
        matches!(self, OpSafety::Unsafe(_))
    }

    /// True for the review-before-deploy tier only.
    pub fn is_warning(&self) -> bool {
        matches!(self, OpSafety::Warning(_))
    }
}

/// One pending operation tagged with its [`OpSafety`] and the migration
/// that introduced it. The unit of output for `checkmigrations`.
#[derive(Debug, Clone)]
pub struct ClassifiedOp {
    pub plugin: String,
    pub migration: String,
    pub op: Operation,
    pub safety: OpSafety,
}

/// Classify one operation for zero-downtime safety. Pure — no DB access,
/// no file reads — so it is trivially unit-testable and reused by both
/// the CLI report and any plugin that wants to gate its own deploys.
pub fn classify_operation(op: &Operation) -> OpSafety {
    match op {
        // Brand-new tables touch no existing rows and no old code reads
        // them yet.
        Operation::CreateTable { .. } | Operation::CreateM2MTable { .. } => OpSafety::Safe,

        // Adding a column is additive — unless it's NOT NULL with no
        // default, in which case old code inserting a row without the
        // column fails. (The engine refuses such an add against a
        // populated SQLite table at apply time; this surfaces the same
        // hazard *before* the operator runs it, and for Postgres too.)
        Operation::AddColumn { table, column } => {
            if !column.nullable && column.default.is_empty() {
                OpSafety::Warning(format!(
                    "adds NOT NULL column `{}.{}` with no default — old code inserting without it will fail. Add it nullable (or with a default), backfill, then tighten",
                    table, column.name
                ))
            } else {
                OpSafety::Safe
            }
        }

        // Destructive / irreversible: data loss the moment it runs.
        Operation::DropTable { table } => OpSafety::Unsafe(format!(
            "drops table `{table}` and every row in it — irreversible, and old code still reading it breaks. Stop using it, deploy, then drop in a later migration"
        )),
        Operation::DropM2MTable { junction_table } => OpSafety::Unsafe(format!(
            "drops join table `{junction_table}` and every row in it — irreversible"
        )),
        Operation::DropColumn { table, column } => OpSafety::Unsafe(format!(
            "drops column `{table}.{column}` and its data — old code reading it breaks. Expand-contract: stop writing it, deploy, then drop"
        )),

        // Renames apply atomically in the DB but NOT atomically with a
        // code deploy: between the migration and the rollout, one of the
        // two code versions references the missing name.
        Operation::RenameTable { from, to } => OpSafety::Warning(format!(
            "renames table `{from}` → `{to}` — not atomic with a code deploy; old code references `{from}`. Expand-contract: add `{to}`, dual-write, switch, then drop `{from}`"
        )),
        Operation::RenameColumn {
            table, from, to, ..
        } => OpSafety::Warning(format!(
            "renames column `{table}.{from}` → `{to}` — old code references `{from}`. Expand-contract: add `{to}`, backfill, switch reads, then drop `{from}`"
        )),

        // An alter can rewrite a column (table lock on large data) and a
        // nullable→NOT NULL tightening fails on existing NULLs.
        Operation::AlterColumn { table, column, .. } => OpSafety::Warning(format!(
            "alters column `{table}.{column}` — a type change rewrites the column (locks the table on large data) and a NOT NULL tightening fails on existing NULLs; verify against production data first"
        )),
    }
}

/// Classify every operation across all pending migrations against the
/// ambient pool. Reads the same applied-set + on-disk diff that
/// `migrate` / `showmigrations` use, then loads each pending migration
/// file and classifies its operations in order. Powers `checkmigrations`.
pub async fn check_pending_safety() -> Result<Vec<ClassifiedOp>, MigrateError> {
    check_pending_safety_in(Path::new(MIGRATIONS_DIR)).await
}

/// [`check_pending_safety`] against an explicit migrations directory.
/// The seam tests use to point at a fixture tree.
pub async fn check_pending_safety_in(dir: &Path) -> Result<Vec<ClassifiedOp>, MigrateError> {
    let applied = match crate::db::pool_dispatched() {
        crate::db::DbPool::Sqlite(pool) => {
            ensure_tracking_table_sqlite(pool).await?;
            applied_names_sqlite(pool).await?
        }
        crate::db::DbPool::Postgres(pool) => {
            ensure_tracking_table_postgres(pool).await?;
            applied_names_postgres(pool).await?
        }
    };

    let report = detect_all_drift(&applied, dir)?;

    let mut out: Vec<ClassifiedOp> = Vec::new();
    for entry in &report.entries {
        if entry.status != MigrationStatus::Pending {
            continue;
        }
        let path = dir.join(&entry.plugin).join(format!("{}.json", entry.name));
        let file = read_migration_file(&path)?;
        for op in &file.operations {
            out.push(ClassifiedOp {
                plugin: entry.plugin.clone(),
                migration: entry.name.clone(),
                op: op.clone(),
                safety: classify_operation(op),
            });
        }
    }
    Ok(out)
}

// =========================================================================
// Internal helpers. Crate-private; the public surface above is the only
// thing the rest of umbra calls into.
// =========================================================================

/// Return every `*.json` migration file in `plugin_dir`, sorted by
/// filename (lexical sort matches numeric order because the prefix is
/// zero-padded). Returns an empty vec if the directory is missing.
fn list_migration_files(plugin_dir: &Path) -> Result<Vec<PathBuf>, MigrateError> {
    if !plugin_dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(plugin_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Read and parse one migration file.
fn read_migration_file(path: &Path) -> Result<MigrationFile, MigrateError> {
    let text = std::fs::read_to_string(path)?;
    let file: MigrationFile = serde_json::from_str(&text)?;
    Ok(file)
}

/// Diff the previous snapshot against the current one and produce the
/// ordered operation list.
///
/// Emits `CreateTable` / `DropTable` for whole-model changes (M5 v1),
/// and `AddColumn` / `DropColumn` for column-level changes on a model
/// that appears in both snapshots (M8 v1). A column whose name stays
/// the same but whose type or nullable flag changed surfaces as
/// [`MigrateError::UnsafeAlter`]: SQLite can't ALTER COLUMN TYPE in
/// place, and a nullable flip on a populated table is destructive.
///
/// Gap 30 adds two-pass rename detection. `Model::NAME` (the Rust struct
/// name) is the stable identity key across snapshots; the SQL table name
/// in `Model::TABLE` may change (e.g. via the `#[umbra(plugin = "...")]`
/// opt-in). The two passes are:
///
/// - **First pass — struct-name match.** If a model present in `current`
///   but absent from `previous` (by `Model::NAME`) has the same NAME as
///   a model present in `previous` but absent from `current`, the table
///   name changed: emit `RenameTable { from, to }` instead of DropTable +
///   CreateTable. A stdout message names the rename so the developer can
///   audit `makemigrations` output.
/// - **Second pass — column-shape match.** Among unpaired drops and
///   creates, if a drop candidate and a create candidate have bit-identical
///   column shapes (same column names, types, nullable, fk_target), emit
///   `RenameTable` and log a warning so the developer can verify the
///   intent. Struct names differ; the shape heuristic fills in for cases
///   like a wholesale model rename (Foo → Bar, identical fields).
/// - **No-match.** Drop and create as today.
///
/// `pub` (not `pub(crate)`) so integration tests can drive the diff
/// directly with hand-built snapshots. Spec 06 calls the diff the
/// engine's contract; exposing it lets the tests pin every scenario
/// without laundering snapshots through the process-wide registry.
pub fn diff(previous: &Snapshot, current: &Snapshot) -> Result<Vec<Operation>, MigrateError> {
    use std::collections::{BTreeMap, HashSet};

    let prev_by_name: BTreeMap<&str, &ModelMeta> = previous
        .models
        .iter()
        .map(|m| (m.name.as_str(), m))
        .collect();
    let curr_by_name: BTreeMap<&str, &ModelMeta> = current
        .models
        .iter()
        .map(|m| (m.name.as_str(), m))
        .collect();

    let mut ops: Vec<Operation> = Vec::new();

    // ---- Pass 0: Walk models present in both snapshots (same NAME). ----
    // Same-name models with a different table produce a first-pass rename.
    // Same-name models with identical table+columns produce nothing.
    // Same-name models with column changes produce column-level ops.

    let mut drop_candidates: Vec<&ModelMeta> = Vec::new(); // in prev, not curr
    let mut create_candidates: Vec<&ModelMeta> = Vec::new(); // in curr, not prev

    // Creates and column-level diffs, in deterministic name order.
    for (name, curr) in &curr_by_name {
        match prev_by_name.get(name) {
            None => {
                // In current but not previous — might be a create or a first-pass rename.
                create_candidates.push(curr);
            }
            Some(prev) if prev.table != curr.table => {
                // Same struct name, different table name → first-pass rename.
                println!(
                    "umbra makemigrations: rename detected (struct-name match): \
                     table `{}` → `{}`",
                    prev.table, curr.table
                );
                ops.push(Operation::RenameTable {
                    from: prev.table.clone(),
                    to: curr.table.clone(),
                });
                // After the rename the columns might also have changed; diff them.
                let col_ops = diff_columns(name, prev, curr)?;
                ops.extend(col_ops);
            }
            Some(prev) if prev == curr => {}
            Some(prev) => {
                ops.extend(diff_columns(name, prev, curr)?);
            }
        }
    }

    // Drops — models in prev but not curr (by NAME).
    for (name, prev) in &prev_by_name {
        if !curr_by_name.contains_key(name) {
            drop_candidates.push(prev);
        }
    }

    // ---- Pass 1: Column-shape heuristic for unpaired drops + creates. ----
    // A sorted, canonical serialisation of (name, ty, nullable, fk_target)
    // is the "shape" fingerprint. Bit-identical shapes → likely a model
    // rename where the struct name also changed.

    let mut paired_drop_tables: HashSet<&str> = HashSet::new();
    let mut paired_create_tables: HashSet<&str> = HashSet::new();

    for create in &create_candidates {
        let create_shape = column_shape(&create.fields);
        for drop in &drop_candidates {
            if paired_drop_tables.contains(drop.table.as_str()) {
                continue;
            }
            let drop_shape = column_shape(&drop.fields);
            if create_shape == drop_shape {
                eprintln!(
                    "umbra makemigrations: rename detected (column-shape match): \
                     `{}` → `{}` — please verify this is a rename and not a coincidental \
                     column-shape match between two unrelated models",
                    drop.table, create.table
                );
                ops.push(Operation::RenameTable {
                    from: drop.table.clone(),
                    to: create.table.clone(),
                });
                paired_drop_tables.insert(drop.table.as_str());
                paired_create_tables.insert(create.table.as_str());
                break;
            }
        }
    }

    // ---- Pass 2: Emit plain CreateTable for unpaired creates. ----
    //
    // Sort the create list topologically by FK dependency so that a
    // table referenced by another table in this batch is created first.
    // Without this, Postgres rejects the second CreateTable with
    // `relation "<target>" does not exist`. (SQLite tolerates the wrong
    // order when `foreign_keys=OFF`, the historical default; once
    // we turned foreign_keys ON in connect_sqlite, SQLite agrees with
    // Postgres on the order requirement.)
    //
    // Kahn's algorithm on (table → set of FK-target tables that are
    // ALSO in the create batch). Self-references and FK targets outside
    // the batch are skipped (they're either harmless or already exist
    // by the time this migration runs).
    let creates: Vec<&&ModelMeta> = create_candidates
        .iter()
        .filter(|c| !paired_create_tables.contains(c.table.as_str()))
        .collect();
    let batch_tables: HashSet<&str> = creates.iter().map(|c| c.table.as_str()).collect();
    let mut deps: BTreeMap<&str, HashSet<&str>> = BTreeMap::new();
    for create in &creates {
        let mut in_batch: HashSet<&str> = HashSet::new();
        for col in &create.fields {
            if let Some(target) = col.fk_target.as_deref()
                && target != create.table.as_str()
                && batch_tables.contains(target)
            {
                in_batch.insert(target);
            }
        }
        deps.insert(create.table.as_str(), in_batch);
    }
    // Kahn: repeatedly pop tables with no remaining deps in the batch.
    // BTreeMap iteration is alphabetical → ties break alphabetically,
    // keeping the output stable.
    let mut ordered: Vec<&&ModelMeta> = Vec::with_capacity(creates.len());
    while !deps.is_empty() {
        let ready: Vec<&str> = deps
            .iter()
            .filter(|(_, d)| d.is_empty())
            .map(|(t, _)| *t)
            .collect();
        if ready.is_empty() {
            // Cyclic FK or other unresolvable dep — fall through to
            // the original order rather than dropping models. A cycle
            // here means the user's schema can't be created with
            // plain CreateTable anyway (Postgres needs deferrable
            // constraints), so we surface the user-visible error at
            // apply time instead of silently looping.
            for create in &creates {
                if deps.contains_key(create.table.as_str()) {
                    ordered.push(create);
                }
            }
            break;
        }
        for t in &ready {
            if let Some(create) = creates.iter().find(|c| c.table.as_str() == *t) {
                ordered.push(create);
            }
            deps.remove(t);
        }
        for (_, set) in deps.iter_mut() {
            for t in &ready {
                set.remove(t);
            }
        }
    }
    for create in ordered {
        ops.push(Operation::CreateTable {
            table: create.table.clone(),
            columns: create.fields.clone(),
            unique_together: create.unique_together.clone(),
            indexes: create.indexes.clone(),
        });
    }

    // ---- Pass 3: Emit plain DropTable for unpaired drops. ----
    for drop in &drop_candidates {
        if !paired_drop_tables.contains(drop.table.as_str()) {
            ops.push(Operation::DropTable {
                table: drop.table.clone(),
            });
        }
    }

    // ---- Pass 4: Diff M2M relations. Closes the remaining BUG-16 gap. ----
    //
    // Treat each (parent_table, field_name) pair as a junction-table
    // identity. Compare the flattened set across snapshots and emit
    // CreateM2MTable / DropM2MTable per delta. Renames of the parent
    // model trip a Drop + Create on the junction — same semantics as
    // Django, and the rename-tracking we'd need to do better is
    // ambitious enough to defer.
    let prev_m2m = collect_m2m_pairs(previous);
    let curr_m2m = collect_m2m_pairs(current);
    for (key, spec) in &curr_m2m {
        if prev_m2m.contains_key(key) {
            continue;
        }
        // New M2M field on an existing or new model. Resolve the
        // target's PK column from the current snapshot.
        match build_create_m2m_op(spec, current) {
            Ok(op) => ops.push(op),
            Err(e) => return Err(e),
        }
    }
    for (key, spec) in &prev_m2m {
        if curr_m2m.contains_key(key) {
            continue;
        }
        // M2M field removed (or its parent was dropped). The junction
        // table goes away.
        ops.push(Operation::DropM2MTable {
            junction_table: spec.junction_table.clone(),
        });
    }

    Ok(ops)
}

/// A flat-resolved M2M descriptor used by [`diff`] to compare snapshots.
/// Owns its strings so it can be keyed in a map without lifetime
/// gymnastics.
#[derive(Debug, Clone)]
struct M2MPair {
    parent_table: String,
    parent_pk: String,
    field_name: String,
    target_table: String,
    junction_table: String,
}

/// Walk a snapshot and produce one [`M2MPair`] per declared M2M field.
/// Keyed on `(parent_table, field_name)` since that uniquely identifies
/// a junction table — two models can't share the same parent_table, and
/// one model can't declare two M2M fields with the same name.
fn collect_m2m_pairs(snap: &Snapshot) -> std::collections::BTreeMap<(String, String), M2MPair> {
    let mut out = std::collections::BTreeMap::new();
    for model in &snap.models {
        let parent_pk = model
            .fields
            .iter()
            .find(|c| c.primary_key)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "id".to_string());
        for rel in &model.m2m_relations {
            let key = (model.table.clone(), rel.field_name.clone());
            out.insert(
                key,
                M2MPair {
                    parent_table: model.table.clone(),
                    parent_pk: parent_pk.clone(),
                    field_name: rel.field_name.clone(),
                    target_table: rel.target_table.clone(),
                    junction_table: format!("{}_{}", model.table, rel.field_name),
                },
            );
        }
    }
    out
}

/// Lift an [`M2MPair`] into a fully-specified [`Operation::CreateM2MTable`].
/// The target table's PK column name is resolved from `current` (the
/// snapshot the diff is computing toward) — without it the DDL would
/// reference a column the child table doesn't have.
fn build_create_m2m_op(spec: &M2MPair, current: &Snapshot) -> Result<Operation, MigrateError> {
    let target = current
        .models
        .iter()
        .find(|m| m.table == spec.target_table)
        .ok_or_else(|| {
            MigrateError::UnsupportedChange(format!(
                "M2M `{}.{}` targets table `{}` which is not registered \
                 in the current snapshot — register the target model with \
                 `AppBuilder::model::<{}>()` before its parent.",
                spec.parent_table, spec.field_name, spec.target_table, spec.target_table,
            ))
        })?;
    let child_pk_col = target
        .fields
        .iter()
        .find(|c| c.primary_key)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "id".to_string());
    let child_ty = target
        .fields
        .iter()
        .find(|c| c.primary_key)
        .map(|c| c.ty)
        .unwrap_or(crate::orm::SqlType::BigInt);
    let parent_model = current
        .models
        .iter()
        .find(|m| m.table == spec.parent_table)
        .expect("parent model exists in snapshot — collect_m2m_pairs iterated it");
    let parent_ty = parent_model
        .fields
        .iter()
        .find(|c| c.primary_key)
        .map(|c| c.ty)
        .unwrap_or(crate::orm::SqlType::BigInt);
    Ok(Operation::CreateM2MTable {
        junction_table: spec.junction_table.clone(),
        parent_table: spec.parent_table.clone(),
        parent_col: spec.parent_pk.clone(),
        child_table: spec.target_table.clone(),
        child_col: child_pk_col,
        parent_ty,
        child_ty,
    })
}

/// Compute a canonical, sorted column-shape fingerprint for rename
/// heuristic detection in `diff`. Two models whose column fingerprints
/// are identical are candidates for a rename (second-pass detection).
///
/// The fingerprint is a sorted `Vec` of `(name, ty, nullable, fk_target)`
/// tuples. Sorting by name ensures the fingerprint is independent of
/// declaration order.
fn column_shape(fields: &[Column]) -> Vec<(String, SqlType, bool, Option<String>)> {
    let mut shape: Vec<(String, SqlType, bool, Option<String>)> = fields
        .iter()
        .map(|c| (c.name.clone(), c.ty, c.nullable, c.fk_target.clone()))
        .collect();
    shape.sort_by(|a, b| a.0.cmp(&b.0));
    shape
}

/// Type changes the migration engine can apply without user
/// intervention. The contract: every entry in this whitelist must be
/// data-preserving on both backends.
///
/// SQLite handles every entry trivially via the table-recreation
/// dance: its dynamic typing means whatever lives in a column today
/// reads back fine under a new column type affinity. Postgres needs
/// `ALTER COLUMN ... TYPE new_type USING column::new_type`, which the
/// renderer emits when this returns `true`.
///
/// What's *not* here is deliberate:
/// - `Text -> BigInt` / numeric parses can fail at runtime on non-
///   numeric rows. Force the user to write the migration so they own
///   the validation.
/// - Bigger int -> smaller int truncates silently.
/// - `Text -> Date` / `Text -> Uuid` are format-dependent.
/// - Anything -> JSON. Even if existing rows are JSON-shaped, that's
///   the user's invariant to assert.
fn is_safe_cast(from: SqlType, to: SqlType) -> bool {
    use SqlType::*;
    if from == to {
        return true;
    }
    match (from, to) {
        // Stringify: every scalar serialises to text losslessly. Read-
        // path code that wants the typed value parses it back; the
        // cast itself never fails.
        (
            SmallInt | Integer | BigInt | Real | Double | Boolean | Date | Time | Timestamptz
            | Uuid | Inet | Cidr | MacAddr | ForeignKey,
            Text,
        ) => true,
        // Integer widening — no data loss.
        (SmallInt, Integer | BigInt) => true,
        (Integer, BigInt) => true,
        // Float widening.
        (Real, Double) => true,
        // ForeignKey is stored as BigInt under the hood, so the two
        // directions are storage-identical. The Rust-side type is
        // different but the bytes on disk are not.
        (ForeignKey, BigInt) => true,
        (BigInt, ForeignKey) => true,
        _ => false,
    }
}

/// Postgres type name for an `ALTER COLUMN ... TYPE <name> USING …`
/// clause. Matches what sea-query's `PostgresQueryBuilder` emits for
/// the same `SqlType` inside a `CREATE TABLE`, so the resulting
/// schema after the alter is identical to a freshly created table.
fn postgres_type_name(ty: SqlType) -> &'static str {
    use SqlType::*;
    match ty {
        SmallInt => "smallint",
        Integer => "integer",
        BigInt | ForeignKey => "bigint",
        Real => "real",
        Double => "double precision",
        Boolean => "boolean",
        Text => "text",
        Date => "date",
        Time => "time",
        // sea-query's Postgres builder emits `timestamp with time zone`
        // for the equivalent column type; both spellings are accepted
        // by Postgres, but mirroring the builder keeps the surface
        // consistent if a test ever round-trips DDL.
        Timestamptz => "timestamp with time zone",
        Uuid => "uuid",
        Json => "jsonb",
        Inet => "inet",
        Cidr => "cidr",
        MacAddr => "macaddr",
        FullText => "tsvector",
        Bytes => "bytea",
        // BUG-10: NUMERIC(19, 4) — same dimensions as the CREATE TABLE
        // build path. Used by the `ALTER COLUMN ... TYPE ...` render
        // when the safe-cast diff allows transitioning to/from
        // Decimal.
        Decimal => "numeric(19, 4)",
        // Arrays render as `<inner>[]` in Postgres. The migration
        // engine doesn't model nested element types deeply enough to
        // emit a precise inner type here at v1; fall back to `text[]`
        // and rely on the column-def renderer for the real shape when
        // recreating the column.
        Array(_) => "text[]",
    }
}

/// Per-model column diff. Same-name columns whose type or nullable
/// flag changed return `UnsafeAlter` (no `AlterColumn` until M8 v1.1
/// covers the table-recreation dance for SQLite plus native ALTER for
/// Postgres). New-named columns emit `AddColumn`; missing-name columns
/// emit `DropColumn`. The ordering is: drops first, then adds, so a
/// rename-as-drop+add doesn't violate a uniqueness constraint mid-
/// migration on a single-row table.
fn diff_columns(
    model: &str,
    previous: &ModelMeta,
    current: &ModelMeta,
) -> Result<Vec<Operation>, MigrateError> {
    use std::collections::BTreeMap;

    let prev_cols: BTreeMap<&str, &Column> = previous
        .fields
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();
    let curr_cols: BTreeMap<&str, &Column> = current
        .fields
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();

    // Walk the intersection by name. Two questions per shared column:
    //   - did the type change? If so, is the change in the safe-cast
    //     whitelist (e.g. BigInt -> Text, SmallInt -> Integer)? Safe
    //     casts emit AlterColumn; unsafe ones still UnsafeAlter so the
    //     user is forced to write the data-preserving migration by
    //     hand.
    //   - did the nullable flag flip? AlterColumn either way.
    // Primary-key changes still UnsafeAlter (a PK rebuild is its own
    // dance and isn't shipped yet).
    let mut alter_columns: Vec<&str> = Vec::new();
    for (name, prev_col) in &prev_cols {
        if let Some(curr_col) = curr_cols.get(name) {
            if prev_col.primary_key != curr_col.primary_key {
                return Err(MigrateError::UnsafeAlter {
                    model: model.to_string(),
                    column: (*name).to_string(),
                    reason: "primary-key flips need a manual data-preserving migration".to_string(),
                });
            }
            let type_changed = prev_col.ty != curr_col.ty;
            if type_changed && !is_safe_cast(prev_col.ty, curr_col.ty) {
                return Err(MigrateError::UnsafeAlter {
                    model: model.to_string(),
                    column: (*name).to_string(),
                    reason: format!(
                        "type change {prev_ty:?} -> {curr_ty:?} is not in the safe-cast whitelist — write a data-preserving migration by hand",
                        prev_ty = prev_col.ty,
                        curr_ty = curr_col.ty,
                    ),
                });
            }
            // Any schema-meaningful field change triggers AlterColumn.
            // UI-only flags (`noform`, `noedit`, `max_length`,
            // `is_string_repr`, `is_multichoice`) are intentionally
            // excluded — they affect admin / OpenAPI rendering but
            // not the database schema, so emitting an ALTER would do
            // no DB work. The snapshot still updates because the next
            // CreateTable in the migration stream carries the flag.
            if type_changed
                || prev_col.nullable != curr_col.nullable
                || prev_col.fk_target != curr_col.fk_target
                || prev_col.unique != curr_col.unique
                || prev_col.default != curr_col.default
                || prev_col.choices != curr_col.choices
                || prev_col.choice_labels != curr_col.choice_labels
                || prev_col.on_delete != curr_col.on_delete
                || prev_col.on_update != curr_col.on_update
                || prev_col.index != curr_col.index
            {
                alter_columns.push(*name);
            }
        }
    }

    let mut ops: Vec<Operation> = Vec::new();

    // AlterColumn ops first, in name order. One AlterColumn per
    // changed column; each carries the full new schema so the render
    // can rebuild without further context. Multiple nullable flips on
    // one table generate multiple AlterColumns; the apply loop runs
    // them sequentially (each is a table-recreation, so back-to-back
    // alters drop and recreate twice; the cost is acceptable while
    // M5.1 ships the simple case).
    let new_columns: Vec<Column> = current.fields.clone();
    let prev_columns_snapshot: Vec<Column> = previous.fields.clone();
    for name in alter_columns {
        ops.push(Operation::AlterColumn {
            table: current.table.clone(),
            column: name.to_string(),
            new_columns: new_columns.clone(),
            prev_columns: Some(prev_columns_snapshot.clone()),
        });
    }

    // Collect the dropped + added column names. We need both lists in
    // memory so the rename heuristic can pair them.
    let mut dropped: Vec<&Column> = Vec::new();
    let mut added: Vec<&Column> = Vec::new();
    for (name, prev_col) in &prev_cols {
        if !curr_cols.contains_key(name) {
            dropped.push(prev_col);
        }
    }
    for col in &current.fields {
        if !prev_cols.contains_key(col.name.as_str()) {
            added.push(col);
        }
    }

    // Gap 88 — column rename detection. When the same diff yields
    // exactly one drop and one add whose column shapes (sans name)
    // match bit-for-bit, the most likely interpretation is a rename
    // rather than a coincidental drop+add of two unrelated columns.
    // Emit RenameColumn instead and warn the user so they can
    // verify. Anything more ambiguous (multiple drops or adds, or
    // mismatched shapes) falls back to the drop+add path so the
    // rename is never inferred against the user's actual intent.
    //
    // The heuristic deliberately stays conservative — Django's
    // makemigrations asks interactively in this case; we don't have
    // a prompt at v1, so the conservative auto-pair is the safest
    // shape. Users can always override by writing the
    // `RenameColumn` op into the migration file by hand.
    let mut paired_drop: Option<&str> = None;
    let mut paired_add: Option<&str> = None;
    if dropped.len() == 1 && added.len() == 1 {
        let d = dropped[0];
        let a = added[0];
        if column_shape_matches(d, a) {
            eprintln!(
                "umbra makemigrations: column rename detected on `{}`: \
                 `{}` → `{}` — verify this is a rename and not a coincidental \
                 shape match; edit the migration file if it's wrong",
                current.table, d.name, a.name,
            );
            ops.push(Operation::RenameColumn {
                table: current.table.clone(),
                from: d.name.clone(),
                to: a.name.clone(),
                column: Some(a.clone()),
            });
            paired_drop = Some(d.name.as_str());
            paired_add = Some(a.name.as_str());
        }
    }

    // Drops first so a same-position add can reuse the column slot.
    for col in &dropped {
        if Some(col.name.as_str()) == paired_drop {
            continue;
        }
        ops.push(Operation::DropColumn {
            table: current.table.clone(),
            column: col.name.clone(),
        });
    }

    // Then adds, in current declaration order so the schema retains
    // the user-written column order even after re-runs.
    for col in &added {
        if Some(col.name.as_str()) == paired_add {
            continue;
        }
        // Gap 97 — refuse to add a NOT NULL column without a default
        // (and without `auto_now_add` / `auto_now`, which fill the
        // column server-side at insert). SQLite + Postgres both
        // reject the ADD on a non-empty table; we surface the same
        // failure at diff time with actionable guidance so the user
        // doesn't ship a migration that bricks every deploy.
        if !col.nullable
            && col.default.is_empty()
            && !col.auto_now_add
            && !col.auto_now
            && !col.primary_key
        {
            return Err(MigrateError::UnsafeAlter {
                model: model.to_string(),
                column: col.name.clone(),
                reason: format!(
                    "adding NOT NULL column `{}` without a default to existing \
                     table `{}` would fail on every populated row. Pick one: \
                     (a) make the field `Option<T>`, (b) add `#[umbra(default = \
                     \"...\")]` so the migration backfills, or (c) add \
                     `#[umbra(auto_now_add)]` for timestamp columns",
                    col.name, current.table,
                ),
            });
        }
        ops.push(Operation::AddColumn {
            table: current.table.clone(),
            column: (*col).clone(),
        });
    }

    Ok(ops)
}

/// Gap 88 helper: compare two column snapshots for shape identity (every
/// schema-meaningful attribute except `name`). Used by the rename-
/// detection heuristic — bit-identical attrs are the signal that a
/// dropped column matches an added column and the diff is actually a
/// rename. Excludes UI-only flags (`noform`, `noedit`, `max_length`,
/// `is_string_repr`, `help`, `example`, `slug_from`) for the same
/// reason the AlterColumn diff excludes them: they have no DB effect.
fn column_shape_matches(a: &Column, b: &Column) -> bool {
    a.ty == b.ty
        && a.primary_key == b.primary_key
        && a.nullable == b.nullable
        && a.fk_target == b.fk_target
        && a.choices == b.choices
        && a.choice_labels == b.choice_labels
        && a.default == b.default
        && a.is_multichoice == b.is_multichoice
        && a.unique == b.unique
        && a.on_delete == b.on_delete
        && a.on_update == b.on_update
        && a.index == b.index
        && a.auto_now_add == b.auto_now_add
        && a.auto_now == b.auto_now
        && a.min == b.min
        && a.max == b.max
        && a.text_format == b.text_format
}

/// Pick the suffix used in a migration filename. Single-op migrations
/// get a descriptive suffix; multi-op migrations fall back to `auto`.
fn suffix_for(ops: &[Operation]) -> String {
    match ops {
        [Operation::CreateTable { table, .. }] => format!("create_{table}"),
        [Operation::DropTable { table }] => format!("drop_{table}"),
        [Operation::AddColumn { table, column }] => format!("add_{}_{}", table, column.name),
        [Operation::DropColumn { table, column }] => format!("drop_{table}_{column}"),
        [Operation::AlterColumn { table, column, .. }] => format!("alter_{table}_{column}"),
        [Operation::RenameTable { from, to }] => format!("rename_{from}_to_{to}"),
        [
            Operation::RenameColumn {
                table, from, to, ..
            },
        ] => format!("rename_{table}_{from}_to_{to}"),
        _ => "auto".to_string(),
    }
}

/// Create the tracking table if it isn't there already. The DDL is
/// dialect-neutral (TEXT + composite PK is valid SQL on both shipped
/// backends), but the executor type isn't — sqlx::query is generic
/// over the database, so each backend gets its own thin wrapper.
///
/// Kept inline because this table is a chicken-and-egg case: every
/// other migration needs the tracking row written, so the table
/// itself can't be a migration.
async fn ensure_tracking_table_sqlite(pool: &sqlx::SqlitePool) -> Result<(), MigrateError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS umbra_migrations (
            plugin TEXT NOT NULL,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL,
            snapshot_hash TEXT NOT NULL,
            PRIMARY KEY (plugin, name)
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Postgres counterpart to [`ensure_tracking_table_sqlite`].
async fn ensure_tracking_table_postgres(pool: &sqlx::PgPool) -> Result<(), MigrateError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS umbra_migrations (
            plugin TEXT NOT NULL,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL,
            snapshot_hash TEXT NOT NULL,
            PRIMARY KEY (plugin, name)
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Pull the set of `(plugin, name)` tuples already recorded in the
/// tracking table (SQLite).
async fn applied_names_sqlite(
    pool: &sqlx::SqlitePool,
) -> Result<std::collections::HashSet<(String, String)>, MigrateError> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT plugin, name FROM umbra_migrations")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Postgres counterpart to [`applied_names_sqlite`].
async fn applied_names_postgres(
    pool: &sqlx::PgPool,
) -> Result<std::collections::HashSet<(String, String)>, MigrateError> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT plugin, name FROM umbra_migrations")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Render one operation to a list of SQL statements via sea-query.
///
/// Dispatches on the ambient backend's [`crate::backend::active`]
/// name; SQLite and Postgres are the two shipped dialects. Most ops
/// produce one statement; `AlterColumn` produces either the SQLite
/// table-recreation dance (`CREATE _umbra_new` + `INSERT ... SELECT`
/// + `DROP` + `RENAME`) or a single native `ALTER TABLE ... ALTER
/// COLUMN ... SET/DROP NOT NULL` on Postgres.
///
/// The apply loop in `run_in` executes each statement in order inside
/// the same transaction.
///
/// `AddColumn` ignores the `primary_key` flag: neither SQLite nor
/// Postgres lets a primary key be added to an existing table without
/// a table-recreation step, and the autodetector won't route a
/// pk-flagged column through `AddColumn` anyway. A hand-edited
/// migration that sets the flag is taken to mean "the user is taking
/// responsibility".
fn render_operation(op: &Operation) -> Vec<String> {
    render_operation_for(op, crate::backend::active().name())
}

/// Render one operation against an explicit backend name. The
/// dispatching seam — the public [`render_operation`] is just
/// `render_operation_for(op, backend::active().name())`. Splitting
/// the two lets tests render Postgres DDL without installing the
/// process-wide ambient backend (the `OnceLock` can only be set once,
/// so `App::build` and tests would otherwise collide).
///
/// Panics on unknown backend names; only `"sqlite"` and `"postgres"`
/// are shipped in Phase 2.
pub fn render_operation_for(op: &Operation, backend_name: &str) -> Vec<String> {
    match backend_name {
        "sqlite" => render_operation_sqlite(op),
        "postgres" => render_operation_postgres(op),
        other => panic!(
            "umbra::migrate: no DDL renderer for backend `{other}`; \
             Phase 2 ships sqlite and postgres only"
        ),
    }
}

/// SQLite-dialect rendering for one operation.
fn render_operation_sqlite(op: &Operation) -> Vec<String> {
    use sea_query::{Alias, SqliteQueryBuilder, Table};

    match op {
        Operation::CreateTable {
            table,
            columns,
            unique_together,
            indexes,
        } => {
            // sea-query's TableCreateStatement renders columns inline.
            // For composite UNIQUE constraints, we append them via
            // `stmt.index(Index::create().unique().col(...))` — works on
            // both backends and uses sea-query's quoting.
            let mut stmt = Table::create();
            stmt.table(Alias::new(table));
            for col in columns {
                let mut def = build_column_def_sqlite(col);
                stmt.col(&mut def);
            }
            for group in unique_together {
                let mut idx = sea_query::Index::create().unique().to_owned();
                for col in group {
                    idx.col(Alias::new(col));
                }
                stmt.index(&mut idx);
            }
            let mut stmts = vec![stmt.build(SqliteQueryBuilder)];
            // Single-column `#[umbra(index)]` indexes follow the
            // CREATE TABLE. Same convention on both backends so a
            // SQLite-dev / Postgres-prod app sees parallel names.
            for col in columns {
                if col.index && !col.primary_key && !col.unique {
                    stmts.push(create_index_stmt(table, &col.name));
                }
            }
            // BUG-7: multi-column indexes follow as plain CREATE INDEX.
            for group in indexes {
                stmts.push(create_multi_index_stmt(table, group));
            }
            stmts
        }
        Operation::DropTable { table } => vec![
            Table::drop()
                .table(Alias::new(table))
                .build(SqliteQueryBuilder),
        ],
        Operation::AddColumn { table, column } => {
            // SQLite-specific limitation: `ALTER TABLE ADD COLUMN`
            // requires a CONSTANT default. `CURRENT_TIMESTAMP` is
            // non-constant ("Cannot add a column with non-constant
            // default"). So when we're adding a NOT NULL auto_now /
            // auto_now_add column on top of an existing table, we
            // emit a two-statement sequence:
            //   1. ADD COLUMN as NULLABLE (no default needed).
            //   2. UPDATE every existing row to `datetime('now')`.
            // The column ends up NULL-permitting at the DB level on
            // SQLite — but the Rust type stays `DateTime<Utc>` (not
            // Option), and every INSERT through the ORM supplies a
            // value via the macro-emitted auto_now arm. The DB-side
            // NOT NULL guarantee is lost only for direct-SQL writers,
            // which umbra already discourages (see CLAUDE.md "Plugins
            // use the ORM"). Postgres has no such restriction —
            // `DEFAULT now()` works there in ALTER, no backfill
            // statement needed (see the Postgres render below).
            let needs_backfill = (column.auto_now || column.auto_now_add)
                && !column.nullable
                && matches!(
                    column.ty,
                    SqlType::Timestamptz | SqlType::Date | SqlType::Time
                );

            let mut stmts = if needs_backfill {
                let mut nullable_col = column.clone();
                nullable_col.nullable = true;
                let mut stmt = Table::alter();
                stmt.table(Alias::new(table));
                let mut def = build_column_def_sqlite(&nullable_col);
                stmt.add_column(&mut def);
                let add_sql = stmt.build(SqliteQueryBuilder);

                // Manual UPDATE — sea-query's update builder is
                // overkill for a single SET col = datetime('now').
                let table_quoted = table.replace('"', "\"\"");
                let col_quoted = column.name.replace('"', "\"\"");
                let backfill_sql = format!(
                    "UPDATE \"{table_quoted}\" SET \"{col_quoted}\" = datetime('now') \
                     WHERE \"{col_quoted}\" IS NULL"
                );
                vec![add_sql, backfill_sql]
            } else {
                let mut stmt = Table::alter();
                stmt.table(Alias::new(table));
                let mut def = build_column_def_sqlite(column);
                stmt.add_column(&mut def);
                vec![stmt.build(SqliteQueryBuilder)]
            };
            if column.index && !column.primary_key && !column.unique {
                stmts.push(create_index_stmt(table, &column.name));
            }
            stmts
        }
        Operation::DropColumn { table, column } => vec![
            Table::alter()
                .table(Alias::new(table))
                .drop_column(Alias::new(column))
                .build(SqliteQueryBuilder),
        ],
        Operation::AlterColumn {
            table,
            column: _,
            new_columns,
            prev_columns: _,
        } => render_alter_column_dance_sqlite(table, new_columns),
        Operation::CreateM2MTable {
            junction_table,
            parent_table,
            parent_col,
            child_table,
            child_col,
            parent_ty,
            child_ty,
        } => {
            // Junction table for many-to-many: two FK columns + composite PK.
            // Column types follow the referenced PKs — `BigInt` → `INTEGER`
            // (SQLite affinity), `Text` → `TEXT`, `Uuid` → `TEXT` on SQLite
            // / `UUID` on Postgres. Raw DDL is the simplest expression of
            // the composite-PK + per-side cascade FK shape; sea-query's
            // builder can't express it cleanly in one call.
            vec![format!(
                r#"CREATE TABLE "{jt}" (
    "parent_id" {pty} NOT NULL REFERENCES "{pt}"("{pc}") ON DELETE CASCADE,
    "child_id" {cty} NOT NULL REFERENCES "{ct}"("{cc}") ON DELETE CASCADE,
    PRIMARY KEY ("parent_id", "child_id")
)"#,
                jt = junction_table,
                pt = parent_table,
                pc = parent_col,
                ct = child_table,
                cc = child_col,
                pty = m2m_pk_sql_type_sqlite(*parent_ty),
                cty = m2m_pk_sql_type_sqlite(*child_ty),
            )]
        }
        Operation::DropM2MTable { junction_table } => vec![
            Table::drop()
                .table(Alias::new(junction_table))
                .build(SqliteQueryBuilder),
        ],
        Operation::RenameTable { from, to } => {
            use sea_query::{Alias, SqliteQueryBuilder, Table};
            vec![
                Table::rename()
                    .table(Alias::new(from.as_str()), Alias::new(to.as_str()))
                    .build(SqliteQueryBuilder),
            ]
        }
        Operation::RenameColumn {
            table, from, to, ..
        } => {
            // SQLite 3.25+ supports `ALTER TABLE ... RENAME COLUMN`
            // natively. Quote both sides to allow names that need
            // escaping; sea-query's column-rename builder isn't
            // exposed cleanly so we render the DDL string directly.
            let t = table.replace('"', "\"\"");
            let f = from.replace('"', "\"\"");
            let tn = to.replace('"', "\"\"");
            vec![format!(
                "ALTER TABLE \"{t}\" RENAME COLUMN \"{f}\" TO \"{tn}\""
            )]
        }
    }
}

/// Postgres-dialect rendering for one operation.
///
/// Postgres has native `ALTER COLUMN` so `AlterColumn` doesn't need
/// the SQLite table-recreation dance; it lowers to a single statement.
/// Integer primary keys use sea-query's `auto_increment()` flag, which
/// the Postgres query builder lowers to `BIGSERIAL` / `SERIAL` rather
/// than SQLite's `INTEGER PRIMARY KEY AUTOINCREMENT` quirk.
fn render_operation_postgres(op: &Operation) -> Vec<String> {
    use sea_query::{Alias, PostgresQueryBuilder, Table};

    match op {
        Operation::CreateTable {
            table,
            columns,
            unique_together,
            indexes,
        } => {
            let mut stmt = Table::create();
            stmt.table(Alias::new(table));
            for col in columns {
                let mut def = build_column_def_postgres(col);
                stmt.col(&mut def);
            }
            for group in unique_together {
                let mut idx = sea_query::Index::create().unique().to_owned();
                for col in group {
                    idx.col(Alias::new(col));
                }
                stmt.index(&mut idx);
            }
            let mut stmts = vec![stmt.build(PostgresQueryBuilder)];
            for col in columns {
                if col.index && !col.primary_key && !col.unique {
                    stmts.push(create_index_stmt(table, &col.name));
                }
            }
            for group in indexes {
                stmts.push(create_multi_index_stmt(table, group));
            }
            stmts
        }
        Operation::DropTable { table } => vec![
            Table::drop()
                .table(Alias::new(table))
                .build(PostgresQueryBuilder),
        ],
        Operation::AddColumn { table, column } => {
            let mut stmt = Table::alter();
            stmt.table(Alias::new(table));
            let mut def = build_column_def_postgres(column);
            stmt.add_column(&mut def);
            let mut stmts = vec![stmt.build(PostgresQueryBuilder)];
            if column.index && !column.primary_key && !column.unique {
                stmts.push(create_index_stmt(table, &column.name));
            }
            stmts
        }
        Operation::DropColumn { table, column } => vec![
            Table::alter()
                .table(Alias::new(table))
                .drop_column(Alias::new(column))
                .build(PostgresQueryBuilder),
        ],
        Operation::AlterColumn {
            table,
            column,
            new_columns,
            prev_columns,
        } => render_alter_column_postgres(table, column, new_columns, prev_columns.as_deref()),
        Operation::CreateM2MTable {
            junction_table,
            parent_table,
            parent_col,
            child_table,
            child_col,
            parent_ty,
            child_ty,
        } => {
            vec![format!(
                r#"CREATE TABLE "{jt}" (
    "parent_id" {pty} NOT NULL REFERENCES "{pt}"("{pc}") ON DELETE CASCADE,
    "child_id" {cty} NOT NULL REFERENCES "{ct}"("{cc}") ON DELETE CASCADE,
    PRIMARY KEY ("parent_id", "child_id")
)"#,
                jt = junction_table,
                pt = parent_table,
                pc = parent_col,
                ct = child_table,
                cc = child_col,
                pty = m2m_pk_sql_type_postgres(*parent_ty),
                cty = m2m_pk_sql_type_postgres(*child_ty),
            )]
        }
        Operation::DropM2MTable { junction_table } => vec![
            Table::drop()
                .table(Alias::new(junction_table))
                .build(PostgresQueryBuilder),
        ],
        Operation::RenameTable { from, to } => {
            // Postgres: ALTER TABLE "<from>" RENAME TO "<to>"
            // sea-query's Table::rename() emits the right form.
            use sea_query::{Alias, PostgresQueryBuilder, Table};
            vec![
                Table::rename()
                    .table(Alias::new(from.as_str()), Alias::new(to.as_str()))
                    .build(PostgresQueryBuilder),
            ]
        }
        Operation::RenameColumn {
            table, from, to, ..
        } => {
            let t = table.replace('"', "\"\"");
            let f = from.replace('"', "\"\"");
            let tn = to.replace('"', "\"\"");
            vec![format!(
                "ALTER TABLE \"{t}\" RENAME COLUMN \"{f}\" TO \"{tn}\""
            )]
        }
    }
}

/// The SQLite table-recreation dance for `AlterColumn`. SQLite has no
/// in-place `ALTER COLUMN`, so the only safe way to flip a column's
/// nullable flag is to rebuild the table:
///
/// 1. `CREATE TABLE _umbra_new_<table>` with the new schema.
/// 2. `INSERT ... SELECT` to copy every row from the old table.
/// 3. `DROP TABLE <table>`.
/// 4. `ALTER TABLE _umbra_new_<table> RENAME TO <table>`.
///
/// Wrapped in a transaction by the caller. Indexes, triggers, and FK
/// targets aren't preserved at M5.1 because umbra-core's schema model
/// doesn't yet carry them; once it does, this routine picks them up
/// by rebuilding them at step 1.
///
/// Nullable `TRUE -> FALSE` fails at step 2 if any row holds NULL,
/// which is the correct data-integrity behaviour. Nullable
/// `FALSE -> TRUE` always succeeds.
fn render_alter_column_dance_sqlite(table: &str, new_columns: &[Column]) -> Vec<String> {
    use sea_query::{Alias, SqliteQueryBuilder, Table};

    let tmp = format!("_umbra_new_{table}");

    // Step 1 — CREATE TABLE _umbra_new_<table>.
    let mut create = Table::create();
    create.table(Alias::new(&tmp));
    for col in new_columns {
        let mut def = build_column_def_sqlite(col);
        create.col(&mut def);
    }

    // Step 2 — INSERT ... SELECT. Same column list both sides; the
    // dance only handles nullable flips (columns are otherwise
    // identical). Each name is double-quoted so SQLite identifier
    // rules don't bite on reserved words.
    let column_list = new_columns
        .iter()
        .map(|c| format!("\"{}\"", c.name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let insert_sql =
        format!("INSERT INTO \"{tmp}\" ({column_list}) SELECT {column_list} FROM \"{table}\"");

    // Step 3 — DROP TABLE <table>.
    let drop_sql = Table::drop()
        .table(Alias::new(table))
        .build(SqliteQueryBuilder);

    // Step 4 — ALTER TABLE _umbra_new_<table> RENAME TO <table>.
    let rename_sql = Table::rename()
        .table(Alias::new(&tmp), Alias::new(table))
        .build(SqliteQueryBuilder);

    vec![
        create.build(SqliteQueryBuilder),
        insert_sql,
        drop_sql,
        rename_sql,
    ]
}

/// Native Postgres `AlterColumn`. Postgres supports
/// `ALTER TABLE x ALTER COLUMN y SET NOT NULL` and
/// `ALTER TABLE x ALTER COLUMN y DROP NOT NULL` in place, so the
/// SQLite table-recreation dance isn't needed. Lowers to a single
/// statement.
///
/// `SET NOT NULL` fails at the server if any row holds NULL on `y`,
/// matching SQLite's INSERT-time failure on the dance — the
/// data-integrity contract is identical between backends.
///
/// `column` is the field name that triggered the flip; `new_columns`
/// is the post-change schema (carried for parity with the SQLite
/// dance, though Postgres only needs the one column).
fn render_alter_column_postgres(
    table: &str,
    column: &str,
    new_columns: &[Column],
    prev_columns: Option<&[Column]>,
) -> Vec<String> {
    let new = new_columns.iter().find(|c| c.name == column).expect(
        "umbra::migrate: AlterColumn op references a column missing from new_columns; \
             this is a bug in `diff_columns`",
    );
    let prev = prev_columns.and_then(|cols| cols.iter().find(|c| c.name == column));

    let q_table = quote_pg_ident(table);
    let q_column = quote_pg_ident(column);

    let mut stmts: Vec<String> = Vec::new();

    // TYPE change: only when we have a previous snapshot AND it differs
    // AND the change is in the safe-cast whitelist (diff_columns has
    // already gated unsafe ones). Emitted before nullable so a NOT
    // NULL flip against the just-cast column reads the new type.
    if let Some(prev_col) = prev {
        if prev_col.ty != new.ty && is_safe_cast(prev_col.ty, new.ty) {
            let new_ty_sql = postgres_type_name(new.ty);
            stmts.push(format!(
                "ALTER TABLE {q_table} ALTER COLUMN {q_column} TYPE {new_ty_sql} USING {q_column}::{new_ty_sql}"
            ));
        }
    }

    // NULL-flag change: skipped when prev is None (legacy migrations
    // with no snapshot — preserve the old "emit unconditionally" path
    // because it's idempotent on Postgres). With a snapshot, only emit
    // when the flag actually flipped.
    let nullable_changed = match prev {
        Some(prev_col) => prev_col.nullable != new.nullable,
        None => true,
    };
    if nullable_changed {
        let clause = if new.nullable {
            "DROP NOT NULL"
        } else {
            "SET NOT NULL"
        };
        stmts.push(format!(
            "ALTER TABLE {q_table} ALTER COLUMN {q_column} {clause}"
        ));
    }

    // From here down — all the gap #65 follow-up changes. Each branch
    // checks if `prev` exists (legacy migrations with no snapshot
    // skip these, matching the historical behaviour) and emits the
    // matching ALTER on real flips.
    if let Some(prev_col) = prev {
        // UNIQUE flag flip. Postgres autogen for column-level UNIQUE
        // at CREATE TABLE is `<table>_<col>_key`; we use the same
        // name when ADDing so a subsequent DROP finds it.
        if prev_col.unique != new.unique {
            let cname = format!("{table}_{column}_key");
            if new.unique {
                stmts.push(format!(
                    "ALTER TABLE {q_table} ADD CONSTRAINT \"{cname}\" UNIQUE ({q_column})"
                ));
            } else {
                stmts.push(format!(
                    "ALTER TABLE {q_table} DROP CONSTRAINT IF EXISTS \"{cname}\""
                ));
            }
        }

        // DEFAULT change. Empty string in either snapshot means "no
        // default"; the canonical SET / DROP pair fully expresses
        // the transition.
        if prev_col.default != new.default {
            if new.default.is_empty() {
                stmts.push(format!(
                    "ALTER TABLE {q_table} ALTER COLUMN {q_column} DROP DEFAULT"
                ));
            } else {
                let escaped = new.default.replace('\'', "''");
                stmts.push(format!(
                    "ALTER TABLE {q_table} ALTER COLUMN {q_column} SET DEFAULT '{escaped}'"
                ));
            }
        }

        // FK target / on_delete / on_update — these are all carried
        // on the same constraint, so any one of them flipping
        // requires a DROP + readd of the whole FK. Autogen name
        // convention `<table>_<col>_fkey` matches Postgres at CREATE
        // TABLE time. Only emitted when the new column is still a
        // FK; if the column stopped being a FK (ty changed away
        // from ForeignKey), the type-change branch above handles
        // it indirectly via the column type rewrite.
        let fk_changed = prev_col.fk_target != new.fk_target
            || prev_col.on_delete != new.on_delete
            || prev_col.on_update != new.on_update;
        if fk_changed && matches!(new.ty, SqlType::ForeignKey) {
            let cname = format!("{table}_{column}_fkey");
            stmts.push(format!(
                "ALTER TABLE {q_table} DROP CONSTRAINT IF EXISTS \"{cname}\""
            ));
            if let Some(target) = &new.fk_target {
                let q_target = quote_pg_ident(target);
                let on_delete_clause = new
                    .on_delete
                    .sql_keyword()
                    .map(|k| format!(" ON DELETE {k}"))
                    .unwrap_or_default();
                let on_update_clause = new
                    .on_update
                    .sql_keyword()
                    .map(|k| format!(" ON UPDATE {k}"))
                    .unwrap_or_default();
                stmts.push(format!(
                    "ALTER TABLE {q_table} ADD CONSTRAINT \"{cname}\" \
                     FOREIGN KEY ({q_column}) REFERENCES {q_target}(\"id\")\
                     {on_delete_clause}{on_update_clause}"
                ));
            }
        }

        // CHECK constraint (single-valued choices) change. MultiChoice
        // uses CSV storage which can't be expressed as a column-level
        // IN constraint; the runtime sqlx Decode path is the guard.
        if prev_col.choices != new.choices && !new.is_multichoice {
            let cname = format!("{table}_{column}_check");
            stmts.push(format!(
                "ALTER TABLE {q_table} DROP CONSTRAINT IF EXISTS \"{cname}\""
            ));
            if !new.choices.is_empty() {
                let values_sql = new
                    .choices
                    .iter()
                    .map(|v| format!("'{}'", v.replace('\'', "''")))
                    .collect::<Vec<_>>()
                    .join(", ");
                stmts.push(format!(
                    "ALTER TABLE {q_table} ADD CONSTRAINT \"{cname}\" \
                     CHECK ({q_column} IN ({values_sql}))"
                ));
            }
        }
    }

    // Defensive: if we somehow produced no statements (shouldn't
    // happen — diff_columns gates on at least one schema-meaningful
    // flag changing), fall back to a single redundant SET NULL flip
    // to match the legacy contract. Tests cover both branches; this
    // is belt-and-braces.
    if stmts.is_empty() {
        let clause = if new.nullable {
            "DROP NOT NULL"
        } else {
            "SET NOT NULL"
        };
        stmts.push(format!(
            "ALTER TABLE {q_table} ALTER COLUMN {q_column} {clause}"
        ));
    }

    stmts
}

/// Quote a SQL identifier the Postgres way: wrap in double quotes,
/// escape inner double quotes by doubling them. Matches sea-query's
/// `PostgresQueryBuilder` output for identifiers so the rendered
/// statements look uniform.
fn quote_pg_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Build a SQLite `ColumnDef`. SQLite has one important quirk: its
/// ROWID-alias mechanic (which gives a primary-key column auto-
/// increment behaviour out of the box) only fires when the column's
/// type is the exact text `INTEGER` — case-insensitive but no other
/// variant. `BIGINT PRIMARY KEY`, even on a column the M3 derive
/// declared as `i64`, does NOT auto-increment, so an `INSERT INTO t
/// (other_col) VALUES (...)` without an explicit PK value fails the
/// NOT NULL constraint. Every umbra user with an `id: i64` model
/// would hit this without the override.
///
/// The fix: when a column is a primary key with an integer SqlType
/// (Integer or BigInt), force the rendered type to `Integer` and
/// attach `auto_increment()` so the generated DDL reads `"id" integer
/// NOT NULL PRIMARY KEY AUTOINCREMENT`. SQLite stores both `i32` and
/// `i64` as INTEGER affinity anyway, so the override is a no-op
/// semantically — the rows that round-trip through `sqlx::FromRow`
/// deserialize back into `i64` cleanly.
///
/// For `SqlType::Uuid` PKs: SQLite stores UUIDs as TEXT. No
/// `DEFAULT gen_random_uuid()` is emitted; the application must supply
/// the UUID at create time (or pass `Uuid::nil()` to trigger the
/// omit-on-insert sentinel that leaves the column to a future default).
///
/// For `SqlType::ForeignKey` columns: rendered as `BIGINT` with a
/// `REFERENCES "<target>"("id")` suffix appended via `.extra()`. The
/// target table name comes from `col.fk_target`.
/// Look up the FK target model's primary-key column name and SQL
/// type. Walks the registered ModelMeta set to find the model whose
/// table matches `fk_target_table`, then picks the first column
/// marked `primary_key = true`. Falls back to `("id", BigInteger)`
/// when the target isn't registered (cross-plugin lookup miss, or
/// the FK points outside the framework's model registry).
///
/// Used by both the SQLite and Postgres FK column-def builders so the
/// generated `<col> <type> REFERENCES <tbl>(<pk_col>)` matches the
/// target's actual PK shape — gap #60 made non-`id`, non-i64 PKs
/// (e.g. `Permission.codename: String`) a real case.
fn fk_target_pk(fk_target_table: &str) -> (String, sea_query::ColumnType) {
    use sea_query::ColumnType;
    let unesc = fk_target_table.replace("\"\"", "\"");
    // Non-panicking registry read — `registered_models()` itself
    // panics when called outside an `App::build()` context, but the
    // migration engine's unit tests construct snapshots by hand and
    // call into DDL emit without booting the framework. Fall through
    // to the historical "id"/BigInteger default in that case.
    let Some(metas) = REGISTRY.get() else {
        return ("id".to_string(), ColumnType::BigInteger);
    };
    for meta in metas.iter().map(|(_, m)| m) {
        if meta.table != unesc {
            continue;
        }
        if let Some(pk) = meta.fields.iter().find(|c| c.primary_key) {
            // Map the PK's SqlType to a sea-query ColumnType. We can't
            // route through `SqliteBackend::map_column` because that
            // wants a `Column` and applies max_length / choices
            // metadata which is irrelevant to a FK column. Hand-roll
            // the few cases the framework supports for PKs.
            let ct = match pk.ty {
                SqlType::BigInt | SqlType::Integer => ColumnType::BigInteger,
                SqlType::SmallInt => ColumnType::SmallInteger,
                SqlType::Text => ColumnType::Text,
                SqlType::Uuid => ColumnType::Uuid,
                // Other PK types fall back to BigInteger as the
                // historical default. The compile-time PrimaryKey
                // trait keeps this list closed in practice.
                _ => ColumnType::BigInteger,
            };
            return (pk.name.clone(), ct);
        }
    }
    ("id".to_string(), ColumnType::BigInteger)
}

fn build_column_def_sqlite(col: &Column) -> sea_query::ColumnDef {
    use sea_query::{Alias, ColumnDef, ColumnType};

    // ForeignKey gets a special path: column type + inline REFERENCES
    // clause both derived from the target model's PK column.
    if matches!(col.ty, SqlType::ForeignKey) {
        let fk_target = col
            .fk_target
            .as_deref()
            .unwrap_or("_unknown_")
            .replace('"', "\"\"");
        let (pk_col_name, pk_col_type) = fk_target_pk(&fk_target);
        let mut def = ColumnDef::new_with_type(Alias::new(&col.name), pk_col_type);
        if !col.nullable {
            def.not_null();
        }
        // BUG-15: `#[umbra(unique)]` on a FK column is the
        // OneToOne idiom — emit UNIQUE inline so the
        // referencing-row uniqueness is enforced at the DB.
        // The FK branch used to skip this because it returned
        // before the non-FK unique branch ran.
        if col.unique {
            def.unique_key();
        }
        def.extra(format!(
            "REFERENCES \"{fk_target}\"(\"{pk_col_name}\"){}",
            fk_action_suffix(col),
        ));
        return def;
    }

    let is_int_pk = col.primary_key && matches!(col.ty, SqlType::Integer | SqlType::BigInt);

    let column_type = if is_int_pk {
        ColumnType::Integer
    } else {
        crate::backend::SqliteBackend.map_column(col)
    };

    let mut def = ColumnDef::new_with_type(Alias::new(&col.name), column_type);
    if !col.nullable {
        def.not_null();
    }
    if col.primary_key {
        def.primary_key();
        if is_int_pk {
            def.auto_increment();
        }
    }
    // `#[umbra(unique)]` lifts to a column-level UNIQUE clause.
    // Skipped on PK columns (already unique) so the DDL stays tidy.
    if col.unique && !col.primary_key {
        def.unique_key();
    }
    // IMP-3: `#[umbra(min = N)]` / `#[umbra(max = N)]` lift to a
    // column-level CHECK clause. Both SQLite and Postgres accept the
    // same syntax. The pre-validation in `insert_json`/`update_json`
    // catches violations earlier with a friendlier error; the CHECK
    // is the DB-side safety net against direct-SQL writers.
    if let Some(check) = check_min_max_sql(col) {
        def.extra(check);
    }
    // User-declared `#[umbra(default = "...")]` lifts to a DDL DEFAULT
    // clause. Required when emitting `ALTER TABLE ADD COLUMN` for a
    // NOT NULL column against a non-empty table (SQLite rejects the
    // ADD otherwise); on CREATE TABLE it sets the column-level default
    // the database uses when an INSERT omits the value.
    //
    // SQLite stores booleans as INTEGER; the literal `'true'` /
    // `'false'` would land as a TEXT default that fails type checks
    // on reads. Translate Boolean defaults to `1` / `0` so the
    // stored representation matches what sqlx expects on hydration
    // (closes IMP-2 in bugs/tests/testBugs.md).
    if !col.default.is_empty() {
        if matches!(col.ty, SqlType::Boolean) {
            // Pass an integer to sea-query so the rendered SQL is
            // `DEFAULT 1` / `DEFAULT 0` instead of the quoted-string
            // `DEFAULT '1'` (which sqlx rejects as TEXT on read of
            // a BOOLEAN column).
            def.default(sqlite_bool_default(&col.default));
        } else {
            def.default(col.default.clone());
        }
    }
    // NOTE: auto_now / auto_now_add deliberately does NOT emit a
    // `DEFAULT CURRENT_TIMESTAMP` here. SQLite rejects non-constant
    // defaults in `ALTER TABLE ADD COLUMN` ("Cannot add a column
    // with non-constant default") and that's the path that matters
    // for evolving an existing table. The SQLite `AddColumn` render
    // path handles the auto_now backfill via a two-statement
    // sequence (nullable ADD + UPDATE backfill). On CREATE TABLE
    // we don't need a default at all because every INSERT goes
    // through the macro-emitted Rust path which always supplies the
    // value. See `Operation::AddColumn` render below.
    def
}

/// Map a user-supplied boolean default string (`"true"` / `"false"`
/// / `"1"` / `"0"`, case-insensitive) to the SQLite integer literal
/// the column expects. Anything unrecognised falls through to `0`
/// — a developer-visible miss (default is wrong, not stored as
/// text) is friendlier than the runtime decode error the textual
/// path produces.
fn sqlite_bool_default(raw: &str) -> i32 {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "t" | "yes" => 1,
        _ => 0,
    }
}

/// IMP-3: lower `#[umbra(min = N)]` / `#[umbra(max = N)]` to a
/// DDL CHECK clause. Returns `None` when the column declares
/// neither bound. The rendered SQL works on both SQLite and
/// Postgres (`"<col>" >= N`, `"<col>" <= N`, joined by `AND`).
/// Only applied to numeric columns — applying it to text would
/// compare strings lexicographically and surprise everyone.
fn check_min_max_sql(col: &Column) -> Option<String> {
    if col.min.is_none() && col.max.is_none() {
        return None;
    }
    if !matches!(
        col.ty,
        SqlType::SmallInt | SqlType::Integer | SqlType::BigInt | SqlType::Real | SqlType::Double
    ) {
        return None;
    }
    let name = col.name.replace('"', "\"\"");
    let mut parts = Vec::with_capacity(2);
    if let Some(n) = col.min {
        parts.push(format!("\"{name}\" >= {n}"));
    }
    if let Some(n) = col.max {
        parts.push(format!("\"{name}\" <= {n}"));
    }
    Some(format!("CHECK ({})", parts.join(" AND ")))
}

/// Build a Postgres `ColumnDef`. Integer primary keys use the
/// standard `auto_increment()` flag — sea-query's `PostgresQueryBuilder`
/// lowers that to `BIGSERIAL` for `BigInt` and `SERIAL` for `Integer`.
/// No SQLite-style INTEGER-type override needed; Postgres has proper
/// `BIGSERIAL` / identity columns and respects the declared width.
///
/// For `SqlType::ForeignKey` columns: rendered as `BIGINT` with a
/// `REFERENCES "<target>"("id")` suffix. The target table name comes
/// from `col.fk_target`.
fn build_column_def_postgres(col: &Column) -> sea_query::ColumnDef {
    use sea_query::{Alias, ColumnDef};

    // ForeignKey gets a special path: column type + inline REFERENCES
    // clause both derived from the target model's PK.
    if matches!(col.ty, SqlType::ForeignKey) {
        let fk_target = col
            .fk_target
            .as_deref()
            .unwrap_or("_unknown_")
            .replace('"', "\"\"");
        let (pk_col_name, pk_col_type) = fk_target_pk(&fk_target);
        // sea-query's ColumnType variants are dialect-agnostic; the
        // same value works for both SQLite and Postgres builders here.
        let mut def = ColumnDef::new_with_type(Alias::new(&col.name), pk_col_type);
        if !col.nullable {
            def.not_null();
        }
        // BUG-15: `#[umbra(unique)]` on a FK column is the
        // OneToOne idiom — emit UNIQUE inline so the
        // referencing-row uniqueness is enforced at the DB.
        // The FK branch used to skip this because it returned
        // before the non-FK unique branch ran.
        if col.unique {
            def.unique_key();
        }
        def.extra(format!(
            "REFERENCES \"{fk_target}\"(\"{pk_col_name}\"){}",
            fk_action_suffix(col),
        ));
        return def;
    }

    let column_type = crate::backend::PostgresBackend.map_column(col);

    let mut def = ColumnDef::new_with_type(Alias::new(&col.name), column_type);
    if !col.nullable {
        def.not_null();
    }
    if col.primary_key {
        def.primary_key();
        if matches!(
            col.ty,
            SqlType::Integer | SqlType::BigInt | SqlType::SmallInt
        ) {
            def.auto_increment();
        }
    }
    // `#[umbra(unique)]` lifts to a column-level UNIQUE clause on
    // Postgres too. Skipped for PK columns (already unique).
    if col.unique && !col.primary_key {
        def.unique_key();
    }
    // IMP-3: numeric bounds CHECK. Mirrors the SQLite branch.
    if let Some(check) = check_min_max_sql(col) {
        def.extra(check);
    }
    // Single-valued Choices: emit a CHECK constraint so a third-party
    // process writing directly to the DB can't insert a value the Rust
    // enum can't model. MultiChoice carries the same choices/labels
    // metadata but the stored value is a CSV — a single-value `IN (...)`
    // constraint would reject every legal CSV. Validating "every CSV
    // piece is a known variant" needs a regex with per-variant
    // escaping, which we leave to the sqlx Decode path at v1.
    if !col.choices.is_empty() && !col.is_multichoice {
        let col_name_escaped = col.name.replace('"', "\"\"");
        let values_sql = col
            .choices
            .iter()
            .map(|v| format!("'{}'", v.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");
        def.extra(format!("CHECK (\"{col_name_escaped}\" IN ({values_sql}))"));
    }
    // User-declared `#[umbra(default = "...")]` lifts to a DDL DEFAULT
    // clause. Required for `ALTER TABLE ADD COLUMN` of a NOT NULL
    // column against a non-empty table — Postgres needs either a
    // default or a separate backfill.
    if !col.default.is_empty() {
        def.default(col.default.clone());
    } else if (col.auto_now || col.auto_now_add)
        && matches!(col.ty, SqlType::Timestamptz | SqlType::Date | SqlType::Time)
    {
        // Mirror of the SQLite branch above. Without a DEFAULT
        // Postgres rejects `ALTER TABLE ADD COLUMN ... NOT NULL`
        // on a populated table. `now()` evaluates per-row during
        // the backfill so every existing row gets a sane value;
        // future INSERTs override via the macro-emitted Rust path.
        def.default(sea_query::Expr::cust("now()"));
    }
    def
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M8 — `plugin_order()` falls back to `registered_plugins()` when
    /// no topological order has been published. The fallback keeps the
    /// engine usable from low-level paths that drive `init_plugins`
    /// directly (the M5 / M6 tests that pre-date phase 1.5 of
    /// `App::build()`).
    ///
    /// Runs in the lib's unit-test binary, which is wholly separate
    /// from the integration test binaries and so owns its own copies
    /// of `REGISTRY` and `PLUGIN_ORDER`. This test seeds `REGISTRY` via
    /// `init_plugins`, never touches `init_plugin_order`, and pins the
    /// fallback to the sorted-by-name `registered_plugins()` output.
    /// As the only test that touches either OnceLock in this binary,
    /// it has them to itself.
    #[test]
    fn plugin_order_falls_back_to_registered_plugins_when_unpublished() {
        let mut per_plugin: std::collections::HashMap<String, Vec<ModelMeta>> =
            std::collections::HashMap::new();
        per_plugin.insert(
            "zeta".to_string(),
            vec![ModelMeta {
                name: "ZetaModel".to_string(),
                table: "zeta".to_string(),
                fields: Vec::new(),
                display: "ZetaModel".to_string(),
                icon: "database".to_string(),
                database: None,
                singleton: false,
                unique_together: Vec::new(),
                indexes: Vec::new(),
                ordering: Vec::new(),
                m2m_relations: Vec::new(),
                soft_delete: false,
            }],
        );
        per_plugin.insert(
            "alpha".to_string(),
            vec![ModelMeta {
                name: "AlphaModel".to_string(),
                table: "alpha".to_string(),
                fields: Vec::new(),
                display: "AlphaModel".to_string(),
                icon: "database".to_string(),
                database: None,
                singleton: false,
                unique_together: Vec::new(),
                indexes: Vec::new(),
                ordering: Vec::new(),
                m2m_relations: Vec::new(),
                soft_delete: false,
            }],
        );
        init_plugins(per_plugin);

        // `init_plugin_order` was never called, so `plugin_order` must
        // return the sorted-by-name fallback.
        let order = plugin_order();
        assert_eq!(
            order,
            vec!["alpha".to_string(), "zeta".to_string()],
            "fallback should sort by name; got {order:?}",
        );
        assert_eq!(
            order,
            registered_plugins(),
            "fallback should exactly equal registered_plugins()",
        );
    }

    /// Gap #65: `#[umbra(unique)]` lifts to a column-level UNIQUE in
    /// CREATE TABLE DDL on both backends. PK columns skip the clause
    /// because they're already unique by virtue of being the PK.
    #[test]
    fn unique_column_emits_unique_keyword_on_both_backends() {
        use sea_query::{Alias, PostgresQueryBuilder, SqliteQueryBuilder, Table};

        let id = Column {
            name: "id".into(),
            ty: SqlType::BigInt,
            primary_key: true,
            nullable: false,
            fk_target: None,
            noform: false,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: vec![],
            choice_labels: vec![],
            default: String::new(),
            is_multichoice: false,
            // Set even though it's a PK so we can assert below that
            // the emit path drops the redundant clause.
            unique: true,
            on_delete: crate::orm::FkAction::NoAction,
            on_update: crate::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: ::core::option::Option::None,
            slug_from: ::core::option::Option::None,
        };
        let username = Column {
            name: "username".into(),
            ty: SqlType::Text,
            primary_key: false,
            nullable: false,
            fk_target: None,
            noform: false,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: vec![],
            choice_labels: vec![],
            default: String::new(),
            is_multichoice: false,
            unique: true,
            on_delete: crate::orm::FkAction::NoAction,
            on_update: crate::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: ::core::option::Option::None,
            slug_from: ::core::option::Option::None,
        };
        let email = Column {
            name: "email".into(),
            ty: SqlType::Text,
            primary_key: false,
            nullable: false,
            fk_target: None,
            noform: false,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: vec![],
            choice_labels: vec![],
            default: String::new(),
            is_multichoice: false,
            unique: false,
            on_delete: crate::orm::FkAction::NoAction,
            on_update: crate::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: ::core::option::Option::None,
            slug_from: ::core::option::Option::None,
        };

        for backend in ["sqlite", "postgres"] {
            let mut stmt = Table::create();
            stmt.table(Alias::new("u"));
            for col in [&id, &username, &email] {
                let mut def = if backend == "sqlite" {
                    build_column_def_sqlite(col)
                } else {
                    build_column_def_postgres(col)
                };
                stmt.col(&mut def);
            }
            let sql = if backend == "sqlite" {
                stmt.to_string(SqliteQueryBuilder)
            } else {
                stmt.to_string(PostgresQueryBuilder)
            };

            // UNIQUE on the explicitly-marked non-PK column.
            assert!(
                sql.contains("\"username\"") && sql.to_uppercase().contains("UNIQUE"),
                "{backend}: expected UNIQUE on username; got: {sql}",
            );
            // No UNIQUE on `email` (flag false).
            let email_clause = sql
                .split("\"email\"")
                .nth(1)
                .unwrap_or_default()
                .split(',')
                .next()
                .unwrap_or_default();
            assert!(
                !email_clause.to_uppercase().contains("UNIQUE"),
                "{backend}: email should not be UNIQUE; clause: {email_clause}",
            );
            // PK still PK; the redundant UNIQUE flag is dropped so we
            // don't double up the constraint.
            let id_clause = sql
                .split("\"id\"")
                .nth(1)
                .unwrap_or_default()
                .split(',')
                .next()
                .unwrap_or_default();
            assert!(
                id_clause.to_uppercase().contains("PRIMARY KEY"),
                "{backend}: id should still be PRIMARY KEY; clause: {id_clause}",
            );
            assert!(
                !id_clause.to_uppercase().contains("UNIQUE"),
                "{backend}: PK column should not also carry UNIQUE; clause: {id_clause}",
            );
        }
    }

    /// Gap #68: `on_delete` / `on_update` lift to the `REFERENCES`
    /// tail in DDL. `NoAction` emits no clause (the SQL default);
    /// any other variant emits `ON DELETE <kw>` / `ON UPDATE <kw>`
    /// on both backends. The clause goes inside the same `extra(...)`
    /// string that already carries `REFERENCES "<target>"("id")` —
    /// the test asserts the full tail shape so a future refactor
    /// that splits the FK rendering won't silently regress.
    #[test]
    fn fk_action_lifts_to_references_clause_on_both_backends() {
        use sea_query::{Alias, PostgresQueryBuilder, SqliteQueryBuilder, Table};

        // Need an FK target table; the DDL renderer looks up the
        // PK column type for `auth_user` via `fk_target_pk`.
        // Using "post" since it's already registered as a real
        // Model in the lib (resolves to BigInt id).
        let plain_fk = Column {
            name: "owner_id".into(),
            ty: SqlType::ForeignKey,
            primary_key: false,
            nullable: false,
            fk_target: Some("post".into()),
            noform: false,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: vec![],
            choice_labels: vec![],
            default: String::new(),
            is_multichoice: false,
            unique: false,
            on_delete: crate::orm::FkAction::NoAction,
            on_update: crate::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: ::core::option::Option::None,
            slug_from: ::core::option::Option::None,
        };
        let cascade_fk = Column {
            on_delete: crate::orm::FkAction::Cascade,
            on_update: crate::orm::FkAction::Cascade,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            ..plain_fk.clone()
        };
        let restrict_fk = Column {
            on_delete: crate::orm::FkAction::Restrict,
            ..plain_fk.clone()
        };
        let set_null_fk = Column {
            nullable: true,
            on_delete: crate::orm::FkAction::SetNull,
            ..plain_fk.clone()
        };

        for backend in ["sqlite", "postgres"] {
            let render_one = |col: &Column| -> String {
                let mut stmt = Table::create();
                stmt.table(Alias::new("t"));
                let mut def = if backend == "sqlite" {
                    build_column_def_sqlite(col)
                } else {
                    build_column_def_postgres(col)
                };
                stmt.col(&mut def);
                if backend == "sqlite" {
                    stmt.to_string(SqliteQueryBuilder)
                } else {
                    stmt.to_string(PostgresQueryBuilder)
                }
            };

            // NoAction → REFERENCES with no tail clauses.
            let sql = render_one(&plain_fk);
            assert!(
                sql.contains("REFERENCES")
                    && !sql.to_uppercase().contains("ON DELETE")
                    && !sql.to_uppercase().contains("ON UPDATE"),
                "{backend}: NoAction should emit REFERENCES alone; got: {sql}",
            );

            // Cascade on both ON DELETE and ON UPDATE.
            let sql = render_one(&cascade_fk);
            assert!(
                sql.to_uppercase().contains("ON DELETE CASCADE")
                    && sql.to_uppercase().contains("ON UPDATE CASCADE"),
                "{backend}: Cascade should emit both clauses; got: {sql}",
            );

            // Restrict on ON DELETE only; ON UPDATE is NoAction so
            // no clause appears.
            let sql = render_one(&restrict_fk);
            assert!(
                sql.to_uppercase().contains("ON DELETE RESTRICT"),
                "{backend}: Restrict missing; got: {sql}",
            );
            assert!(
                !sql.to_uppercase().contains("ON UPDATE"),
                "{backend}: ON UPDATE shouldn't appear for NoAction; got: {sql}",
            );

            // SET NULL renders verbatim (two-word keyword).
            let sql = render_one(&set_null_fk);
            assert!(
                sql.to_uppercase().contains("ON DELETE SET NULL"),
                "{backend}: SET NULL missing; got: {sql}",
            );
        }
    }

    /// Gap #65 follow-up: the diff engine detects changes to *every*
    /// schema-meaningful field, not just `ty` and `nullable`. Each
    /// branch builds a baseline column, mutates one field, runs
    /// `diff_columns`, and asserts an `AlterColumn` op is produced.
    /// Catches the regression where toggling `unique` or `on_delete`
    /// would silently leave the table unchanged.
    #[test]
    fn diff_detects_all_schema_meaningful_field_changes() {
        fn baseline() -> Column {
            Column {
                name: "x".into(),
                ty: SqlType::Text,
                primary_key: false,
                nullable: false,
                fk_target: None,
                noform: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: vec![],
                choice_labels: vec![],
                default: String::new(),
                is_multichoice: false,
                unique: false,
                on_delete: crate::orm::FkAction::NoAction,
                on_update: crate::orm::FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                widget: None,
                supported_backends: Vec::new(),
                min: None,
                max: None,
                text_format: ::core::option::Option::None,
                slug_from: ::core::option::Option::None,
            }
        }
        fn meta_with(col: Column) -> ModelMeta {
            ModelMeta {
                name: "M".into(),
                table: "m".into(),
                fields: vec![col],
                display: "M".into(),
                icon: "database".into(),
                database: None,
                singleton: false,
                unique_together: Vec::new(),
                indexes: Vec::new(),
                ordering: Vec::new(),
                m2m_relations: Vec::new(),
                soft_delete: false,
            }
        }
        let prev = meta_with(baseline());
        let mutations: Vec<(&str, fn(&mut Column))> = vec![
            ("unique", |c| c.unique = true),
            ("default", |c| c.default = "hello".into()),
            ("choices", |c| {
                c.choices = vec!["a".into(), "b".into()];
                c.choice_labels = vec!["A".into(), "B".into()];
            }),
            ("nullable", |c| c.nullable = true),
        ];
        for (label, mutate) in mutations {
            let mut col = baseline();
            mutate(&mut col);
            let current = meta_with(col);
            let ops = diff_columns("M", &prev, &current).expect("diff should succeed");
            assert!(
                !ops.is_empty(),
                "{label}: diff should produce at least one op; got none",
            );
            assert!(
                ops.iter()
                    .any(|op| matches!(op, Operation::AlterColumn { column, .. } if column == "x")),
                "{label}: expected AlterColumn on `x`; got: {ops:?}",
            );
        }
    }

    /// Gap #65 follow-up: the Postgres `AlterColumn` render handles
    /// the new diff types (unique, default, choices, FK actions)
    /// with native `ALTER TABLE ... ADD/DROP CONSTRAINT` /
    /// `SET/DROP DEFAULT` statements. SQLite is unchanged — the
    /// rebuild dance already swallows any column metadata change.
    #[test]
    fn postgres_alter_column_renders_constraint_changes() {
        let baseline = Column {
            name: "x".into(),
            ty: SqlType::Text,
            primary_key: false,
            nullable: false,
            fk_target: None,
            noform: false,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: vec![],
            choice_labels: vec![],
            default: String::new(),
            is_multichoice: false,
            unique: false,
            on_delete: crate::orm::FkAction::NoAction,
            on_update: crate::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: ::core::option::Option::None,
            slug_from: ::core::option::Option::None,
        };

        // unique false → true: emit ADD CONSTRAINT ... UNIQUE
        let mut new = baseline.clone();
        new.unique = true;
        let stmts = render_alter_column_postgres("m", "x", &[new], Some(&[baseline.clone()]));
        let joined = stmts.join("\n");
        assert!(
            joined.contains("ADD CONSTRAINT") && joined.contains("UNIQUE"),
            "unique add: expected ADD CONSTRAINT UNIQUE; got: {joined}",
        );

        // unique true → false: emit DROP CONSTRAINT ... IF EXISTS
        let prev_unique = Column {
            unique: true,
            ..baseline.clone()
        };
        let stmts =
            render_alter_column_postgres("m", "x", &[baseline.clone()], Some(&[prev_unique]));
        let joined = stmts.join("\n");
        assert!(
            joined.contains("DROP CONSTRAINT IF EXISTS"),
            "unique drop: expected DROP CONSTRAINT IF EXISTS; got: {joined}",
        );

        // default empty → "hello": SET DEFAULT 'hello'
        let mut new = baseline.clone();
        new.default = "hello".into();
        let stmts = render_alter_column_postgres("m", "x", &[new], Some(&[baseline.clone()]));
        let joined = stmts.join("\n");
        assert!(
            joined.contains("SET DEFAULT 'hello'"),
            "default set: expected SET DEFAULT; got: {joined}",
        );

        // default "hello" → empty: DROP DEFAULT
        let prev_default = Column {
            default: "hello".into(),
            ..baseline.clone()
        };
        let stmts =
            render_alter_column_postgres("m", "x", &[baseline.clone()], Some(&[prev_default]));
        let joined = stmts.join("\n");
        assert!(
            joined.contains("DROP DEFAULT"),
            "default drop: expected DROP DEFAULT; got: {joined}",
        );

        // FK on_delete change → DROP + readd FK with new clause
        let fk_baseline = Column {
            ty: SqlType::ForeignKey,
            fk_target: Some("other".into()),
            ..baseline.clone()
        };
        let fk_cascade = Column {
            on_delete: crate::orm::FkAction::Cascade,
            ..fk_baseline.clone()
        };
        let stmts = render_alter_column_postgres("m", "x", &[fk_cascade], Some(&[fk_baseline]));
        let joined = stmts.join("\n");
        assert!(
            joined.contains("DROP CONSTRAINT IF EXISTS")
                && joined.contains("FOREIGN KEY")
                && joined.contains("ON DELETE CASCADE"),
            "FK cascade add: expected drop+readd with ON DELETE CASCADE; got: {joined}",
        );
    }

    /// IMP-2 from bugs/tests/testBugs.md: a `#[umbra(default = "true")]`
    /// on a boolean column used to land as `DEFAULT 'true'` on
    /// SQLite, which decode-fails on read (column type is INTEGER,
    /// the stored TEXT can't deserialize as `bool`). The SQLite
    /// renderer now maps the string to `1` / `0`.
    #[test]
    fn sqlite_bool_default_translates_to_integer_literal() {
        use sea_query::{Alias, SqliteQueryBuilder, Table};

        let bool_col = Column {
            name: "is_active".into(),
            ty: SqlType::Boolean,
            primary_key: false,
            nullable: false,
            fk_target: None,
            noform: false,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: vec![],
            choice_labels: vec![],
            default: "true".into(),
            is_multichoice: false,
            unique: false,
            on_delete: crate::orm::FkAction::NoAction,
            on_update: crate::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: ::core::option::Option::None,
            slug_from: ::core::option::Option::None,
        };
        let mut stmt = Table::create();
        stmt.table(Alias::new("t"));
        let mut def = build_column_def_sqlite(&bool_col);
        stmt.col(&mut def);
        let sql = stmt.to_string(SqliteQueryBuilder);
        assert!(
            sql.contains("DEFAULT 1") && !sql.contains("DEFAULT 'true'"),
            "bool default 'true' on sqlite should render as DEFAULT 1; got: {sql}",
        );

        // "false" → 0
        let mut bool_col_false = bool_col.clone();
        bool_col_false.default = "false".into();
        let mut stmt = Table::create();
        stmt.table(Alias::new("t"));
        let mut def = build_column_def_sqlite(&bool_col_false);
        stmt.col(&mut def);
        let sql = stmt.to_string(SqliteQueryBuilder);
        assert!(
            sql.contains("DEFAULT 0") && !sql.contains("DEFAULT 'false'"),
            "bool default 'false' on sqlite should render as DEFAULT 0; got: {sql}",
        );

        // Non-bool columns are untouched (text default stays
        // single-quoted literal).
        let text_col = Column {
            name: "label".into(),
            ty: SqlType::Text,
            default: "hello".into(),
            ..bool_col.clone()
        };
        let mut stmt = Table::create();
        stmt.table(Alias::new("t"));
        let mut def = build_column_def_sqlite(&text_col);
        stmt.col(&mut def);
        let sql = stmt.to_string(SqliteQueryBuilder);
        assert!(
            sql.contains("DEFAULT 'hello'"),
            "text default should stay quoted; got: {sql}",
        );
    }

    /// BUG-4 from bugs/tests/testBugs.md: `#[umbra(index)]` lifts
    /// to a `CREATE INDEX IF NOT EXISTS idx_<table>_<col>` statement
    /// alongside the `CREATE TABLE`. The index is skipped on PK
    /// and UNIQUE columns (those are already indexed by the
    /// constraint).
    #[test]
    fn index_attribute_emits_create_index_alongside_create_table() {
        let id = Column {
            name: "id".into(),
            ty: SqlType::BigInt,
            primary_key: true,
            nullable: false,
            fk_target: None,
            noform: false,
            noedit: false,
            is_string_repr: false,
            max_length: 0,
            choices: vec![],
            choice_labels: vec![],
            default: String::new(),
            is_multichoice: false,
            unique: false,
            on_delete: crate::orm::FkAction::NoAction,
            on_update: crate::orm::FkAction::NoAction,
            // PK with index=true; the renderer should skip the
            // extra CREATE INDEX because the PK constraint
            // already covers it.
            index: true,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: ::core::option::Option::None,
            slug_from: ::core::option::Option::None,
        };
        let slug = Column {
            name: "slug".into(),
            ty: SqlType::Text,
            primary_key: false,
            nullable: false,
            index: true,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            ..id.clone()
        };
        let title = Column {
            name: "title".into(),
            ty: SqlType::Text,
            primary_key: false,
            nullable: false,
            index: false,
            auto_now_add: false,
            auto_now: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            ..id.clone()
        };
        let op = Operation::CreateTable {
            table: "post".into(),
            columns: vec![id, slug, title],
            unique_together: Vec::new(),
            indexes: Vec::new(),
        };

        for backend in ["sqlite", "postgres"] {
            let stmts = render_operation_for(&op, backend);
            assert!(
                stmts
                    .iter()
                    .any(|s| s.to_uppercase().contains("CREATE TABLE")),
                "{backend}: expected a CREATE TABLE; got: {stmts:?}",
            );
            let index_stmts: Vec<_> = stmts
                .iter()
                .filter(|s| s.to_uppercase().contains("CREATE INDEX"))
                .collect();
            assert_eq!(
                index_stmts.len(),
                1,
                "{backend}: expected exactly one CREATE INDEX (on `slug`); got {index_stmts:?}",
            );
            let ix = index_stmts[0];
            assert!(
                ix.contains("\"idx_post_slug\"") && ix.contains("(\"slug\")"),
                "{backend}: index should target post(slug); got: {ix}",
            );
            assert!(
                ix.to_uppercase().contains("IF NOT EXISTS"),
                "{backend}: should be idempotent via IF NOT EXISTS; got: {ix}",
            );
        }
    }

    /// Regression: adding an `auto_now` / `auto_now_add` column to an
    /// existing populated table.
    ///
    ///   - SQLite: a 2-statement sequence (nullable ADD + UPDATE
    ///     backfill) since SQLite refuses non-constant defaults in
    ///     ALTER. The column ends up nullable at the DB level;
    ///     Rust still enforces non-null at the type level.
    ///   - Postgres: a single ALTER with `DEFAULT now()` — Postgres
    ///     allows the non-constant default and backfills inline.
    #[test]
    fn auto_now_add_column_renders_safe_backfill_per_backend() {
        for (label, auto_now, auto_now_add) in
            [("auto_now", true, false), ("auto_now_add", false, true)]
        {
            let col = Column {
                name: "updated_at".to_string(),
                ty: SqlType::Timestamptz,
                primary_key: false,
                nullable: false,
                fk_target: None,
                noform: false,
                noedit: false,
                is_string_repr: false,
                max_length: 0,
                choices: Vec::new(),
                choice_labels: Vec::new(),
                default: String::new(),
                is_multichoice: false,
                unique: false,
                on_delete: crate::orm::FkAction::NoAction,
                on_update: crate::orm::FkAction::NoAction,
                index: false,
                auto_now_add,
                auto_now,
                help: String::new(),
                example: String::new(),
                widget: None,
                supported_backends: Vec::new(),
                min: None,
                max: None,
                text_format: None,
                slug_from: None,
            };

            // SQLite: the AddColumn op must produce TWO statements:
            // an ADD COLUMN nullable + an UPDATE backfill. The ADD
            // must NOT carry `NOT NULL` (otherwise SQLite rejects
            // it on the populated rows), and must NOT carry a
            // DEFAULT (otherwise SQLite rejects the non-constant).
            let op = Operation::AddColumn {
                table: "customer".to_string(),
                column: col.clone(),
            };
            let stmts = render_operation_sqlite(&op);
            assert_eq!(
                stmts.len(),
                2,
                "{label} SQLite: must emit ADD + UPDATE, got: {stmts:?}",
            );
            let add_sql = stmts[0].to_uppercase();
            assert!(
                add_sql.contains("ADD COLUMN"),
                "{label} SQLite: first stmt must be ADD COLUMN, got: {}",
                stmts[0],
            );
            assert!(
                !add_sql.contains("NOT NULL"),
                "{label} SQLite: ADD COLUMN must be nullable (NOT NULL = SQLite reject), got: {}",
                stmts[0],
            );
            assert!(
                !add_sql.contains("DEFAULT"),
                "{label} SQLite: ADD COLUMN must omit DEFAULT (non-constant = SQLite reject), got: {}",
                stmts[0],
            );
            let backfill_sql = &stmts[1];
            assert!(
                backfill_sql.contains("UPDATE") && backfill_sql.contains("datetime('now')"),
                "{label} SQLite: second stmt must be backfill UPDATE, got: {backfill_sql}",
            );

            // Postgres: single ALTER with NOT NULL + DEFAULT now().
            let pstmts = render_operation_postgres(&op);
            assert_eq!(
                pstmts.len(),
                1,
                "{label} Postgres: single statement suffices, got: {pstmts:?}",
            );
            let p = &pstmts[0];
            assert!(
                p.to_lowercase().contains("default now()"),
                "{label} Postgres: expected DEFAULT now() in ALTER, got: {p}",
            );
            assert!(
                p.to_uppercase().contains("NOT NULL"),
                "{label} Postgres: keeps NOT NULL (Postgres allows non-constant defaults), got: {p}",
            );
        }
    }
}
