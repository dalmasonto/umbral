# CLI startcommand rot, plugin route-discovery drift, and a typed plugin-settings schema

Status: DRAFT for gaps5 #76 (tf#289), #77 (tf#290), #78 (tf#291).
Date: 2026-08-08
Scope: three related plugin-contract / CLI items that all share one root cause, namely that the framework keeps a *hand-maintained copy* of something the live plugin/command registry already knows. #76 is a hardcoded command-reservation list that rots. #77 is a two-list route declaration that drifts. #78 replaces ad-hoc `Settings.extra` reads with a declared, typed settings schema. Each section is self-contained; they are filed together because the fix pattern is identical (derive from the live registry, do not duplicate it) and #78 ties directly into the plugin manifest, gaps5 #6.

Pairs with:

- `crates/umbral-cli/src/scaffold.rs` (the hardcoded reservations, the `startcommand` scaffolder).
- `crates/umbral-cli/src/lib.rs` (command dispatch, `builtin_command_names`, `full_catalog`).
- `crates/umbral-core/src/cli.rs` (`PluginCommand`, `CommandSet`, `command_catalog_with_app_commands`).
- `crates/umbral-core/src/plugin.rs` (`routes`, `routes_builder`, `route_paths`, `system_checks`, `commands`).
- `crates/umbral-core/src/routes.rs` (`RouteSpec`, the `Routes` builder, `RouteRegistry`).
- `crates/umbral-core/src/settings.rs` (`Settings.extra`, `Settings::extra_str`).
- `crates/umbral-core/src/check.rs` (`SystemCheck`, `CheckContext`, `SystemCheckFinding`, `Severity`, `CheckLocation`).
- `docs/specs/plugin-manifest-and-registry.md` (the manifest for the #78 and #77 tie-ins; gaps5 #6).

## gaps5 #76 (tf#289): `startcommand` rot and third-party command shadowing

### The problem

`umbral startcommand <name>` scaffolds a new management command. Before writing anything, `scaffold_command` (`scaffold.rs:1620`) rejects any name that collides with an existing command, because dispatch tries registered commands BEFORE the built-in parser (`lib.rs:412` step 1 vs the built-in set that follows). A user command named `migrate` would not error loudly; it would quietly take over and the next deploy would apply zero migrations and exit 0. So the reservation guard is load-bearing.

That guard is `reserved_command_names()` at `scaffold.rs:127`:

```rust
pub fn reserved_command_names() -> Vec<String> {
    let mut names = crate::builtin_command_names();          // derived, good
    names.extend(RESERVED_PLUGIN_COMMAND_NAMES.iter().map(|s| s.to_string())); // hardcoded, rots
    names.sort();
    names.dedup();
    names
}
```

It is built from two halves with opposite hygiene:

1. **The framework half** (`builtin_command_names()`, `lib.rs:636`) is read off the derived clap parser via `CommandFactory`. Adding a subcommand to `Cli` in `lib.rs` reserves its name automatically. This half is correct and needs no maintenance.

2. **The plugin half** is a hardcoded const array, `RESERVED_PLUGIN_COMMAND_NAMES` at **`crates/umbral-cli/src/scaffold.rs:101-113`**:

```rust
pub const RESERVED_PLUGIN_COMMAND_NAMES: &[&str] = &[
    "clearsessions", "collectstatic", "createsuperuser", "gen-client",
    "migrate_schemas", "startauthentication", "startpagination",
    "startpermission", "startthrottle", "tasks-beat", "tasks-worker",
];
```

The doc-comment above it admits why: these command names "only exist once the plugin is registered on an App, and `startcommand` runs outside any App. So they're listed." That is the whole bug. The list is a manual mirror of every built-in plugin's `Plugin::commands()`. It rots two ways:

- **Built-in drift.** A built-in plugin that adds a command (or renames one) must remember to edit this array in a different crate. Miss it and `startcommand createsuperuser` scaffolds a command that silently shadows the real one - exactly the failure the guard exists to prevent.
- **Third-party invisibility.** The array only lists BUILT-IN plugin commands. A third-party plugin the user installed (`acme-billing` contributing a `billing-sync` command) is not and cannot be in this const. `startcommand billing-sync` sails through and shadows it. The guard protects the framework's own commands but not the ecosystem's, which is precisely backwards for a plugin-first framework.

The `RESERVED_PLUGIN_NAMES` array right above it (`scaffold.rs:61-91`, the built-in *plugin* names for `startplugin`) has the same shape but a weaker failure mode (a name clash there fails at registration, loudly), so it is out of scope here; this item is about *command* names, whose clash is silent.

### The fix: run `startcommand` against the live command registry

The framework already has the exact data the const is trying to approximate. `CommandSet::collect(app.commands(), app.plugins(), &reserved)` (`cli.rs:232`) walks every app-registered and plugin-contributed command, and `CommandSet::catalog()` (`cli.rs:259`) returns `(name, about)` for each. Dispatch itself is built on this. The reservation list should be READ from that walk, not hand-copied from it.

The obstacle the doc-comment names is real: `scaffold_command` runs in the `umbral` CLI binary, which has no `App` and therefore no plugin list. The framework `umbral` binary cannot know which third-party plugins a given project installed, because those plugins are compiled into the *project's* binary, not the CLI's.

Resolution, two parts:

1. **Forward `startcommand` into the project.** The project binary is the one thing that HAS the fully-wired `App` with every plugin (built-in and third-party) registered. Make `umbral startcommand` a thin front-end that shells into the project binary (`cargo run -- __scaffold-command <name> --in <target>`, an internal subcommand), so the scaffolder runs with `app.plugins()` in hand. This mirrors the existing dispatch split: `dispatch_with_argv` (`lib.rs:351`) already collects `CommandSet` from the live app once (`lib.rs:383`) and asks it questions. The scaffolder becomes one more question asked of that same set. The internal subcommand is gated (an `UMBRAL_INTERNAL` marker or a hidden clap subcommand) so it never appears in the user-facing catalog.

2. **Derive the reserved set from the live registry.** Inside the project binary, replace the `RESERVED_PLUGIN_COMMAND_NAMES` half of `reserved_command_names()` with the names from `CommandSet::catalog()`. The framework half stays as-is (already derived). The signature changes from a free function to one that takes the collected command set:

```rust
// reads the LIVE registry instead of a hand-listed const
pub fn reserved_command_names(commands: &CommandSet<'_>) -> Vec<String> {
    let mut names = crate::builtin_command_names();           // framework, off clap
    names.extend(commands.catalog().into_iter().map(|(n, _)| n)); // plugins, off the walk
    names.sort();
    names.dedup();
    names
}
```

`RESERVED_PLUGIN_COMMAND_NAMES` is then deleted. There is nothing left for it to hold: every name in it is now produced by walking the plugins that actually contribute those commands, so a built-in that renames a command, and a third-party plugin the const never knew about, are both covered by construction.

**Fallback when the project will not build.** If `cargo run` fails (the project has a compile error, or is not present), `startcommand` cannot reach the live registry. In that case it degrades to the framework-half guard (`builtin_command_names()`, still derived and correct) plus a printed warning that plugin-command shadowing could not be checked because the project did not build. That is strictly better than today: today the plugin half is a stale const even when the project builds fine.

**Interaction with the manifest (#6).** The plugin manifest's `Capability::Commands` flag (spec §2.2) declares *that* a plugin ships commands; it does not enumerate their names, so it does not replace this walk. The authoritative name list stays the live `CommandSet`. The manifest is complementary, not a substitute.

## gaps5 #77 (tf#290): plugin route-discovery escape-hatch drift

### The problem

A plugin contributes HTTP routes through `Plugin::routes()` (`plugin.rs:185`), which returns an `axum::Router` merged into the app. axum exposes no introspection of its internal route table, so for any surface that needs to *list* routes outside the request flow (the dev-mode 404 page today; ungated-mutating-route audits and OpenAPI security annotations tomorrow) the plugin must ALSO declare them via `Plugin::route_paths()` (`plugin.rs:249`), which returns `Vec<RouteSpec>`.

These are two independent lists that nothing forces to agree. The trait doc-comment says so outright (`plugin.rs:174`): "a route mounted here but not declared in `route_paths()` is invisible to every audit / discovery surface." A plugin author who adds `.route("/foo", ...)` to `routes()` and forgets `route_paths()` gets a silently incomplete registry. The cost today is a stale 404 page; the cost tomorrow (an ungated mutating route the audit cannot see) is a security gap.

gaps4 #31 already shipped the drift-free alternative, `Plugin::routes_builder()` (`plugin.rs:223`), returning `Option<Routes>`. The `Routes` builder (`routes.rs:200`) records a `RouteSpec` on every `.get/.post/.route` call at the same moment it mounts the handler, so the axum router and the spec list come from ONE source and cannot diverge. When `routes_builder()` returns `Some`, the framework takes both the router and the specs from it and ignores `routes()`/`route_paths()` entirely.

Two gaps remain:

1. **`routes_builder` is opt-in and defaults to `None`.** Nothing steers a plugin author toward it. The scaffolder still emits the drift-prone `routes()` + `route_paths()` pair, so new plugins start life on the drifting path.
2. **No boot-time signal.** A plugin that mounts routes via `routes()` with an empty or mismatched `route_paths()` boots clean. The drift is discovered later, when a discovery surface is wrong.

The one residual even the builder cannot close: paths inside a router merged via `Routes::with_router` (`routes.rs:377`) or an axum `nest` are still not recorded, because axum has no route-table introspection (documented at `plugin.rs:211` and `routes.rs:36`). That escape hatch keeps the same caveat by design; the goal is to make it the rare, flagged exception rather than the default.

### The fix: scaffold the builder, warn on the legacy path

**1. Make `Routes` the scaffolded default.** The `startplugin` generator emits `routes_builder()` returning `Some(Routes::new().get(...).post(...))` instead of the `routes()` + `route_paths()` pair. New plugins are drift-free from their first commit, and the builder is what an author copies from existing code. (The `AppBuilder::routes(Routes::new()...)` path for the user binary already uses the builder, per `routes.rs:158`; this brings plugins in line.)

**2. Add a boot-time system check `route.discovery.drift`** to `framework_checks()` (`check.rs:172`). It is a framework built-in, not a per-plugin check, because it reasons across the whole plugin walk against one policy. For each registered plugin it emits a `Warning` (never an `Error`; a stale discovery list is benign, not a correctness break) with `CheckLocation::Plugin { plugin }` when the plugin is on the legacy path AND its declaration looks drifted:

- The plugin returns `None` from `routes_builder()` (legacy path), AND
- it declares the `Routes` capability in its manifest (gaps5 #6, spec §2.2) - i.e. it says it contributes routes, AND
- its `route_paths()` is empty.

That combination is "I mount routes but declare none for discovery," the exact drift signature. The finding's hint points the author at `routes_builder()` and this decision.

**Why the check leans on the manifest capability flag.** The check cannot observe drift directly: an `axum::Router` returned from `routes()` is opaque, so the framework cannot count how many routes it holds and compare against `route_paths().len()`. What it CAN read cheaply is (a) whether `routes_builder()` is `Some`, and (b) the manifest's declared `Routes` / `RoutesBuilder` / `RoutePaths` capabilities. So the check is a cross-check of *declared intent* against *chosen mechanism*, in the same spirit as the manifest's own drift cross-checks (spec §3.2 item 4). A plugin that has genuinely no routes declares no `Routes` capability and is never flagged. A plugin on the builder path returns `Some` and is never flagged. Only the legacy-and-appears-drifted case warns.

**CheckContext extension.** The check needs per-plugin visibility that `CheckContext` does not carry today: it holds `registered_plugin_names: &[&str]` (`check.rs:110`) but not the manifests or the `routes_builder`-is-`Some` bit. This is the same small extension the manifest spec already calls for (spec §3.1: add `plugin_manifests: &[PluginManifest]`, populated by `App::build` from the same plugin walk). `route.discovery.drift` reads the manifest capabilities from that field, plus one added parallel slice recording whether each plugin's `routes_builder()` returned `Some` (computed once during the build's route-collection phase, not re-invoked). No new global; the data rides the existing walk, per the "one intentional global" rule in `CLAUDE.md`.

**Escape-hatch honesty.** `Routes::with_router` and `nest` paths stay legal and stay un-introspectable. The check does not try to flag them (it cannot see inside them). The doc-comments on `routes_builder` (`plugin.rs:211`) and `Routes::with_router` (`routes.rs:368`) already state the residual caveat; this item does not change it, it just makes the drift-free path the one you fall into by default and the drifting path the one you get warned about.

## gaps5 #78 (tf#291): a typed plugin-settings schema registry

### The problem

Plugins read configuration from `Settings.extra` (`settings.rs:502`), the `#[serde(flatten)]` catch-all for `UMBRAL_`-prefixed keys that do not map to a named field:

```rust
#[serde(flatten)]
pub extra: std::collections::HashMap<String, toml::Value>,
```

with one accessor, `Settings::extra_str(&self, key: &str) -> Option<&str>` (`settings.rs:622`), for the scalar-string case, and direct `toml::Value` indexing for nested tables. Every plugin that needs config hand-rolls its own reader, an ad-hoc `from_settings` pattern. This works but has no schema, and the absence shows:

- **No declaration.** Nothing lists which keys a plugin reads, their types, or their defaults. An operator deploying `acme-billing` has to read the plugin's source to learn it wants `billing.stripe_key`.
- **No validation.** A misspelled or missing required key is not caught at boot; it surfaces as a runtime `None` deep in a handler, or a silent fallback to a default. (The framework already learned this lesson for its OWN keys: `warn_on_near_miss_keys` at `settings.rs:732` catches typos of the *named* fields, but `extra` keys - exactly where plugin config lives - get no such guard.)
- **No secret hygiene.** `extra` is where third-party API keys land, so the redacting `Debug` masks every `extra` value (`RedactedExtra`, `settings.rs:543`). But nothing knows WHICH keys are secret, so nothing can warn "your required secret is still empty" or "still at an obvious default" the way the built-in `secret_key` check does (`check.rs`, `settings.required`).
- **No docs generation.** There is no machine-readable list to render an operator-facing "settings this app needs" page from.

The manifest spec (gaps5 #6, §2.3) already ships a *lightweight* `RequiredSetting { key, ty, required, secret, env, help }` for compatibility and docs. It explicitly defers the full typed schema to this item (spec §6: "Not the typed-settings-schema registry (gaps5 #78) ... When #78 lands, the manifest's `required_settings` becomes a projection of that schema rather than a parallel source"). This item builds that schema.

### The fix: a `PluginSettings` schema contributed via a new `Plugin` method

Add one method to the `Plugin` trait, defaulting empty so no existing plugin breaks:

```rust
// crates/umbral-core/src/plugin.rs, added to the Plugin trait:

/// The typed settings schema this plugin reads from `Settings.extra`
/// (gaps5 #78). Each field declares its key, type, env-var mapping,
/// default, secret flag, validation, and one-line docs. The framework
/// validates these at boot (a `settings.plugin.<name>` system check),
/// masks the secrets in logs, and generates operator docs from them.
///
/// Default: no declared settings. A plugin that reads nothing from
/// `Settings.extra` leaves this alone.
fn settings_schema(&self) -> PluginSettings {
    PluginSettings::empty()
}
```

with the schema type in `umbral-core`, re-exported from the facade under `umbral::plugin` (NOT the prelude; this is plugin-author / tooling surface, matching the manifest's tiering):

```rust
pub struct PluginSettings {
    /// The plugin's key namespace under Settings.extra, e.g. "billing".
    /// Fields are addressed as "<namespace>.<field.key>".
    pub namespace: &'static str,
    pub fields: Vec<SettingField>,
}

pub struct SettingField {
    /// Key under the plugin's namespace, e.g. "stripe_key" ->
    /// Settings.extra["billing"]["stripe_key"].
    pub key: &'static str,
    /// Coarse type for docs + validation.
    pub ty: SettingType,
    /// Boot fails (Error) if absent and no default; else the default
    /// applies. Absent-and-no-default with `required` is the Error case.
    pub required: bool,
    /// Redact in logs / docs; feeds a "secret is still empty or default"
    /// warning. Never echoed.
    pub secret: bool,
    /// Value used when the key is absent. `None` + `required` = boot Error.
    pub default: Option<SettingValue>,
    /// Environment variable that overrides it (e.g. "STRIPE_KEY").
    pub env: Option<&'static str>,
    /// Optional validator run at boot on the resolved value.
    pub validate: Option<fn(&SettingValue) -> Result<(), String>>,
    /// One-line human description for generated docs.
    pub help: &'static str,
}

pub enum SettingType { String, Int, Bool, Url, Duration, List }
```

`SettingValue` is the resolved, typed value (a small enum over the `SettingType` variants), parsed once from the `toml::Value` in `extra` at boot so plugins read a typed value instead of re-parsing a string on every access.

### Boot validation, secret hygiene, docs

**A framework built-in check `settings.plugin`** joins `framework_checks()` (`check.rs:172`). Like the manifest compat check it is framework-level (it walks every plugin) and reads the schemas from the same plugin walk the manifest extension already threads through `CheckContext` (spec §3.1). For each plugin, for each `SettingField`:

- Resolve the value: env var (if `env` set and present) wins, else `Settings.extra[namespace][key]`, else `default`.
- If unresolved AND `required`: `Severity::Error` (blocks boot), `CheckLocation::Plugin { plugin }`, message naming the key and its `env` var and `help`.
- If resolved but `validate` returns `Err`: `Severity::Error` with the validator's message.
- If `secret` AND (empty OR equal to `default`): `Severity::Warning`, reusing the exact posture of the built-in `secret_key` check ("still the insecure default" at `check.rs`, `prod_secret_key_error`).
- Type mismatch (a `toml::Value` that will not coerce to `ty`): `Severity::Error`.

This is the plugin-config analogue of `warn_on_near_miss_keys` (`settings.rs:732`) and `settings_required` (`check.rs`), extended from the framework's own named fields to plugin-declared ones.

**Secret masking gains a source of truth.** Today `RedactedExtra` (`settings.rs:543`) masks EVERY `extra` value because it cannot tell which are secret. With schemas registered, masking can be precise: mask fields declared `secret: true`, show the rest (an operator debugging a non-secret plugin toggle can then see its value). This is a follow-on refinement, not required for the first cut; the conservative "mask all of extra" stays correct in the meantime.

**Docs generation.** A CLI subcommand (`umbral settings`, future work in `umbral-cli`) walks every registered plugin's `settings_schema()` and prints an operator-facing table: namespace, key, type, required, env var, default, secret flag, help. Because the schema is declared, this needs no per-plugin cooperation beyond implementing the method.

### Tie to the manifest (#6): projection, not duplication

The manifest's `required_settings: Vec<RequiredSetting>` (spec §2.3) and this schema's `fields` overlap deliberately. The manifest field is the *lightweight, static* face (readable from `[package.metadata.umbral]` without booting, feeds the catalog and compatibility check). The `PluginSettings` schema is the *runtime, typed, validated* face. Per the manifest spec's own §6, once #78 lands the manifest's `required_settings` is DERIVED from the schema (`RequiredSetting` is the projection `{ key, ty, required, secret, env, help }` of a `SettingField`, dropping `default` and `validate`), so there is one source of truth (the schema) and the manifest reads from it rather than restating it. A plugin declares its settings ONCE, in `settings_schema()`; the manifest's `required_settings` is generated from that.

### Migration path

`Settings.extra` and `extra_str` stay. The schema is additive: a plugin with no `settings_schema()` override behaves exactly as today (reads `extra` by hand, no boot validation). Adopting the schema is a per-plugin opt-in that buys boot validation, precise masking, and docs. The built-in plugins that read `extra` today (per their `from_settings` patterns) migrate one at a time; each migration is a self-contained commit that adds a `settings_schema()` impl and deletes that plugin's hand-rolled reader in favor of the typed `SettingValue`.

## Open questions

1. **#76 forwarding transport.** Shell into `cargo run -- __scaffold-command` (simple, but couples `startcommand` to a buildable project and to `cargo`) versus a lighter mechanism (a generated, checked-in `settings`/command manifest the CLI reads without compiling). The `cargo run` route is recommended for the first cut because it is the only path that sees genuinely third-party plugins; the fallback-to-framework-half guard covers the will-not-build case. Revisit if project build time makes `startcommand` feel slow.
2. **#77 check severity.** `Warning` only (recommended: a stale discovery list is benign) versus an opt-in `AppBuilder::strict_route_discovery()` that escalates to `Error` for apps that treat the ungated-route audit as a release gate. Mirrors the manifest spec's open question 1 on `plugin.compat` severity; resolve the two together for a consistent strict-mode story.
3. **#78 secret masking cutover.** Flip `RedactedExtra` to schema-aware masking (show non-secret plugin settings) in this item, or keep "mask all of extra" and treat precise masking as a separate follow-up once several plugins have adopted schemas? Recommend the latter: the conservative mask is never wrong, and schema-aware masking is only useful once schemas are widespread.
