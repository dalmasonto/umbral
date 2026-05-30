//! The boot-time system check framework.
//!
//! The `App::builder().build()` lifecycle runs the system check as
//! phase 4 (per spec 01 §Lifecycle phases). The framework's built-in
//! checks live here; plugin-contributed checks land at M7 via
//! `Plugin::system_checks()`.
//!
//! At M4 the only check that's meaningful without a model registry or
//! Plugin walk is [`settings_required`] — it verifies that production
//! `Settings` have safe values (most importantly that `secret_key`
//! isn't left at the insecure dev default). More checks (`field.
//! backend`, `model.pk.present`, `model.table.unique`, `route.
//! collision`, `plugin.dependency.*`) land alongside the registries
//! they need: M5's migration engine for the model walk, M7's Plugin
//! contract for plugin/route walks.
//!
//! See `docs/specs/05-backends-and-system-check.md` for the full
//! built-in catalogue.

use crate::backend::DatabaseBackend;
use crate::settings::Settings;

/// One named system check.
///
/// Built-in checks live in `framework_checks()`; plugin checks return
/// from `Plugin::system_checks()` (M7). Each check is a function pointer
/// that takes the [`CheckContext`] and produces zero or more
/// [`SystemCheckFinding`]s.
pub struct SystemCheck {
    /// Stable identifier, dot-delimited. Used in error reports and so
    /// users can grep for failures: `field.backend`, `settings.required`,
    /// etc.
    pub id: &'static str,
    /// The check function.
    pub run: fn(&CheckContext<'_>) -> Vec<SystemCheckFinding>,
}

/// Context available to a system check at boot.
///
/// Holds references to everything a check might consult: the active
/// backend, the validated settings. The model list (M5) and plugin
/// registry (M7) get added when they exist.
pub struct CheckContext<'a> {
    /// The active database backend.
    pub backend: &'a dyn DatabaseBackend,
    /// The runtime settings, post-load, pre-publish.
    pub settings: &'a Settings,
}

/// One issue surfaced by a system check.
#[derive(Debug)]
pub struct SystemCheckFinding {
    /// The id of the check that produced this finding. Matches the
    /// owning [`SystemCheck::id`].
    pub check_id: &'static str,
    /// Whether this is an error (blocks boot) or just a warning (logged
    /// and proceeds).
    pub severity: Severity,
    /// The thing that's broken: which model, which field, which plugin,
    /// which route, or just "the settings."
    pub location: CheckLocation,
    /// A user-facing one-line message.
    pub message: String,
    /// Optional follow-up: what the user should change to fix it.
    pub hint: Option<String>,
}

/// Severity of a system-check finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Block boot. `AppBuilder::build()` returns
    /// `BuildError::SystemCheckFailed`.
    Error,
    /// Log via `tracing::warn!`, continue booting.
    Warning,
}

/// Where in the framework a finding originates. The variants grow as
/// the registries do.
#[derive(Debug, Clone)]
pub enum CheckLocation {
    /// A field on a model. M5/M7 work.
    Field {
        plugin: &'static str,
        model: &'static str,
        field: &'static str,
    },
    /// A model. M5/M7 work.
    Model {
        plugin: &'static str,
        model: &'static str,
    },
    /// A plugin's own metadata. M7 work.
    Plugin { plugin: &'static str },
    /// A registered route. M7 work.
    Route { path: String },
    /// The settings as a whole.
    Settings,
}

/// Return the framework's built-in checks.
///
/// Body filled in by the M4 fan-out subagent B. The function exists in
/// the scaffold so `app::build()` can call it for phase 4; subagent B
/// populates the returned vec with the `settings.required` check (and
/// any others that fit at M4).
pub fn framework_checks() -> Vec<SystemCheck> {
    Vec::new()
}

/// Run every check in `checks` against `ctx`, accumulate findings, and
/// partition into errors vs warnings. Used by `AppBuilder::build()`
/// phase 4 and by tests.
///
/// Returns the full findings list; callers decide what to do with the
/// Error-severity entries (the builder turns them into
/// `BuildError::SystemCheckFailed`).
pub fn run_all(ctx: &CheckContext<'_>, checks: &[SystemCheck]) -> Vec<SystemCheckFinding> {
    let mut findings = Vec::new();
    for check in checks {
        findings.extend((check.run)(ctx));
    }
    findings
}
