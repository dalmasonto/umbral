//! gaps3 #40 — cross-plugin foreign-key ordering.
//!
//! `App::build()` sorts plugins with Kahn's algorithm and breaks ties with a
//! `BTreeSet`, i.e. alphabetically. When *no* plugin declares
//! `Plugin::dependencies()`, every plugin has in-degree 0, so the
//! "topological" order collapses to plain alphabetical order. `"accounts"`
//! sorts before `"auth"`, so `CREATE TABLE accounts_git_hub_account (... user
//! bigint REFERENCES auth_user(id))` ran before `auth_user` existed and the
//! first umbralrs.dev deploy died with `relation "auth_user" does not exist`.
//!
//! Any database that *already* contains the referenced table migrates fine, so
//! the whole dev loop, every test, and every incremental deploy passed. Only a
//! fresh database executes the creates in dependency order. It was a
//! prod-only, first-run-only failure.
//!
//! The schema already states the edge: a `ForeignKey<T>` field renders
//! `REFERENCES "<T::TABLE>"`, and every `Column` carries `fk_target`. So the
//! ordering can be *derived* rather than hand-declared. These tests pin that.
//!
//! `App::build()` publishes process-global state (`db::init`, the model
//! registry, `init_plugin_order`) through `OnceLock`s, so exactly ONE
//! successful build may run per test binary. This file spends it on the
//! ordering assertion; the cycle-attribution case fails in phase 1.5 before
//! any global is published and so may share the process.

use umbral::migrate::{Column, ModelMeta};
use umbral::orm::{FkAction, SqlType};
use umbral::plugin::Plugin;
use umbral::{App, BuildError};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A plugin that owns exactly one table, optionally with one physical FK into
/// another plugin's table. `deps` stays empty on purpose: the whole point is
/// that the author never declared the edge.
struct FkPlugin {
    name: &'static str,
    table: &'static str,
    fk_target: Option<&'static str>,
    /// `false` reproduces `#[umbral(db_constraint = false)]` — a logical FK
    /// that renders no `REFERENCES` clause and therefore orders nothing.
    db_constraint: bool,
    deps: &'static [&'static str],
}

impl FkPlugin {
    fn new(name: &'static str, table: &'static str) -> Self {
        Self {
            name,
            table,
            fk_target: None,
            db_constraint: true,
            deps: &[],
        }
    }

    fn fk_to(mut self, target: &'static str) -> Self {
        self.fk_target = Some(target);
        self
    }

    fn logical_fk_to(mut self, target: &'static str) -> Self {
        self.fk_target = Some(target);
        self.db_constraint = false;
        self
    }
}

fn column(name: &str, ty: SqlType, primary_key: bool) -> Column {
    Column {
        name: name.to_string(),
        ty,
        primary_key,
        nullable: false,
        fk_target: None,
        noform: false,
        privileged: false,
        private: false,
        secret: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: FkAction::NoAction,
        on_update: FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        case_insensitive: false,
        help: String::new(),
        example: String::new(),
        widget: None,
        supported_backends: Vec::new(),
        min: None,
        max: None,
        text_format: None,
        slug_from: None,
    }
}

impl Plugin for FkPlugin {
    fn name(&self) -> &'static str {
        self.name
    }

    fn dependencies(&self) -> &'static [&'static str] {
        self.deps
    }

    fn models(&self) -> Vec<ModelMeta> {
        let mut fields = vec![column("id", SqlType::BigInt, true)];
        if let Some(target) = self.fk_target {
            let mut fk = column("owner", SqlType::ForeignKey, false);
            fk.fk_target = Some(target.to_string());
            fk.db_constraint = self.db_constraint;
            fields.push(fk);
        }
        vec![ModelMeta {
            view: None,
            materialized: false,
            name: format!("{}Model", self.name),
            table: self.table.to_string(),
            fields,
            display: format!("{}Model", self.name),
            icon: "database".to_string(),
            database: None,
            singleton: false,
            unique_together: Vec::new(),
            indexes: Vec::new(),
            ordering: Vec::new(),
            m2m_relations: Vec::new(),
            soft_delete: false,
            audited: false,
            app_label: self.name.to_string(),
        }]
    }
}

async fn settings_and_pool() -> (umbral::Settings, sqlx::SqlitePool) {
    let settings = umbral::Settings::from_env().expect("figment defaults load in a test env");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    (settings, pool)
}

fn position(order: &[String], plugin: &str) -> usize {
    order
        .iter()
        .position(|p| p == plugin)
        .unwrap_or_else(|| panic!("`{plugin}` missing from plugin_order(): {order:?}"))
}

// ---------------------------------------------------------------------------
// The regression: a derived FK edge outranks the alphabetical tie-break.
// ---------------------------------------------------------------------------

/// `fkorder_accounts` FKs `fkorder_auth`'s table and declares NO dependency.
/// Alphabetically `fkorder_accounts` sorts first, which is exactly the order
/// that produced `relation "auth_user" does not exist` on the fresh Postgres.
/// The FK edge derived from the model registry must reorder them.
///
/// The logical-FK plugin (`db_constraint = false`) renders no `REFERENCES`
/// clause, so it imposes no DDL ordering and must NOT gain an edge — it stays
/// wherever the alphabetical tie-break puts it.
#[tokio::test]
async fn cross_plugin_fk_orders_target_before_dependent() {
    let (settings, pool) = settings_and_pool().await;

    let accounts = FkPlugin::new("fkorder_accounts", "fkorder_account").fk_to("fkorder_user");
    let auth = FkPlugin::new("fkorder_auth", "fkorder_user");
    // Sorts first alphabetically and points at `fkorder_user`, but only
    // logically — no physical constraint, so no ordering obligation.
    let logical = FkPlugin::new("fkorder_aaa", "fkorder_logical").logical_fk_to("fkorder_user");

    App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(accounts)
        .plugin(auth)
        .plugin(logical)
        .build()
        .expect("build succeeds: the FK edges are acyclic");

    let order = umbral::migrate::plugin_order();

    assert!(
        position(&order, "fkorder_auth") < position(&order, "fkorder_accounts"),
        "the FK target's plugin must be created before the plugin that \
         REFERENCES it; got {order:?}",
    );

    // The implicit "app" plugin stays last: app models FK into plugin tables.
    assert_eq!(
        order.last().map(String::as_str),
        Some("app"),
        "the implicit `app` plugin stays last; got {order:?}",
    );

    // A logical-only FK creates no ordering obligation, so the alphabetical
    // tie-break still puts `fkorder_aaa` ahead of its (non-)target.
    assert!(
        position(&order, "fkorder_aaa") < position(&order, "fkorder_auth"),
        "`db_constraint = false` renders no REFERENCES clause and must not \
         constrain ordering; got {order:?}",
    );
}

// ---------------------------------------------------------------------------
// Cycle attribution. Fails in phase 1.5, before any global is published.
// ---------------------------------------------------------------------------

/// Two plugins whose models FK each other cannot both be created first. Across
/// crates Cargo already forbids this (a `ForeignKey<T>` needs `T` in scope, so
/// mutual FKs would be a circular crate dependency), but two plugins defined in
/// one crate can still do it. The build must name the FK edge rather than
/// reporting a bare `PluginCycle` the author never declared.
#[tokio::test]
async fn foreign_key_cycle_is_attributed_to_the_fk_not_the_declaration() {
    let (settings, pool) = settings_and_pool().await;

    let a = FkPlugin::new("fkcycle_a", "fkcycle_a_table").fk_to("fkcycle_b_table");
    let b = FkPlugin::new("fkcycle_b", "fkcycle_b_table").fk_to("fkcycle_a_table");

    let result = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(a)
        .plugin(b)
        .build();

    match result {
        Err(BuildError::ForeignKeyCycle { edges }) => {
            assert!(
                edges.iter().any(|e| e.plugin == "fkcycle_a"
                    && e.depends_on == "fkcycle_b"
                    && e.table == "fkcycle_a_table"
                    && e.fk_target == "fkcycle_b_table"),
                "the error must name the offending column's table and target; got {edges:?}",
            );
            assert!(
                edges.iter().any(|e| e.plugin == "fkcycle_b"),
                "both sides of the cycle should be reported; got {edges:?}",
            );
        }
        Err(other) => panic!("expected BuildError::ForeignKeyCycle, got {other:?}"),
        Ok(_) => panic!("a cross-plugin foreign-key cycle has no valid CREATE TABLE order"),
    }
}

/// A cycle the author *declared* keeps reporting as `PluginCycle`. The FK-edge
/// derivation must not swallow or rename the pre-existing diagnostic.
#[tokio::test]
async fn declared_cycle_still_reports_plugin_cycle() {
    let (settings, pool) = settings_and_pool().await;

    let mut a = FkPlugin::new("declcycle_a", "declcycle_a_table");
    a.deps = &["declcycle_b"];
    let mut b = FkPlugin::new("declcycle_b", "declcycle_b_table");
    b.deps = &["declcycle_a"];

    let result = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(a)
        .plugin(b)
        .build();

    match result {
        Err(BuildError::PluginCycle { names }) => {
            assert!(
                names.contains(&"declcycle_a") && names.contains(&"declcycle_b"),
                "PluginCycle should still cover both declared plugins; got {names:?}",
            );
        }
        Err(other) => panic!("expected BuildError::PluginCycle, got {other:?}"),
        Ok(_) => panic!("a declared dependency cycle must stay rejected"),
    }
}
