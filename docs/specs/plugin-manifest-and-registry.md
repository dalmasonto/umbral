# Plugin manifest and registry/catalog format

Status: DRAFT for gaps5 #6 (tf#219). Companion to the marketplace-governance line item gaps5 #90 (tf#303).
Date: 2026-08-08
Scope: the manifest + compatibility contract only. Marketplace hosting, signing infrastructure, and a running registry service are explicitly OUT of scope (Stage 3 per the product north star, `docs/decisions/2026-08-08-product-north-star.md`). This spec defines the data shapes and the boot-time check that everything downstream builds on.

Pairs with:

- `STABILITY.md` (the lockstep-version and plugin-compatibility policy this check enforces).
- `crates/umbral-core/src/plugin.rs` (the real `Plugin` trait whose capabilities the manifest describes).
- `crates/umbral-core/src/check.rs` (the boot-time system-check mechanism the compatibility check plugs into).
- `docs/specs/02-plugin-contract.md` and `docs/specs/08-authoring-plugins.md`.

## 1. Motivation

The `Plugin` trait (`crates/umbral-core/src/plugin.rs`) is a strong *runtime* contract: a plugin contributes models, routes, middleware, commands, and lifecycle hooks, and the core touches it only as `Box<dyn Plugin>`. What the trait does NOT carry today is *distribution metadata*: who wrote the plugin, which umbral versions it supports, where its docs live, whether it is maintained, whether it owns migrations, and which settings or secrets it requires to boot. gaps5 #6 calls this out directly: "a strong trait, but not distribution metadata, compatibility ranges, security status, docs URL, migrations ownership, or marketplace discovery."

Two problems follow from that gap:

1. **No boot-time compatibility signal.** Because every umbral crate shares one lockstep version (`STABILITY.md` §Versioning), "compatible with umbral 0.0.x" is a single range, not a per-crate matrix. But nothing today lets a plugin *declare* that range, so nothing can warn when a plugin built against 0.0.9 is loaded into a 0.0.12 app whose `Plugin` trait grew a required method or changed a signature.
2. **No discovery surface.** There is no machine-readable index a tool (or a future marketplace) can read to list published plugins, their capabilities, and their trust signals.

This spec defines a `PluginManifest` (§2), a boot-time compatibility check (§3), and a registry/catalog index format (§4) that a future marketplace (§5) layers signing and trust badges onto.

## 2. The manifest

### 2.1 What a plugin declares

| Field | Meaning | Source of truth |
|---|---|---|
| `name` | Stable identifier. MUST equal `Plugin::name()`. | trait, mirrored in manifest |
| `crate_name` | The Cargo crate that ships the plugin (`umbral-auth`, `acme-billing`). | `Cargo.toml` |
| `version` | The plugin's own release version. For built-ins this equals the lockstep umbral version; for third-party plugins it is independent. | `Cargo.toml` `package.version` |
| `umbral_req` | The umbral version range the plugin supports, as a semver `VersionReq` (e.g. `>=0.0.9, <0.1.0`). Because versions are lockstep, this is ONE range, not a per-crate matrix. | manifest |
| `docs_url` | Where a human reads the plugin's docs. | manifest / `package.documentation` |
| `repository` | Source repository URL. | manifest / `package.repository` |
| `maintenance` | Maintenance/support status: `Active`, `Maintenance`, `Deprecated`, `Unmaintained`. | manifest |
| `security` | Security posture: `Supported` (fixes shipped), `EndOfLife`, or `Advisory { rustsec: Option<String> }`. | manifest |
| `owns_migrations` | Whether this plugin ships its own migrations (owns a `migrations/<name>/` tree and rows in the tracking table). | manifest, cross-checked against `Plugin::models()` |
| `capabilities` | Which `Plugin`-trait capabilities the plugin actually contributes (§2.2). | manifest, cross-checked against the live trait methods |
| `required_settings` | Settings keys the plugin needs to boot, with type and whether each is a secret (§2.3). | manifest |

`name`, `owns_migrations`, and `capabilities` are *cross-checkable*: the framework already walks the live `Plugin` object at boot, so a manifest that claims `owns_migrations = true` while `Plugin::models()` is empty (or vice versa) is a declared-vs-actual drift the compatibility check can flag as a warning. This is the same drift-avoidance philosophy the trait already applies to `routes()` vs `route_paths()` (see `routes_builder`).

### 2.2 Capabilities: the REAL `Plugin`-trait methods

The manifest's `capabilities` set is drawn from the actual overridable methods on the `Plugin` trait in `crates/umbral-core/src/plugin.rs`. `name()` is mandatory and not a capability; every other method has an empty default and is opt-in. The complete set a plugin can contribute:

| Capability enum variant | Trait method | Contributes |
|---|---|---|
| `Dependencies` | `dependencies()` | Load-order edges (names of plugins that must load first) |
| `Models` | `models()` | `ModelMeta` -> migrations + ORM |
| `Routes` | `routes()` | Axum router merged into the app |
| `RoutesBuilder` | `routes_builder()` | Drift-free router + recorded `RouteSpec`s |
| `RoutePaths` | `route_paths()` | Declared `RouteSpec`s for discovery surfaces |
| `OpenApiPaths` | `openapi_paths()` | OpenAPI path items |
| `SystemChecks` | `system_checks()` | Boot-time `SystemCheck`s |
| `Storage` | `provides_storage()` | Registers a `Storage` backend in `on_ready` |
| `Database` | `database()` | Per-plugin DB alias routing |
| `TemplatesDirs` | `templates_dirs()` | Template search directories |
| `TemplateRegistrars` | `template_registrars()` | Custom minijinja filters/functions/globals |
| `WrapRouter` | `wrap_router()` | Raw tower `Layer` wrapping of the router |
| `Middleware` | `middleware()` | Ergonomic `before_request` / `after_response` middleware |
| `StaticFiles` | `static_files()` | Binary-embedded static assets |
| `StaticDirs` | `static_dirs()` | Namespaced on-disk static source directories |
| `StaticRootDirs` | `static_root_dirs()` | Root-level (un-namespaced) static directories |
| `Commands` | `commands()` | CLI subcommands (`PluginCommand`) |
| `ApiEndpoints` | `api_endpoints()` | Advertised endpoints for service discovery |
| `OnReady` | `on_ready()` | Startup lifecycle hook |

The manifest lists only the capabilities the plugin actually uses, so a reader (or a catalog card) can answer "does this plugin add routes? own migrations? ship CLI commands?" without compiling and booting it.

### 2.3 Required settings and secrets

Plugins read config from `Settings.extra` today via ad-hoc `from_settings` patterns (gaps5 #78). The manifest gives that a declarative face WITHOUT waiting on the full typed-settings-schema registry: each entry names the key, its expected type, whether it is required to boot, and whether it is a secret (so tools never print its value and system checks can flag a default/empty value).

```rust
pub struct RequiredSetting {
    /// Dotted key under Settings.extra, e.g. "billing.stripe_key".
    pub key: &'static str,
    /// Coarse type for docs + validation: String, Int, Bool, Url, Duration, List.
    pub ty: SettingType,
    /// Boot fails (Error) if absent; else a Warning.
    pub required: bool,
    /// Redact in logs/catalog; never echoed. Feeds a "secret is still empty" check.
    pub secret: bool,
    /// Environment variable that overrides it, if any.
    pub env: Option<&'static str>,
    /// One-line human description for generated docs.
    pub help: &'static str,
}
```

### 2.4 How the manifest is expressed: BOTH surfaces

There are two natural homes for this metadata, and they serve different readers, so the design uses both with a clear precedence.

**(a) A `Plugin` trait method returning a `PluginManifest`.** This is the *runtime* surface: available from the live `Box<dyn Plugin>` at boot, so the compatibility check and any in-process discovery endpoint can read it without parsing files. It gets a default impl so no existing plugin breaks, but the default is deliberately thin (it can fill `name` from `Plugin::name()` and leave the rest `Unknown`), which itself is a signal the plugin has not opted into the manifest contract yet.

```rust
// crates/umbral-core/src/plugin.rs, added to the Plugin trait:

/// Distribution + compatibility metadata for this plugin (gaps5 #6).
///
/// Default returns a minimal manifest carrying only `name()`; a plugin
/// that wants boot-time compatibility checking and catalog discovery
/// overrides this. The framework cross-checks the declared
/// `capabilities` and `owns_migrations` against the live trait object
/// and warns on drift.
fn manifest(&self) -> PluginManifest {
    PluginManifest::minimal(self.name())
}
```

```rust
// The struct (lives in umbral-core, re-exported from the facade under
// `umbral::plugin::PluginManifest`; NOT in the prelude per STABILITY.md
// tiering - this is power-user/tooling surface, not everyday handler code).

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub crate_name: String,
    pub version: semver::Version,
    /// The umbral version range this plugin supports. ONE range because
    /// all umbral crates share a lockstep version (STABILITY.md).
    pub umbral_req: semver::VersionReq,
    pub docs_url: Option<String>,
    pub repository: Option<String>,
    pub maintenance: MaintenanceStatus,
    pub security: SecurityStatus,
    /// True if the plugin owns a migrations/<name>/ tree. Cross-checked
    /// against Plugin::models() at boot.
    pub owns_migrations: bool,
    /// Which Plugin-trait capabilities this plugin contributes (§2.2).
    pub capabilities: Vec<Capability>,
    pub required_settings: Vec<RequiredSetting>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum MaintenanceStatus { Active, Maintenance, Deprecated, Unmaintained, Unknown }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SecurityStatus {
    Supported,
    EndOfLife,
    Advisory { rustsec: Option<String> },
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Capability {
    Dependencies, Models, Routes, RoutesBuilder, RoutePaths, OpenApiPaths,
    SystemChecks, Storage, Database, TemplatesDirs, TemplateRegistrars,
    WrapRouter, Middleware, StaticFiles, StaticDirs, StaticRootDirs,
    Commands, ApiEndpoints, OnReady,
}
```

**(b) `[package.metadata.umbral]` in the plugin crate's `Cargo.toml`.** This is the *static* surface: readable by `cargo metadata` and by a catalog crawler WITHOUT building or running the crate. It is the source a registry index (§4) is generated from. Cargo ignores unknown `package.metadata.*` keys, so this is inert to the build.

```toml
# In a plugin crate's Cargo.toml
[package]
name = "acme-billing"
version = "1.2.0"
documentation = "https://docs.rs/acme-billing"
repository = "https://github.com/acme/acme-billing"

[package.metadata.umbral]
plugin_name = "billing"           # must equal Plugin::name()
umbral_req = ">=0.0.9, <0.1.0"    # the supported lockstep range
maintenance = "active"
security = "supported"
owns_migrations = true
capabilities = ["models", "routes", "commands", "middleware", "on_ready"]

[[package.metadata.umbral.required_settings]]
key = "billing.stripe_key"
ty = "string"
required = true
secret = true
env = "STRIPE_KEY"
help = "Stripe secret API key used to create charges."
```

**Precedence and reconciliation.** The `Cargo.toml` block and the `manifest()` method must agree. The blessed authoring path is: write the `[package.metadata.umbral]` block, then let a derive (`#[derive(PluginManifest)]`, future work) generate the `manifest()` impl from it via `env!("CARGO_PKG_VERSION")`, `env!("CARGO_PKG_REPOSITORY")`, and a build-script-embedded copy of the metadata block. Until that derive exists, plugins hand-write `manifest()` and the two are kept in sync by the author. The RUNTIME check (§3) trusts `manifest()` (it is what is actually loaded); the CATALOG (§4) is generated from `Cargo.toml`. A future lint can diff the two.

## 3. Boot-time compatibility check

### 3.1 Where it lives

This is a framework built-in `SystemCheck`, not a per-plugin one, because it must reason about every plugin against the single running umbral version. It slots into the existing phase-4 system-check mechanism (`crates/umbral-core/src/check.rs`): `App::build()` already walks the sorted plugin list and runs `framework_checks()` plus each `Plugin::system_checks()`. We add one built-in with id `plugin.compat`.

The running umbral version is `env!("CARGO_PKG_VERSION")` read from `umbral-core` at compile time (unambiguous because of the lockstep-version policy in `STABILITY.md`). The check needs the live `Box<dyn Plugin>` list to call `manifest()` on each; `CheckContext` today carries `registered_plugin_names: &[&str]` but not the manifests, so this check requires a small `CheckContext` extension (add `plugin_manifests: &[PluginManifest]`, populated by `App::build` from the same walk that fills `registered_plugin_names`). That mirrors how `provides_storage` and `registered_plugin_names` were already threaded in.

### 3.2 What it does

For each registered plugin:

1. Read `manifest()`.
2. If `manifest.umbral_req` does NOT match the running umbral version, emit a finding.
   - **Default severity: `Error` (blocks boot).** A plugin loaded outside its declared range is exactly the class of failure this contract exists to catch, and `STABILITY.md` §Plugin compatibility says the check "warns," so the framework default is a `Warning` with an opt-in to hard-fail (`AppBuilder::strict_plugin_compat()`), OR the reverse. RESOLVE before implementation (see open question 1). The finding text names the plugin, its `umbral_req`, and the running version, and hints to upgrade the plugin or pin umbral.
3. If `manifest` is the thin default (opted out), emit a `Warning` (id `plugin.compat.undeclared`): the plugin declares no supported range, so compatibility cannot be verified.
4. **Drift cross-checks** (all `Warning`):
   - `owns_migrations = true` but `Plugin::models()` is empty, or `false` but non-empty.
   - A `Capability` is listed but the corresponding trait method returns empty at boot (best-effort: only the cheaply-observable ones, e.g. `Models`, `Commands`, `Routes`/`RoutePaths`, `Middleware`, `ApiEndpoints`).
   - `security = Advisory` with a RUSTSEC id: surface it loudly (`Warning`, or `Error` if `strict`) so an app cannot silently ship a plugin with a known advisory.
5. `required_settings` validation folds in here too: a `required` setting absent from `Settings.extra`/env is an `Error`; a `secret` setting left empty or at an obvious default is a `Warning` (reusing the same posture as the existing `settings.required` secret-key check).

Findings use the existing `SystemCheckFinding` shape with `CheckLocation::Plugin { plugin }`, so they render through the same boot report as every other check. `Severity::Error` returns `BuildError::SystemCheckFailed`; `Severity::Warning` logs via `tracing::warn!` and boot proceeds, exactly as `crates/umbral-core/src/check.rs` documents.

### 3.3 Tie to STABILITY.md

`STABILITY.md` §Plugin compatibility is the policy; this check is its enforcement:

- "Third-party plugins declare the umbral version range they support (via the plugin manifest, gaps5 #6)" -> the `umbral_req` field (§2.1).
- "A boot-time system check warns when an installed plugin falls outside its declared range" -> the `plugin.compat` check (§3.2).
- "Because versions are lockstep, 'compatible with umbral 0.0.x' is a single range" -> `umbral_req` is one `VersionReq`, matched against one running version.

The MSRV line in `STABILITY.md` is orthogonal (Rust version, enforced by Cargo's `rust-version`), so the manifest does not duplicate it; a plugin's MSRV is its own `Cargo.toml` `rust-version`.

## 4. Registry / catalog format

The catalog is a **static, machine-readable index of published plugins** generated from each crate's `[package.metadata.umbral]` block (§2.4b). It is data, not a service: a JSON document that a CLI (`umbral plugins search`, future) or a website can consume. Hosting it is Stage 3 and out of scope; the FORMAT is in scope so tooling and the eventual marketplace agree on the shape.

### 4.1 Index shape

```json
{
  "catalog_version": "1",
  "generated_at": "2026-08-08T00:00:00Z",
  "plugins": [
    {
      "name": "billing",
      "crate_name": "acme-billing",
      "versions": [
        {
          "version": "1.2.0",
          "umbral_req": ">=0.0.9, <0.1.0",
          "docs_url": "https://docs.rs/acme-billing",
          "repository": "https://github.com/acme/acme-billing",
          "maintenance": "active",
          "security": "supported",
          "owns_migrations": true,
          "capabilities": ["models", "routes", "commands", "middleware", "on_ready"],
          "required_settings": [
            { "key": "billing.stripe_key", "ty": "string", "required": true, "secret": true, "env": "STRIPE_KEY" }
          ],
          "published_at": "2026-08-01T00:00:00Z",
          "yanked": false
        }
      ]
    }
  ]
}
```

Notes:

- The unit is a plugin *name* with an array of published *versions*, because compatibility is resolved per version (`umbral_req` can widen or narrow across releases).
- Secret VALUES never appear; only the `required_settings` shape (key/type/secret-flag) is published, so the catalog documents what an operator must supply without leaking anything.
- `catalog_version` lets the format evolve; `"1"` is this draft.
- The catalog is derivable offline: `cargo metadata` over a set of crates + crates.io release timestamps is enough to produce it. No running umbral app is required.

### 4.2 Generation

A CLI subcommand (`umbral plugins index`, future work under the `umbral-cli` crate) crawls a provided list of crates (or a local workspace) and emits the JSON above. Because the source is `[package.metadata.umbral]`, a third-party plugin needs zero umbral-specific tooling to be indexable: it just fills the Cargo metadata block and publishes to crates.io as usual.

## 5. Path to a marketplace (gaps5 #90) - OUT OF SCOPE here, contract only

gaps5 #90 (tf#303) wants "plugin signing, verified publishers, security badges, and compatibility metadata." This spec delivers the last of those (compatibility metadata) and shapes the index so the first three attach cleanly LATER, without a format break:

- **Compatibility badges** render directly from `umbral_req` + the drift/`security` fields already in the catalog. No new data needed.
- **Security badges** render from `security` (`Supported` / `EndOfLife` / `Advisory { rustsec }`). A green/amber/red badge is a pure function of that field plus a RUSTSEC cross-reference.
- **Verified publishers** attach as an OPTIONAL `publisher` object on each catalog entry (identity + verification method), added under a bumped `catalog_version`. Absent today; the array-of-versions shape leaves room for it.
- **Signing** attaches as an OPTIONAL `signature` object (detached signature over the version entry's canonical JSON, plus the signing key id). The catalog entry is already a self-contained record, so signing it is additive.

What is deliberately NOT in this spec: the registry *service* (hosting, upload, auth, moderation, the review/certification workflow), the signing *infrastructure* (key custody, rotation, trust roots), and any UI. Those are Stage 3 platform work. This spec is the manifest + compatibility contract that Stage 3 stands on.

## 6. Interactions and non-goals

- **Not the typed-settings-schema registry (gaps5 #78).** `required_settings` here is a lightweight declaration for compatibility + docs, not the full typed config schema with validation. When #78 lands, the manifest's `required_settings` becomes a projection of that schema rather than a parallel source.
- **Not the plugin compliance/certification harness (gaps5 #81).** That harness *tests* a plugin; this manifest *describes* one. They are complementary: the harness can assert that a plugin's declared `capabilities` match what its tests exercise.
- **Not route discovery (gaps5 #77).** The `RoutePaths`/`RoutesBuilder` capability flags say *whether* a plugin contributes routes; the actual route list stays with `route_paths()` / `routes_builder()`.
- **No new global.** The manifests are read from the existing plugin walk in `App::build`; nothing is stashed in a `OnceLock`. This respects the "one intentional global" rule (the `DbPool`) in `CLAUDE.md`.

## 7. Open questions

1. **Default severity of `plugin.compat` out-of-range**: hard `Error` by default (safest) with an escape hatch, or `Warning` by default (matches the literal `STABILITY.md` wording "warns") with `AppBuilder::strict_plugin_compat()` to escalate? Recommend: `Warning` default to honor the current stability wording, `Error` under strict, and revisit at 1.0.
2. **Derive macro timing**: ship `#[derive(PluginManifest)]` (reads `[package.metadata.umbral]` via a build script) with this contract, or land the struct + trait method first and add the derive once a second third-party plugin exists to validate the ergonomics?
3. **Advisory severity**: should `security = Advisory { rustsec }` be an `Error` under strict mode, or always a loud `Warning`? It overlaps with a future `cargo-audit` CI gate (gaps5 #20/#99); decide whether boot-time is the right place to enforce it at all, versus leaving advisories to CI.
