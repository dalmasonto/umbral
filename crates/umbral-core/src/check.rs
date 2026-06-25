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
const INSECURE_DEV_SECRET_KEY: &str = "umbral-insecure-dev-key-change-me";

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
    /// `true` when at least one registered plugin reports
    /// [`crate::plugin::Plugin::provides_storage`]. The
    /// `field.storage_backend` check reads this to decide whether a
    /// model with a `FileField` / `ImageField` has a backend to resolve
    /// uploads through.
    ///
    /// This is the *capability flag* of the plugin list, not the ambient
    /// `crate::storage::storage_opt()` — storage is registered in
    /// `on_ready`, which runs *after* this check, so the ambient backend
    /// isn't published yet at check time. `App::build` populates this
    /// from the sorted plugin list before running the checks. Tests that
    /// build a `CheckContext` by hand (without a plugin walk) set `true`
    /// to keep the storage check inert.
    pub provides_storage: bool,
    /// The names of every registered plugin, in topological order, as
    /// returned by [`crate::plugin::Plugin::name`]. Populated by
    /// `App::build` before running phase 4 checks. Tests that build a
    /// `CheckContext` by hand should supply an empty slice (`&[]`) to
    /// make plugin-aware checks that need a specific set of names inert,
    /// or supply the names they want to exercise directly.
    pub registered_plugin_names: &'a [&'a str],
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
            id: "settings.host_validation",
            run: settings_host_validation,
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
        SystemCheck {
            id: "field.storage_backend",
            run: field_storage_backend,
        },
        SystemCheck {
            id: "field.choices_default",
            run: field_choices_default,
        },
        SystemCheck {
            id: "plugin.security_missing",
            run: plugin_security_missing,
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
///    `UMBRAL_ENVIRONMENT=Prod`. If `bind_addr` isn't `127.0.0.1` or
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
            hint: Some("set UMBRAL_SECRET_KEY in your production env, or change `secret_key` in umbral.toml.".to_string()),
        });
        return findings;
    }
    // The default for Environment is Dev, so an operator who never
    // sets UMBRAL_ENVIRONMENT slips past the strict check above. Add a
    // bind-address heuristic: if we're binding to something other than
    // loopback, treat it as likely-public and warn.
    if insecure && !is_loopback_bind(&ctx.settings.bind_addr) {
        findings.push(SystemCheckFinding {
            check_id: "settings.required",
            severity: Severity::Warning,
            location: CheckLocation::Settings,
            message: format!(
                "Settings.secret_key is the insecure dev default, but bind_addr `{}` doesn't look like loopback. Set UMBRAL_ENVIRONMENT=Prod if this is a production deployment so the boot-check fails loudly instead of just warning.",
                ctx.settings.bind_addr,
            ),
            hint: Some("set UMBRAL_SECRET_KEY, or restrict bind_addr to 127.0.0.1 for local dev.".to_string()),
        });
    }
    findings
}

/// Warn when the server binds a non-loopback address but Host-header
/// validation isn't enforced. `App::build` only mounts the
/// `allowed_hosts` guard under [`Environment::Prod`] (see
/// `app.rs`); a deployment that binds `0.0.0.0` while still flagged
/// `Dev` therefore accepts *any* `Host` header — the classic vector
/// for cache-poisoning and poisoned password-reset links.
///
/// The Prod path already enforces, so this only fires outside Prod,
/// and only on a non-loopback bind (a local `127.0.0.1` dev server is
/// not reachable with a forged Host from the network). It's a warning,
/// not a boot-blocking error, for the same reason the insecure-key
/// non-loopback case is: surprising a homelab test with a hard failure
/// would be worse than the nudge.
fn settings_host_validation(ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
    if !host_validation_unenforced(&ctx.settings.environment, &ctx.settings.bind_addr) {
        return Vec::new();
    }
    vec![SystemCheckFinding {
        check_id: "settings.host_validation",
        severity: Severity::Warning,
        location: CheckLocation::Settings,
        message: format!(
            "bind_addr `{}` is not loopback, but Host-header validation is only enforced in Environment::Prod. This deployment accepts any Host header (cache-poisoning / poisoned-reset-link risk).",
            ctx.settings.bind_addr,
        ),
        hint: Some(
            "set UMBRAL_ENVIRONMENT=Prod (enforces allowed_hosts), or bind 127.0.0.1 for local dev."
                .to_string(),
        ),
    }]
}

/// Pure predicate behind [`settings_host_validation`]: Host validation
/// is unenforced when we're *not* in Prod yet bound to a non-loopback
/// address. Split out so it's testable without constructing a full
/// [`CheckContext`] (which needs a live backend).
fn host_validation_unenforced(environment: &Environment, bind_addr: &str) -> bool {
    !matches!(environment, Environment::Prod) && !is_loopback_bind(bind_addr)
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
            hint: Some("set UMBRAL_ALLOWED_HOSTS or `allowed_hosts` in umbral.toml to the hostnames this app actually serves.".to_string()),
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
            hint: Some("set UMBRAL_LOG_LEVEL to \"info\", \"warn\", or \"error\" for production deployments.".to_string()),
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
                hint: Some("the URL and the active backend must agree; fix `database_url` in umbral.toml or whichever codepath overrode the backend.".to_string()),
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
                // IMP-5: per-field backend gate via
                // `#[umbral(backend = "postgres")]`. When the slice
                // is non-empty and the active backend isn't listed,
                // reject at boot with a clear message. The
                // hardcoded `is_postgres_only` branch below remains
                // for types the framework knows about; the
                // declared-list path covers user-facing attribute
                // shape.
                if !field.supported_backends.is_empty()
                    && !field.supported_backends.iter().any(|b| b == active)
                {
                    findings.push(SystemCheckFinding {
                        check_id: "field.backend",
                        severity: Severity::Error,
                        location: CheckLocation::Settings,
                        message: format!(
                            "Field `{plugin}::{}::{}` declares `#[umbral(backend = ...)]` \
                             as {:?}, but the active backend is `{active}`.",
                            model.name, field.name, field.supported_backends,
                        ),
                        hint: Some(format!(
                            "switch UMBRAL_DATABASE_URL to a backend matching one of \
                             {:?}, or drop the `backend` attribute and pick a portable \
                             field type.",
                            field.supported_backends,
                        )),
                    });
                    continue;
                }
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
                            "switch UMBRAL_DATABASE_URL to a `postgres://...` URL, \
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

/// Fail at boot when a model declares a `FileField` / `ImageField`
/// (detected by the column's `widget` being `"file"` or `"image"`) but
/// no registered plugin provides a [`Storage`](crate::storage::Storage)
/// backend.
///
/// **Why the capability flag, not the ambient `storage_opt()`:** a
/// `Storage` backend is registered in `Plugin::on_ready`, which runs
/// *after* the system-check phase (see `App::build`'s phase ordering).
/// So at check time `crate::storage::storage_opt()` is still `None` even
/// when `StoragePlugin` is wired and *will* register a backend a moment
/// later. Checking the ambient here would false-positive on every app
/// that uses media. Instead we read `ctx.provides_storage`, which
/// `App::build` computes from the sorted plugin list's
/// `Plugin::provides_storage()` flags — the *declared capability*, which
/// is knowable at check time.
///
/// **Error**, not Warning: a file/image field with no backend means
/// `FileField::url` silently falls back to the raw key, producing broken
/// `<img src>` / download links in production. Failing the build with a
/// clear fix is the right behaviour.
fn field_storage_backend(ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
    let mut findings = Vec::new();
    // A backend is (or will be) registered — nothing to check.
    if ctx.provides_storage {
        return findings;
    }
    // Low-level tests that drive `run_all` without booting an App never
    // publish the model registry; skip silently (there are no models to
    // walk anyway, same guard as `field_backend`).
    if !crate::migrate::is_initialised() {
        return findings;
    }
    for plugin in crate::migrate::registered_plugins() {
        for model in crate::migrate::models_for_plugin(&plugin) {
            for field in &model.fields {
                let is_file_field = matches!(field.widget.as_deref(), Some("file") | Some("image"));
                if !is_file_field {
                    continue;
                }
                // Leak the owned strings into the finding's
                // &'static-typed location. The walk runs once at boot, so
                // the small leak is acceptable and matches the
                // location-string contract (Field carries &'static str).
                findings.push(SystemCheckFinding {
                    check_id: "field.storage_backend",
                    severity: Severity::Error,
                    location: CheckLocation::Field {
                        plugin: Box::leak(plugin.clone().into_boxed_str()),
                        model: Box::leak(model.name.clone().into_boxed_str()),
                        field: Box::leak(field.name.clone().into_boxed_str()),
                    },
                    message: format!(
                        "Model `{plugin}::{}` field `{}` declares a file/image field, \
                         but no Storage backend is registered.",
                        model.name, field.name,
                    ),
                    hint: Some(
                        "add `StoragePlugin` to your app (it registers a filesystem Storage \
                         backend), or call `umbral::storage::set_storage(...)` before \
                         `App::build()` to wire a custom backend."
                            .to_string(),
                    ),
                });
            }
        }
    }
    findings
}

/// Walk every registered model and fail at boot when a `choices`
/// column's declared default isn't one of the column's choices.
///
/// **Why this exists (gaps2 #32):** a choices field's default lands
/// verbatim in DDL (`migrate.rs`'s `def.default(col.default.clone())`),
/// so writing `#[umbral(default = "PostStatus::Draft")]` — the Rust enum
/// *path* instead of the stored DB literal `"draft"` — ships a broken
/// schema. Postgres rejects the row at insert via the `CHECK (col IN
/// (...))` constraint; SQLite stores the undecodable text and errors on
/// the next `SELECT` when the `ChoiceField` decoder can't map it back.
/// Per the "backend mismatches caught at boot" principle, this surfaces
/// the mistake at build time with a clear message instead of in prod.
///
/// The check works off `Column.choices`, which already holds the DB
/// values (`FieldSpec::choices`), so `choices` *is* the allowed set —
/// no need to reach for `ChoiceField::VALUES`. When the bad default
/// contains `::` (the tell-tale of a pasted Rust enum path), we lower
/// the part after the last `::` and, if that matches a real choice,
/// emit a did-you-mean for the stored literal.
///
/// **Error**, not Warning: the DDL is wrong and the table is unusable.
fn field_choices_default(_ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
    let mut findings = Vec::new();
    // Low-level tests that drive `run_all` without booting an App never
    // publish the model registry; skip silently (same guard as the
    // other model-walking checks).
    if !crate::migrate::is_initialised() {
        return findings;
    }
    for plugin in crate::migrate::registered_plugins() {
        for model in crate::migrate::models_for_plugin(&plugin) {
            for field in &model.fields {
                // Only choices columns with an explicit default can be
                // wrong this way: a non-choices column has no allowed
                // set to violate, and an empty default emits no DDL
                // `DEFAULT` at all.
                if field.choices.is_empty()
                    || field.default.is_empty()
                    || field.choices.contains(&field.default)
                {
                    continue;
                }
                let hint = if field.default.contains("::") {
                    // `Foo::Bar` → `bar`; choices are typically declared
                    // with `rename_all = "lowercase"`, so lower the tail
                    // before checking for a match.
                    let suggested = field
                        .default
                        .rsplit("::")
                        .next()
                        .unwrap_or(&field.default)
                        .to_lowercase();
                    if field.choices.contains(&suggested) {
                        format!(
                            "Did you mean the DB literal `{suggested}`? Choices defaults are \
                             the stored value (e.g. `\"draft\"`), not the Rust enum path \
                             (`\"PostStatus::Draft\"`)."
                        )
                    } else {
                        format!(
                            "Set the default to one of the stored values: [{}].",
                            field.choices.join(", "),
                        )
                    }
                } else {
                    format!(
                        "Set the default to one of the stored values: [{}].",
                        field.choices.join(", "),
                    )
                };
                // Leak the owned strings into the finding's
                // &'static-typed location — the walk runs once at boot,
                // matching the storage check's pattern.
                findings.push(SystemCheckFinding {
                    check_id: "field.choices_default",
                    severity: Severity::Error,
                    location: CheckLocation::Field {
                        plugin: Box::leak(plugin.clone().into_boxed_str()),
                        model: Box::leak(model.name.clone().into_boxed_str()),
                        field: Box::leak(field.name.clone().into_boxed_str()),
                    },
                    message: format!(
                        "Model `{plugin}::{}` field `{}` has default `{}` which is not one \
                         of its choices: [{}].",
                        model.name,
                        field.name,
                        field.default,
                        field.choices.join(", "),
                    ),
                    hint: Some(hint),
                });
            }
        }
    }
    findings
}

/// Warn when `AuthPlugin` or `SessionsPlugin` is registered but
/// `SecurityPlugin` is NOT.
///
/// An app that handles authenticated or session traffic with no
/// `SecurityPlugin` has **no CSRF protection and no hardening headers**
/// (CSP, Strict-Transport-Security, X-Frame-Options, etc.) — an
/// easy-to-miss footgun. The check is a **Warning** (boot continues)
/// because some apps legitimately handle CSRF through other means (a
/// reverse-proxy header, a separate middleware, or a custom plugin).
///
/// Gaps2 #25 (scaffold-independent half): the scaffold half that auto-
/// mounts `SecurityPlugin` in `umbral startproject` is deferred until the
/// #8 scaffold lands.
fn plugin_security_missing(ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
    let names = ctx.registered_plugin_names;
    let has_auth = names.contains(&"auth");
    let has_sessions = names.contains(&"sessions");
    if !(has_auth || has_sessions) {
        // Neither auth nor sessions — nothing to warn about.
        return Vec::new();
    }
    if names.contains(&"security") {
        // SecurityPlugin is present — all good.
        return Vec::new();
    }
    let who = match (has_auth, has_sessions) {
        (true, true) => "AuthPlugin and SessionsPlugin are",
        (true, false) => "AuthPlugin is",
        (false, true) => "SessionsPlugin is",
        (false, false) => unreachable!(),
    };
    vec![SystemCheckFinding {
        check_id: "plugin.security_missing",
        severity: Severity::Warning,
        location: CheckLocation::Settings,
        message: format!(
            "{who} mounted without SecurityPlugin — requests have no CSRF \
             protection or security headers (CSP, HSTS, X-Frame-Options, …). \
             Add `.plugin(SecurityPlugin::new())` to your App builder, or \
             handle CSRF / headers through another mechanism.",
        ),
        hint: Some(
            "add `.plugin(umbral_security::SecurityPlugin::new())` to your \
             `App::builder()` call."
                .to_string(),
        ),
    }]
}

/// True for `SqlType` variants that only work on Postgres. Phase 4.1
/// added `Array(_)`; Phase 4.4 adds `Inet`, `Cidr`, `MacAddr`. Future
/// Postgres-only types (HStore, FullTextSearch) get added to this
/// match.
fn is_postgres_only(ty: crate::orm::SqlType) -> bool {
    use crate::orm::SqlType;
    matches!(
        ty,
        SqlType::Array(_)
            | SqlType::Inet
            | SqlType::Cidr
            | SqlType::MacAddr
            // gaps2 #70: text-backed Postgres types (XML / LTREE /
            // BIT VARYING) have no SQLite equivalent; the boot check
            // rejects them on SQLite the same way as the network types.
            | SqlType::Xml
            | SqlType::Ltree
            | SqlType::Bit
            | SqlType::FullText
            // BUG-10: sqlx's `rust_decimal` Encode/Decode is
            // Postgres-only. SQLite has no native NUMERIC type;
            // any model with a Decimal column fails the boot
            // check the same way Array does.
            | SqlType::Decimal
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

#[cfg(test)]
mod tests {
    use super::{host_validation_unenforced, is_loopback_bind};
    use crate::settings::Environment;

    #[test]
    fn loopback_binds_are_recognised() {
        assert!(is_loopback_bind("127.0.0.1:8000"));
        assert!(is_loopback_bind("localhost:3000"));
        assert!(is_loopback_bind("[::1]:8080"));
        assert!(is_loopback_bind(":8000")); // host omitted → local
        assert!(!is_loopback_bind("0.0.0.0:8000"));
        assert!(!is_loopback_bind("192.168.1.10:8000"));
    }

    #[test]
    fn host_validation_warns_only_off_prod_and_non_loopback() {
        // Non-loopback + not Prod → unenforced (warn).
        assert!(host_validation_unenforced(
            &Environment::Dev,
            "0.0.0.0:8000"
        ));
        // Prod enforces regardless of bind.
        assert!(!host_validation_unenforced(
            &Environment::Prod,
            "0.0.0.0:8000"
        ));
        // Loopback bind is not network-reachable with a forged Host.
        assert!(!host_validation_unenforced(
            &Environment::Dev,
            "127.0.0.1:8000"
        ));
    }
}
