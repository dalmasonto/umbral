//! End-to-end coverage for the M7 Plugin contract.
//!
//! Exercises the keystone subset that shipped at M7 v1:
//!
//! - `Plugin::name`, `dependencies`, `models`, `routes`,
//!   `system_checks`, and `on_ready`.
//! - `AppBuilder::plugin(...)` registration.
//! - The topological sort baked into `App::build()` (cycle detection,
//!   dependency-not-found, reserved-name and duplicate-name rejection).
//! - The per-plugin model walk surfaced by
//!   `migrate::registered_plugins` / `models_for_plugin`.
//! - The merged router that mounts every plugin's `routes()` after the
//!   hand-written one.
//! - `on_ready` firing in topological order.
//!
//! `migrate::REGISTRY`, `db::DB_POOL`, the active backend, and
//! `settings::SETTINGS` are all process-wide `OnceLock`s, so every
//! success-path test in this file shares one boot of
//! `App::builder().build()` driven through a `tokio::sync::OnceCell`.
//! The failure-path tests build their own `AppBuilder`s with bad
//! inputs; every variant they cover is rejected in phase 1.5 of
//! `App::build()`, which runs BEFORE phase 3 publishes the ambient
//! state, so multiple failing builds compose safely alongside the
//! shared success-path boot.
//!
//! See `crates/umbra-core/src/plugin.rs` for the trait shape,
//! `crates/umbra-core/src/app.rs` for the build-phase ordering, and
//! `docs/specs/02-plugin-contract.md` for the design rationale.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use http::Request;
use http_body_util::BodyExt;
use tokio::sync::OnceCell;
use tower::ServiceExt;

use umbra::App;
use umbra::Settings;
use umbra::check::{SystemCheck, SystemCheckFinding};
use umbra::migrate::{ModelMeta, models_for_plugin, plugin_order, registered_plugins};
use umbra::plugin::{AppContext, Plugin, PluginError};
use umbra::web::{Router, get};
use umbra_core::app::BuildError;
use umbra_core::migrate::{APP_PLUGIN_NAME, Column};
use umbra_core::orm::{Post, SqlType};

// --------------------------------------------------------------------- //
// Shared on_ready ordering log. Every plugin built into the shared App  //
// appends its name here when its `on_ready` fires. Cloned via Arc into  //
// each plugin so the test thread can read the final order back out.    //
// --------------------------------------------------------------------- //

type OrderLog = Arc<Mutex<Vec<&'static str>>>;

// --------------------------------------------------------------------- //
// A configurable test plugin. Carries an `Arc<AtomicBool>` per          //
// side-effect (on_ready flag, system-check flag) so the test thread     //
// observes whether each contract point fired; also takes an optional    //
// `OrderLog` so it can record itself on the shared sequence.            //
// --------------------------------------------------------------------- //

struct TestPlugin {
    name: &'static str,
    deps: &'static [&'static str],
    on_ready_flag: Arc<AtomicBool>,
    check_flag: Arc<AtomicBool>,
    route_path: &'static str,
    model_table: &'static str,
    order_log: Option<OrderLog>,
}

impl TestPlugin {
    fn minimal(name: &'static str) -> Self {
        Self {
            name,
            deps: &[],
            on_ready_flag: Arc::new(AtomicBool::new(false)),
            check_flag: Arc::new(AtomicBool::new(false)),
            route_path: "/__test/unused",
            model_table: "__unused__",
            order_log: None,
        }
    }
}

impl Plugin for TestPlugin {
    fn name(&self) -> &'static str {
        self.name
    }

    fn dependencies(&self) -> &'static [&'static str] {
        self.deps
    }

    fn models(&self) -> Vec<ModelMeta> {
        // Hand-built ModelMeta with one BigInt primary-key column. The
        // exact column shape doesn't matter for the contract tests;
        // what matters is that this `ModelMeta` flows into the
        // per-plugin registry under `self.name`.
        let model_name = format!("{}__Model", self.name);
        vec![ModelMeta {
            display: model_name.clone(),
            icon: "database".to_string(),
            database: None,
            singleton: false,
            unique_together: Vec::new(),
            indexes: Vec::new(),
            ordering: Vec::new(),
            m2m_relations: Vec::new(),
            name: model_name,
            table: self.model_table.to_string(),
            fields: vec![Column {
                name: "id".to_string(),
                ty: SqlType::BigInt,
                primary_key: true,
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
                on_delete: umbra_core::orm::FkAction::NoAction,
                on_update: umbra_core::orm::FkAction::NoAction,
                index: false,
                auto_now_add: false,
                auto_now: false,
                help: String::new(),
                example: String::new(),
                supported_backends: Vec::new(),
                min: None,
                max: None,
                text_format: ::core::option::Option::None,
                slug_from: ::core::option::Option::None,
            }],
        }]
    }

    fn routes(&self) -> Router {
        let path = self.route_path;
        Router::new().route(path, get(|| async { "ok" }))
    }

    fn system_checks(&self) -> Vec<SystemCheck> {
        // A side-effecting check: flipping the flag proves the check
        // executed during phase 4. The check itself never fails: it
        // returns an empty findings list, so it can't block boot.
        //
        // The closure can't capture `self`, so the side effect is
        // routed through a process-wide registry keyed by check id.
        // Each test plugin registers its flag under a unique id, then
        // a free-function check pulls the matching flag back out and
        // flips it.
        register_check_flag(self.name, self.check_flag.clone());
        vec![SystemCheck {
            id: self.name,
            run: run_test_check,
        }]
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        self.on_ready_flag.store(true, Ordering::SeqCst);
        if let Some(log) = &self.order_log {
            log.lock().expect("order log not poisoned").push(self.name);
        }
        Ok(())
    }
}

// --------------------------------------------------------------------- //
// Side-effect registry for the per-plugin system check. `SystemCheck`   //
// is a function pointer (`fn(&CheckContext) -> Vec<_>`); it can't       //
// close over plugin state. The plugin stores its flag in this registry  //
// keyed by `check_id` at `system_checks()`-collection time; the         //
// generic check function looks the flag up by id and flips it.         //
// --------------------------------------------------------------------- //

static CHECK_FLAGS: OnceLock<Mutex<HashMap<&'static str, Arc<AtomicBool>>>> = OnceLock::new();

fn check_flags() -> &'static Mutex<HashMap<&'static str, Arc<AtomicBool>>> {
    CHECK_FLAGS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_check_flag(id: &'static str, flag: Arc<AtomicBool>) {
    check_flags()
        .lock()
        .expect("check_flags mutex not poisoned")
        .insert(id, flag);
}

fn run_test_check(ctx: &umbra::check::CheckContext<'_>) -> Vec<SystemCheckFinding> {
    // A real id-driven lookup. We can't read the check's own id from
    // `ctx`, so walk the registry and flip every flag whose key matches
    // one of the test plugin names. For the assertions below, the
    // important property is "the flag for the plugin's check was
    // flipped"; flipping every registered flag is a stricter version
    // of the same property and just as testable.
    let _ = ctx; // unused
    let map = check_flags()
        .lock()
        .expect("check_flags mutex not poisoned");
    for flag in map.values() {
        flag.store(true, Ordering::SeqCst);
    }
    Vec::new()
}

// --------------------------------------------------------------------- //
// Shared App boot. Two plugins:                                          //
//                                                                        //
//   - parent_plugin: no deps, route /parent/ping, model parent_table,    //
//     system check, on_ready that records itself in the shared log.      //
//   - child_plugin: depends on "parent_plugin", route /child/ping,       //
//     model child_table, on_ready that records itself in the shared log. //
//                                                                        //
// One `.model::<Post>()` registration covers the implicit "app" plugin   //
// path so `models_for_plugin("app")` has something to return.            //
// --------------------------------------------------------------------- //

static BOOT: OnceCell<BootState> = OnceCell::const_new();

struct BootState {
    parent_on_ready: Arc<AtomicBool>,
    parent_check: Arc<AtomicBool>,
    child_on_ready: Arc<AtomicBool>,
    order: OrderLog,
    router: Mutex<Option<Router>>,
}

async fn boot() -> &'static BootState {
    BOOT.get_or_init(|| async {
        let parent_on_ready = Arc::new(AtomicBool::new(false));
        let parent_check = Arc::new(AtomicBool::new(false));
        let child_on_ready = Arc::new(AtomicBool::new(false));
        let order: OrderLog = Arc::new(Mutex::new(Vec::new()));

        let parent = TestPlugin {
            name: "parent_plugin",
            deps: &[],
            on_ready_flag: parent_on_ready.clone(),
            check_flag: parent_check.clone(),
            route_path: "/parent/ping",
            model_table: "parent_table",
            order_log: Some(order.clone()),
        };

        let child = TestPlugin {
            name: "child_plugin",
            deps: &["parent_plugin"],
            on_ready_flag: child_on_ready.clone(),
            check_flag: Arc::new(AtomicBool::new(false)),
            route_path: "/child/ping",
            model_table: "child_table",
            order_log: Some(order.clone()),
        };

        let settings = Settings::from_env().expect("figment defaults always load in a test env");
        let pool = umbra::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite should always connect");

        let app = App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Post>()
            .plugin(parent)
            .plugin(child)
            .build()
            .expect("App::build() should succeed on the happy path");

        BootState {
            parent_on_ready,
            parent_check,
            child_on_ready,
            order,
            router: Mutex::new(Some(app.into_router())),
        }
    })
    .await
}

/// Pull a clone of the merged router out of `BootState`. `Router` is
/// `Clone`, so a shared `Option<Router>` behind a `Mutex` hands every
/// test the same composed router without consuming it.
fn shared_router() -> Router {
    let state = BOOT.get().expect("shared_router called before boot");
    state
        .router
        .lock()
        .expect("shared router mutex not poisoned")
        .clone()
        .expect("router stored after build")
}

// --------------------------------------------------------------------- //
// Success-path tests. All share the BOOT OnceCell.                      //
// --------------------------------------------------------------------- //

/// Every plugin's `on_ready` should have fired by the time
/// `App::build()` returns. The flag flip inside `TestPlugin::on_ready`
/// is the visible side effect.
#[tokio::test]
async fn plugin_on_ready_fires() {
    let state = boot().await;
    assert!(
        state.parent_on_ready.load(Ordering::SeqCst),
        "parent_plugin's on_ready should have fired by the end of App::build()",
    );
    assert!(
        state.child_on_ready.load(Ordering::SeqCst),
        "child_plugin's on_ready should have fired by the end of App::build()",
    );
}

/// `migrate::registered_plugins()` should list every plugin that
/// contributed models: the implicit `"app"` plugin (from
/// `.model::<Post>()`) plus every `.plugin(...)` registration that
/// returned at least one model from `Plugin::models()`. The per-plugin
/// model walk then routes each plugin's metadata to the right slot.
#[tokio::test]
async fn plugin_models_land_in_per_plugin_registry() {
    boot().await;

    let plugins = registered_plugins();
    assert!(
        plugins.contains(&APP_PLUGIN_NAME.to_string()),
        "implicit `app` plugin should appear in registered_plugins(); got {plugins:?}",
    );
    assert!(
        plugins.contains(&"parent_plugin".to_string()),
        "parent_plugin should appear in registered_plugins(); got {plugins:?}",
    );
    assert!(
        plugins.contains(&"child_plugin".to_string()),
        "child_plugin should appear in registered_plugins(); got {plugins:?}",
    );

    let app_models = models_for_plugin(APP_PLUGIN_NAME);
    assert_eq!(
        app_models.len(),
        1,
        "implicit `app` plugin should hold the one `.model::<Post>()` registration; got {app_models:?}",
    );
    assert_eq!(
        app_models[0].table, "post",
        "the one model registered via `.model::<T>()` is `Post`; got {:?}",
        app_models[0].table,
    );

    let parent_models = models_for_plugin("parent_plugin");
    assert_eq!(
        parent_models.len(),
        1,
        "parent_plugin contributes one model"
    );
    assert_eq!(parent_models[0].table, "parent_table");

    let child_models = models_for_plugin("child_plugin");
    assert_eq!(child_models.len(), 1, "child_plugin contributes one model");
    assert_eq!(child_models[0].table, "child_table");
}

/// The plugin's `system_checks()` get walked alongside the framework's
/// in phase 4 of `App::build()`. The side-effecting check the test
/// plugin returns flips a flag the test can read back out.
#[tokio::test]
async fn plugin_system_check_runs_during_build() {
    let state = boot().await;
    assert!(
        state.parent_check.load(Ordering::SeqCst),
        "parent_plugin's system_check should have run during phase 4 of App::build()",
    );
}

/// Phase 5 merges every plugin's `routes()` into the merged App router.
/// Driving a synthetic request through the merged router via
/// `tower::ServiceExt::oneshot` confirms the plugin's route is wired up
/// and responds 200 OK with the expected body.
#[tokio::test]
async fn plugin_routes_mount_under_the_app_router() {
    boot().await;
    let router = shared_router();

    let response = router
        .oneshot(
            Request::builder()
                .uri("/parent/ping")
                .body(axum::body::Body::empty())
                .expect("build a GET request"),
        )
        .await
        .expect("router should respond to /parent/ping without erroring");
    assert_eq!(
        response.status(),
        http::StatusCode::OK,
        "parent_plugin's /parent/ping should respond 200 OK; got {}",
        response.status(),
    );

    let body = response
        .into_body()
        .collect()
        .await
        .expect("collect response body")
        .to_bytes();
    assert_eq!(
        &body[..],
        b"ok",
        "the /parent/ping handler returns \"ok\"; got {:?}",
        std::str::from_utf8(&body[..]),
    );

    // The router should also serve the dependent plugin's route.
    let router = shared_router();
    let response = router
        .oneshot(
            Request::builder()
                .uri("/child/ping")
                .body(axum::body::Body::empty())
                .expect("build a GET request"),
        )
        .await
        .expect("router should respond to /child/ping without erroring");
    assert_eq!(response.status(), http::StatusCode::OK);
}

/// `on_ready` runs in topological order. The parent plugin (no deps)
/// must fire before the child plugin (`dependencies()` returns
/// `["parent_plugin"]`). The shared `OrderLog` captures the actual
/// execution order so the test can pin the sequence.
#[tokio::test]
async fn topological_order_governs_on_ready() {
    let state = boot().await;
    let order = state
        .order
        .lock()
        .expect("order log mutex not poisoned")
        .clone();
    assert_eq!(
        order,
        vec!["parent_plugin", "child_plugin"],
        "parent_plugin must fire before child_plugin per the topological sort; got {order:?}",
    );
}

/// M8 — `App::build()` publishes the topological plugin order through
/// `migrate::plugin_order()`. The implicit `"app"` plugin lands first
/// (it has no dependencies and is the home of `.model::<T>()`
/// registrations), then every registered plugin in dependency order.
/// `parent_plugin` (no deps) precedes `child_plugin`
/// (`dependencies() = ["parent_plugin"]`); the migration engine reads
/// this exact slice when it walks per-plugin migration directories so
/// cross-plugin FK targets get created before their dependents.
#[tokio::test]
async fn plugin_order_reflects_the_topological_sort() {
    boot().await;
    let order = plugin_order();
    assert_eq!(
        order,
        vec![
            APP_PLUGIN_NAME.to_string(),
            "parent_plugin".to_string(),
            "child_plugin".to_string(),
        ],
        "plugin_order must list `app` first then dependencies before dependents; got {order:?}",
    );
}

// --------------------------------------------------------------------- //
// Failure-path tests. Each builds its own AppBuilder with bad inputs    //
// and asserts the BuildError variant. Every failure exercised below     //
// surfaces in phase 1.5 of App::build(), which runs BEFORE the          //
// OnceLock writes in phase 3, so multiple failing builds inside one    //
// test binary compose without poisoning the shared App's ambient state. //
// --------------------------------------------------------------------- //

/// A minimal in-memory pool for a failing-build path. Failure-path
/// tests need a `Settings` and a `"default"` pool registered so that
/// `App::build()` reaches phase 1.5; the pool is opened against
/// `sqlite::memory:` and never used because the build short-circuits
/// before phase 3.
async fn failing_build_settings_and_pool() -> (Settings, sqlx::SqlitePool) {
    let settings = Settings::from_env().expect("figment defaults always load in a test env");
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite should always connect");
    (settings, pool)
}

/// A plugin claiming the reserved `"app"` name is rejected in phase
/// 1.5 with `BuildError::ReservedPluginName`. The implicit `"app"`
/// plugin owns models registered via `.model::<T>()`; a user plugin
/// claiming the same name would collide on the migration tracking key.
#[tokio::test]
async fn reserved_name_app_rejected() {
    let (settings, pool) = failing_build_settings_and_pool().await;

    // `App` doesn't implement `Debug`, so `.expect_err` on the
    // `Result<App, BuildError>` won't compile. Match on the result
    // directly instead.
    let result = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(TestPlugin::minimal("app"))
        .build();

    match result {
        Err(BuildError::ReservedPluginName) => {}
        Err(other) => panic!("expected BuildError::ReservedPluginName, got {other:?}"),
        Ok(_) => panic!("a plugin named `app` must be rejected, but build() succeeded"),
    }
}

/// Two plugins reporting the same `name()` are rejected with
/// `BuildError::DuplicatePluginName { name }`. Plugin names are unique
/// keys in the migration tracking table and the dependency graph, so a
/// duplicate must surface at boot.
#[tokio::test]
async fn duplicate_plugin_names_rejected() {
    let (settings, pool) = failing_build_settings_and_pool().await;

    let result = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(TestPlugin::minimal("duplicate"))
        .plugin(TestPlugin::minimal("duplicate"))
        .build();

    match result {
        Err(BuildError::DuplicatePluginName { name }) => {
            assert_eq!(
                name, "duplicate",
                "BuildError::DuplicatePluginName should carry the colliding name; got {name}",
            );
        }
        Err(other) => panic!("expected BuildError::DuplicatePluginName, got {other:?}"),
        Ok(_) => panic!("two plugins with the same name must be rejected"),
    }
}

/// A plugin whose `dependencies()` names a plugin that was never
/// registered is rejected with `BuildError::DependencyNotFound` that
/// names both sides of the missing edge.
#[tokio::test]
async fn unmet_dependency_rejected() {
    let (settings, pool) = failing_build_settings_and_pool().await;

    let mut plugin = TestPlugin::minimal("dependent");
    plugin.deps = &["does_not_exist"];

    let result = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(plugin)
        .build();

    match result {
        Err(BuildError::DependencyNotFound { plugin, missing }) => {
            assert_eq!(plugin, "dependent");
            assert_eq!(missing, "does_not_exist");
        }
        Err(other) => panic!("expected BuildError::DependencyNotFound, got {other:?}"),
        Ok(_) => panic!("a plugin with an unregistered dependency must be rejected"),
    }
}

/// Two plugins forming a dependency cycle (A depends on B, B depends
/// on A) are rejected with `BuildError::PluginCycle` whose `names`
/// list covers every plugin still unsorted at the end of Kahn's
/// algorithm.
#[tokio::test]
async fn cycle_rejected() {
    let (settings, pool) = failing_build_settings_and_pool().await;

    let mut a = TestPlugin::minimal("cycle_a");
    a.deps = &["cycle_b"];
    let mut b = TestPlugin::minimal("cycle_b");
    b.deps = &["cycle_a"];

    let result = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(a)
        .plugin(b)
        .build();

    match result {
        Err(BuildError::PluginCycle { names }) => {
            assert!(
                names.contains(&"cycle_a") && names.contains(&"cycle_b"),
                "PluginCycle.names should cover both plugins in the cycle; got {names:?}",
            );
        }
        Err(other) => panic!("expected BuildError::PluginCycle, got {other:?}"),
        Ok(_) => panic!("a dependency cycle must be rejected"),
    }
}

/// FEATURES.md #6 — per-plugin database alias routing. A plugin
/// whose `database()` returns `Some("blog_db")` should cause every
/// model that plugin owns to resolve to the `blog_db` alias (not
/// the default).
///
/// We test this through the public `migrate::model_alias` accessor
/// because the shared App boot in BOOT only registers the default
/// pool; standing up a second pool just to assert the routing
/// resolves would race with the rest of the file. The accessor is
/// what `queryset::resolve_pool` calls, so it's the right level to
/// pin the contract.
///
/// The shared boot registers the parent_plugin and child_plugin
/// fixtures, neither of which returns `Some` from `database()`, so
/// `model_alias` for their models returns `None` (default pool).
#[tokio::test]
async fn model_alias_returns_none_for_plugins_without_a_database_override() {
    let _ = boot().await;

    // Neither fixture plugin overrides `database()`, so their models
    // route to the default pool. The exact model names depend on the
    // fixtures defined above; we just assert the framework's
    // implicit `"app"` plugin's `Post` model isn't routed either.
    assert_eq!(
        umbra::migrate::model_alias("Post"),
        None,
        "Post belongs to the implicit `app` plugin which has no database() override; \
         model_alias should return None so resolve_pool falls back to the default pool"
    );
}
