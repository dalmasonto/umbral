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
use crate::settings::{Environment, Settings};

/// The insecure dev default for `Settings.secret_key`. Kept in sync with
/// `crate::settings::default_secret_key()`; that function returns an owned
/// `String`, so duplicating the literal here lets the check compare without
/// allocating.
const INSECURE_DEV_SECRET_KEY: &str = "umbra-insecure-dev-key-change-me";

/// The default `allowed_hosts` list emitted by
/// `crate::settings::default_allowed_hosts()`. Mirrored here so the
/// `settings.allowed_hosts` check can detect "still the dev default"
/// without allocating.
const DEFAULT_ALLOWED_HOSTS: &[&str] = &["localhost", "127.0.0.1"];

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
/// At M4 the catalogue is intentionally short: there's no model
/// registry (M5) or plugin walk (M7) yet, so only checks that read
/// purely from `Settings` and the active backend are meaningful. The
/// rest of the built-in catalogue (`field.backend`, `model.pk.present`,
/// `model.table.unique`, `route.collision`, `plugin.dependency.*`)
/// lands alongside the registries it needs.
pub fn framework_checks() -> Vec<SystemCheck> {
    vec![
        SystemCheck {
            id: "settings.required",
            run: settings_required,
        },
        SystemCheck {
            id: "settings.allowed_hosts",
            run: settings_allowed_hosts,
        },
        SystemCheck {
            id: "settings.log_level",
            run: settings_log_level,
        },
        SystemCheck {
            id: "backend.url_scheme.matches_active_backend",
            run: backend_url_scheme_matches_active_backend,
        },
        SystemCheck {
            id: "field.backend",
            run: field_backend,
        },
    ]
}

/// Verify that `secret_key` is not the insecure dev default. Two
/// layers:
///
/// 1. **Hard error in `Environment::Prod`** — the original check.
///    Blocks boot when the operator self-identifies as production.
/// 2. **Warning when the bind address looks public** — defense in
///    depth for the operator who forgot to set
///    `UMBRA_ENVIRONMENT=Prod`. If `bind_addr` isn't `127.0.0.1` or
///    `localhost`, the process is likely serving real network
///    traffic, and the insecure dev key is dangerous regardless of
///    the declared environment.
///
/// The boot-blocking error is intentionally reserved for explicit
/// production declarations — surprising people with a build failure
/// because they bound to `0.0.0.0` in a homelab test would be worse
/// than the warning. The warning is the visible nudge.
fn settings_required(ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
    let mut findings = Vec::new();
    let insecure = ctx.settings.secret_key == INSECURE_DEV_SECRET_KEY;
    if matches!(ctx.settings.environment, Environment::Prod) && insecure {
        findings.push(SystemCheckFinding {
            check_id: "settings.required",
            severity: Severity::Error,
            location: CheckLocation::Settings,
            message: "Settings.secret_key is still set to the insecure dev default in Environment::Prod. This is a hard production risk.".to_string(),
            hint: Some("set UMBRA_SECRET_KEY in your production env, or change `secret_key` in umbra.toml.".to_string()),
        });
        return findings;
    }
    // The default for Environment is Dev, so an operator who never
    // sets UMBRA_ENVIRONMENT slips past the strict check above. Add a
    // bind-address heuristic: if we're binding to something other than
    // loopback, treat it as likely-public and warn.
    if insecure && !is_loopback_bind(&ctx.settings.bind_addr) {
        findings.push(SystemCheckFinding {
            check_id: "settings.required",
            severity: Severity::Warning,
            location: CheckLocation::Settings,
            message: format!(
                "Settings.secret_key is the insecure dev default, but bind_addr `{}` doesn't look like loopback. Set UMBRA_ENVIRONMENT=Prod if this is a production deployment so the boot-check fails loudly instead of just warning.",
                ctx.settings.bind_addr,
            ),
            hint: Some("set UMBRA_SECRET_KEY, or restrict bind_addr to 127.0.0.1 for local dev.".to_string()),
        });
    }
    findings
}

/// True when `bind_addr` parses as the loopback interface — i.e.
/// `127.0.0.1`, `::1`, or `localhost`. Anything else is treated as
/// likely public-facing for the secret_key defence-in-depth check.
fn is_loopback_bind(bind_addr: &str) -> bool {
    // The setting is `host:port`; split off the port and inspect the
    // host. Fall back to a string-prefix check for IPv6 brackets.
    let host = bind_addr
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(bind_addr)
        .trim_start_matches('[')
        .trim_end_matches(']');
    host == "127.0.0.1" || host == "::1" || host == "localhost" || host.is_empty()
}

/// Warn when `allowed_hosts` is still the dev default in
/// `Environment::Prod`. A real prod app almost never serves only
/// loopback; logging this gives the operator a nudge while letting the
/// build proceed.
fn settings_allowed_hosts(ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
    let mut findings = Vec::new();
    if matches!(ctx.settings.environment, Environment::Prod)
        && ctx.settings.allowed_hosts.len() == DEFAULT_ALLOWED_HOSTS.len()
        && ctx
            .settings
            .allowed_hosts
            .iter()
            .zip(DEFAULT_ALLOWED_HOSTS.iter())
            .all(|(a, b)| a == b)
    {
        findings.push(SystemCheckFinding {
            check_id: "settings.allowed_hosts",
            severity: Severity::Warning,
            location: CheckLocation::Settings,
            message: "Settings.allowed_hosts is still the dev default [\"localhost\", \"127.0.0.1\"] in Environment::Prod. A real production deployment almost certainly serves a public hostname.".to_string(),
            hint: Some("set UMBRA_ALLOWED_HOSTS or `allowed_hosts` in umbra.toml to the hostnames this app actually serves.".to_string()),
        });
    }
    findings
}

/// Warn when `log_level` is `debug` or `trace` in `Environment::Prod`.
/// Verbose logging in production leaks internals into stdout and
/// usually means a debug session was left on by accident.
fn settings_log_level(ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
    let mut findings = Vec::new();
    let level = ctx.settings.log_level.to_ascii_lowercase();
    if matches!(ctx.settings.environment, Environment::Prod)
        && (level == "debug" || level == "trace")
    {
        findings.push(SystemCheckFinding {
            check_id: "settings.log_level",
            severity: Severity::Warning,
            location: CheckLocation::Settings,
            message: format!(
                "Settings.log_level is \"{}\" in Environment::Prod. Verbose logging in production leaks internals and adds noise.",
                ctx.settings.log_level
            ),
            hint: Some("set UMBRA_LOG_LEVEL to \"info\", \"warn\", or \"error\" for production deployments.".to_string()),
        });
    }
    findings
}

/// Defensive invariant: the URL scheme in `database_url` should match
/// the active backend's `name()`. Phase 2 picks the backend from the
/// URL, so the two agree by construction today; this check exists so a
/// future codepath that sets the backend manually can't silently drift.
fn backend_url_scheme_matches_active_backend(ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
    let mut findings = Vec::new();
    let scheme = ctx
        .settings
        .database_url
        .split_once(':')
        .map(|(s, _)| s)
        .unwrap_or("");
    let expected_backend = match scheme {
        "postgres" | "postgresql" => Some("postgres"),
        "sqlite" => Some("sqlite"),
        _ => None,
    };
    if let Some(expected) = expected_backend {
        let active = ctx.backend.name();
        if expected != active {
            findings.push(SystemCheckFinding {
                check_id: "backend.url_scheme.matches_active_backend",
                severity: Severity::Error,
                location: CheckLocation::Settings,
                message: format!(
                    "Settings.database_url scheme \"{scheme}\" implies backend \"{expected}\", but the active backend is \"{active}\"."
                ),
                hint: Some("the URL and the active backend must agree; fix `database_url` in umbra.toml or whichever codepath overrode the backend.".to_string()),
            });
        }
    }
    findings
}

/// Walk every registered model and fail at boot when a field's type
/// is incompatible with the active backend.
///
/// Phase 4.1 ships exactly one gated type: `SqlType::Array(_)`, which
/// only works on Postgres. The check matches on the `Column::ty`
/// stored in the migrate registry directly, rather than walking back
/// to `Model::FIELDS` for the `supported_backends` slice (the latter
/// isn't carried on `migrate::Column`). When the next Postgres-only
/// `SqlType` variant lands (HStore, FullTextSearch, etc.), it gets
/// added to the `is_postgres_only` match below.
///
/// **Error**, not Warning: a field rendered against the wrong backend
/// produces incorrect DDL or a runtime panic deep inside `bind_value`.
/// Boot-time failure with a clear message is the right behaviour.
fn field_backend(ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
    let mut findings = Vec::new();
    let active = ctx.backend.name();
    if active == "postgres" {
        // No Postgres-only type is rejected on Postgres; the SQLite
        // side does the rejecting. Early return keeps the registry
        // walk out of the hot path on Postgres boots.
        return findings;
    }
    // Low-level tests that drive `run_all` without booting an App
    // never publish the model registry; the check would panic on
    // `registered_plugins()`. Skip silently — there are no models to
    // walk anyway.
    if !crate::migrate::is_initialised() {
        return findings;
    }

    for plugin in crate::migrate::registered_plugins() {
        for model in crate::migrate::models_for_plugin(&plugin) {
            for field in &model.fields {
                if is_postgres_only(field.ty) {
                    findings.push(SystemCheckFinding {
                        check_id: "field.backend",
                        severity: Severity::Error,
                        location: CheckLocation::Settings,
                        message: format!(
                            "Field `{plugin}::{}::{}` has type {:?} which is Postgres-only, but the active backend is `{active}`.",
                            model.name, field.name, field.ty,
                        ),
                        hint: Some(
                            "switch UMBRA_DATABASE_URL to a `postgres://...` URL, \
                             or change the field to a portable type — \
                             `serde_json::Value` (SqlType::Json) is the closest \
                             portable analogue to an array."
                                .to_string(),
                        ),
                    });
                }
            }
        }
    }
    findings
}

/// True for `SqlType` variants that only work on Postgres. Phase 4.1
/// added `Array(_)`; Phase 4.4 adds `Inet`, `Cidr`, `MacAddr`. Future
/// Postgres-only types (HStore, FullTextSearch) get added to this
/// match.
fn is_postgres_only(ty: crate::orm::SqlType) -> bool {
    use crate::orm::SqlType;
    matches!(
        ty,
        SqlType::Array(_) | SqlType::Inet | SqlType::Cidr | SqlType::MacAddr | SqlType::FullText
    )
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
