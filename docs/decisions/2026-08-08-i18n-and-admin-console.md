# Internationalization and the admin as an org/ops console

| | |
|---|---|
| **Status** | Draft (covers planning/gaps5.md #74 tf#287, #75 tf#288) |
| **Date** | 2026-08-08 |
| **Touches** | new plugin `umbral-i18n`; new admin surface built on `plugins/umbral-admin` (`AdminView`); composes with `plugins/umbral-tenants`, `plugins/umbral-sso`, `plugins/umbral-org`, `plugins/umbral-auth`, `plugins/umbral-permissions` |
| **Companions** | `docs/decisions/2026-08-08-enterprise-identity-design.md` (umbral-sso / umbral-org), `docs/decisions/2026-08-08-product-north-star.md`, `crates/umbral-core/src/templates.rs` (the minijinja seam), `plugins/umbral-admin/src/views.rs` (the AdminView seam) |

## Purpose and scope

Two backlog items, grouped in one doc because both extend surfaces that already exist rather than inventing new machinery, and because the admin console (#75) localizes through the same i18n plugin (#74):

- **gaps5 #74 (tf#287): i18n and l10n.** Translation catalogs, a locale resolver, minijinja `t()` / `tn()` filters, pluralization, date/number/timezone formatting, and admin string localization. Delivered as a new `umbral-i18n` plugin.
- **gaps5 #75 (tf#288): evolve the admin into an org/ops console.** Org and team management, an IdP-config UI over the `umbral-sso` / `umbral-org` models, a cross-plugin audit event explorer, a tenant switcher, support impersonation with an approval gate, and compliance exports (ties gaps5 #86 tf#299). Delivered as new `AdminView` pages plus a small set of admin-owned models, not a new plugin.

This is Stage 1/Stage 2 framework work per the product north star: every capability is a plugin (or an extension of one), there is no privileged core, and a REST-free, i18n-free, single-org app still compiles and runs with none of this present.

## What already exists (the seams we build on)

Accurate to the code as of this date. New work reuses these by their real names.

### The template engine (`crates/umbral-core/src/templates.rs`)

- The engine is a `minijinja::Environment<'static>` published once into a process-wide `ENGINE: OnceLock`. Rendering goes through the ambient `render(name, ctx)` accessor. In `Environment::Dev` the engine is rebuilt from disk per render (hot reload) via `build_env`.
- Filters and functions are registered on the `Environment` with `env.add_filter(name, closure)` and `env.add_function(name, closure)`. The built-ins already include several l10n-adjacent ones: `currency` (`register_currency_filter`, which already carries a `KES` symbol and thousands grouping), `now()` (`register_now_function`, a chrono `strftime` formatter), and `querystring_with`.
- **`TemplateRegistrar = Box<dyn Fn(&mut Environment<'static>) + Send + Sync>`** is the plugin seam for adding filters/functions/globals. A plugin returns them from `Plugin::template_registrars()`; `App::build` flattens every plugin's registrars into the process-wide `REGISTRARS: OnceLock` before the engine is built, and `build_env` applies them on the first build AND on every dev-mode rebuild (they are `Fn`, not `FnOnce`, on purpose). Registrars run AFTER the built-ins, so a plugin can deliberately override a built-in filter by re-registering the same name (minijinja's `add_*` overwrites). This is exactly where `t()` and `tn()` are registered.
- **Ambient per-request context via `tokio::task_local!`.** `CURRENT_USER`, `CURRENT_CSRF`, and `CURRENT_USER_LAZY` are set by middleware and read by `render` through `merge_ambient_context` / `merge_ambient_value`, which inject `user`, `csrf_token`, and `csrf_input` into every template context (the handler's own keys always win). The scope helpers `with_current_user(...)` and `with_current_csrf(...)` establish the task-local for the duration of a request. This is the precedent a `CURRENT_LOCALE` task-local follows exactly: a locale-resolution middleware scopes it, and `merge_ambient_value` injects `locale` / `LANGUAGE_CODE` into the context so a template reads `{{ locale }}` without a per-handler pass-through.
- Autoescape is decided by file extension via `set_auto_escape_callback` (`.html`/`.htm` escape, `.txt` does not). A custom formatter is already installed via `env.set_formatter` (rendering `None`/`Undefined` as empty). Per-plugin template directories come from `Plugin::templates_dirs()`, searched first-match-wins in topological order.

### Settings (`crates/umbral-core/src/settings.rs`)

- `time_zone: Option<String>` (Gap 106) already exists for marshalling naive datetimes (an IANA name like `Africa/Nairobi`, validated against the tz database). i18n's timezone formatting reads and extends this rather than inventing a parallel tz field.

### The admin (`plugins/umbral-admin`)

- **`AdminView`** (`src/views.rs`): a registered admin page not tied to a model, mounted at `{admin_base}/custom-views/{path}/`. Builder surface: `AdminView::new(path, title)`, `.with_subtitle(...)`, `.with_icon(...)` (Lucide icon name), `.with_group(...)` (sidebar group), `.with_permission(codename)` (permission gate), `.hide()` (routable but off the sidebar), `.section(WidgetSection)`, `.add_sections(...)`. Registered on the plugin via `AdminPlugin::view(AdminView)` / `.views(...)`.
- **Widget surface** (`src/widgets.rs`, re-exported from `lib.rs`): `WidgetKind`, `Widget`, `WidgetSection`, `WidgetDataFn`, `Span`, and the payload types `KpiPayload`, `BarPayload`, `LinePayload`, `DonutPayload`, `TablePayload`, `FeedPayload`, `HeatmapPayload`, `RadialPayload`, `ProgressPayload`, `CardPayload`, plus `WidgetFilter` / `WidgetFilterKind` for per-widget filter controls (period, select, etc.).
- **Permission enforcement is double-gated.** A `.with_permission(codename)` view is checked on page load AND at the per-widget data endpoint (`/api/dashboard/widgets/{key}/data`): `routes()` builds a `widget_gates` map (`widget_key -> codename`) that `dashboard_widget_data` enforces after `require_staff`, and the CSV export endpoint (`/api/dashboard/widgets/{key}/export.csv`) shares the same gate so an export can never read numbers the page could not. Custom views mount under the hyphenated `/custom-views/` namespace, which can never collide with a snake_case model table.
- **`AdminAuditLog`** (`src/models.rs`): the admin's append-only audit trail (`actor_user_id`, `action` = `"create"|"update"|"delete"|"action:<key>"`, `model` = SQL table, `object_id: Option<String>` PK-agnostic, `diff_summary`, `created_at`), written fire-and-forget via `log(...)`, surfaced read-only (`#[umbral(noedit)]` on every column). The cross-plugin audit explorer reads this table.
- **Model registration** via `AdminPlugin::register(AdminModel)` and `register_for(plugin_name, AdminModel)` so a plugin's own models group correctly in the sidebar.

### Tenancy (`plugins/umbral-tenants`)

- **`Tenant`** registry model (`schema_name`, `name`, `domain`, `is_active`), `current_tenant() -> Option<TenantKey>`, `RouteContext::new().with_tenant(TenantKey)`, and `umbral::db::route_context_scope(ctx, fut)`. The resolution middleware is installed via `Plugin::wrap_router`. The admin tenant switcher reuses `route_context_scope` to run a request under a chosen tenant.

### Enterprise identity (`docs/decisions/2026-08-08-enterprise-identity-design.md`)

- The IdP-config UI edits the models that design defines: `SsoProvider`, `SsoIdentity`, `SsoDomain` (umbral-sso) and `Org`, `OrgDomain`, `OrgMembership`, `ScimToken`, `GroupMapping` (umbral-org). Those are already normal admin models; the console adds a purpose-built editing view on top of the raw CRUD.

---

## Section A: `umbral-i18n` (gaps5 #74, tf#287)

### Plugin shape and composition

`umbral-i18n` is a new crate under `plugins/`. It depends on the `umbral` facade only. `Plugin::dependencies()` is empty: i18n needs neither auth nor sessions (locale can resolve from `Accept-Language`, a path prefix, or a cookie without a logged-in user). It contributes:

- a `Plugin::template_registrars()` set that adds the `t`, `tn`, `tctx`, `datetime`, `date`, `time`, `number`, and `tz` filters/functions to the minijinja `Environment`,
- a locale-resolution middleware via `Plugin::wrap_router` (the same hook `umbral-tenants` uses) that scopes a `CURRENT_LOCALE` task-local for the request,
- optional catalog-compile and catalog-extract CLI commands via `Plugin::commands()`,
- a typed settings block (default locale, supported locales, resolution order, cookie name).

The wiring a consumer writes:

```rust
App::builder()
    .plugin(
        I18nPlugin::new()
            .default_locale("en")
            .supported(&["en", "fr", "sw", "ar"])          // "ar" is RTL; see below
            .catalog_format(CatalogFormat::Fluent)          // or CatalogFormat::Gettext
            .locales_dir("./locales")                       // on-disk catalogs
            .resolve(&[LocaleSource::Path, LocaleSource::Cookie, LocaleSource::AcceptLanguage])
            .cookie_name("umbral_locale"),
    )
```

### Catalog format: Fluent as the default, gettext as the compatibility path

Two formats, chosen behind `CatalogFormat`, because they serve different populations:

- **Fluent** (Mozilla's `fluent-rs`) is the default. Its message model handles gender, plural categories, and nested selectors natively, which is what a modern app actually needs and what a naive gettext `msgid`/`msgstr` map handles poorly. A message reads as `welcome = Welcome, { $name }` and a plural as a `{ $n ->  [one] ... *[other] ... }` selector, so pluralization is a property of the message rather than a separate `tn()` call.
- **gettext** (`.po` / `.mo`, via a `gettext`-family crate) is the compatibility on-ramp. Existing projects and existing translator tooling (Weblate, Crowdin, Poedit) speak `.po`, so a team with an established localization pipeline drops their catalogs in unchanged. gettext plural forms are honored via the `Plural-Forms` header expression.

Do not reimplement the primitive: catalog parsing, plural-rule evaluation, and message formatting stand on `fluent-rs` (Fluent) or a `gettext` crate (gettext). The plugin owns resolution, caching, and the minijinja glue, not the format engine.

Catalogs live under `locales/<locale>/<domain>.ftl` (Fluent) or `locales/<locale>/LC_MESSAGES/<domain>.po` (gettext). A plugin ships its own catalogs from its `templates_dirs()`-adjacent `locales/` directory so a translated string is co-located with the plugin that owns it (the admin's own strings ship this way, see Section B). Catalogs are loaded and compiled once at boot into an in-memory bundle keyed by locale; in `Environment::Dev` they reload on change alongside the template hot-reload path.

### The locale resolver

A middleware (mounted via `wrap_router`) resolves one locale per request and scopes it into a `CURRENT_LOCALE` task-local, mirroring how `CURRENT_USER` / `CURRENT_CSRF` are scoped. Resolution walks the configured `resolve(&[...])` order and takes the first source that yields a supported locale:

1. **Path prefix** (`LocaleSource::Path`): a leading `/<locale>/` segment (`/fr/products`). When enabled, the middleware strips the prefix before the inner router sees the path and records the active locale, so route handlers stay locale-agnostic. This is the "locale routing" the gap asks for.
2. **Cookie** (`LocaleSource::Cookie`): the `umbral_locale` cookie, set by a language switcher.
3. **`Accept-Language`** (`LocaleSource::AcceptLanguage`): parsed and quality-ranked against the supported set, with language-range fallback (`fr-CA` matches `fr`).
4. **Default**: the configured `default_locale`, always the final fallback.

A logged-in user's stored preference, when auth is present, can be layered in front of the cookie by an app that sets the cookie from the profile; the plugin does not depend on auth to do this. The resolved locale, its text direction (`ltr` / `rtl`, derived from the language), and the active catalog handle are all readable from a `current_locale()` accessor for Rust-side callers (mirroring `current_csrf()`).

### minijinja filters and functions

Registered through `template_registrars()`, so they exist in the same `Environment` as `currency` and `now()` and survive dev rebuilds:

| Call | Purpose |
|---|---|
| `{{ "welcome" \| t }}` | Translate a key in the active locale (ambient from `CURRENT_LOCALE`). |
| `{{ "welcome" \| t(name=user.username) }}` | Translate with named arguments (Fluent placeables / gettext interpolation). |
| `{{ "cart.items" \| tn(count=n) }}` | Plural-aware translate; picks the plural category for `n` in the active locale. |
| `{{ "menu.file" \| tctx("noun") }}` | Contextual translate (gettext `msgctxt` / a Fluent term namespace) to disambiguate a homograph. |
| `{{ order.placed_at \| datetime(format="medium") }}` | Locale-aware date+time, formatted in the active timezone. |
| `{{ order.placed_at \| date }}` / `\| time` | Locale-aware date-only / time-only. |
| `{{ total \| number }}` | Locale-aware number grouping and decimal separator. |
| `{{ ts \| tz("Africa/Nairobi") }}` | Render a timestamp in an explicit timezone, overriding the ambient one. |

`t` and `tn` read the ambient locale from the task-local, so a template author never threads a locale argument. `merge_ambient_value` additionally injects `locale` (the BCP-47 tag) and `dir` (`ltr`/`rtl`) into every context, so a base template can write `<html lang="{{ locale }}" dir="{{ dir }}">` with no handler change. A missing key falls back to the key text (never a render error), and logs once per key at `debug` so untranslated strings are discoverable without breaking the page.

The existing `currency` filter is extended to consult the active locale for symbol placement and grouping when no explicit code is passed, rather than being replaced, so pre-i18n templates keep working byte-for-byte.

### Pluralization

Pluralization is delegated to the format engine's plural rules (Fluent's `PluralRules`, gettext's `Plural-Forms`), which implement the CLDR plural categories (`zero`, `one`, `two`, `few`, `many`, `other`) per language. `tn(count=n)` evaluates the category for `n` in the active locale and selects the matching variant. The plugin never hardcodes an English `n == 1` rule; a language with three plural forms (for example Polish or Arabic) gets the right form because the rule set, not the framework, decides.

### Date, number, and timezone formatting

- **Timezone.** Naive datetimes are already marshalled through `Settings.time_zone` (Gap 106). i18n adds a per-request timezone (resolvable from the user profile or a cookie) that, when present, overrides the global default for the `datetime` / `date` / `time` filters; the `tz(name)` filter is the explicit-override escape hatch. Conversion uses `chrono-tz`, already a dependency of the settings tz support.
- **Numbers and dates.** Locale-aware formatting (grouping separator, decimal mark, date field order, month/day names) stands on an ICU-backed crate (`icu` / `icu_datetime`, `icu_decimal`) rather than a hand-rolled table, so the format matches CLDR data. The `format="short|medium|long|full"` skeleton mirrors ICU's named styles.

### Admin string localization

The admin's own chrome (sidebar labels, table headers, form labels, action buttons, flash messages) is localized by shipping the admin's English strings as an `umbral-admin` catalog domain and rendering them through the same `t` filter. Because the admin owns a private minijinja `Environment`, it registers the i18n filters into that environment too (the same way it already registers a `static()` equivalent via `resolve_static_url`), so `t` resolves inside admin templates. Model-level display names (`#[umbral(display = "...")]`) and field labels can carry a translation key so a model's admin label localizes; when i18n is absent the raw display string renders unchanged. This keeps the admin fully usable with zero i18n config and localized the moment the plugin is present with a catalog.

### Security and correctness considerations (Section A)

- **No injection through translations.** Translated strings render through the same autoescape path as any template value (`.html` templates escape). A catalog is developer/translator-supplied content, not end-user input, but placeables that interpolate user data (`t(name=user.username)`) are escaped by minijinja exactly as `{{ user.username }}` would be. A translator cannot smuggle markup into a page.
- **Locale is untrusted input.** The path/cookie/header locale is validated against the configured `supported` allowlist before use; an unknown or malformed locale falls to the default. A locale value never reaches a filesystem path (catalogs are loaded at boot by supported-locale name, not per-request), closing any path-traversal vector.
- **Fail-open on missing translations, fail-safe on everything else.** A missing key renders the key; a missing catalog renders the default locale; a malformed catalog fails the boot (a broken translation file is a deploy-time error, not a per-request surprise).

### Config / settings (Section A)

Builder plus env fallbacks under `UMBRAL_I18N_*`: `default_locale`, `supported`, `catalog_format`, `locales_dir`, resolution order, `cookie_name`, and an optional `per_request_timezone` toggle. `I18nPlugin::from_settings(&Settings)` exists so a full env-only deployment works.

---

## Section B: the admin as an org/ops console (gaps5 #75, tf#288)

### Shape: `AdminView` pages plus a thin model layer, not a new plugin

The console is built almost entirely from the existing `AdminView` + widget surface, registered on the running `AdminPlugin`. It adds a small set of admin-owned models only where a new workflow needs persisted state (impersonation grants, compliance export jobs). Everything else reads models that already exist (`Tenant`, `SsoProvider`, `Org`, `OrgMembership`, `AdminAuditLog`, the permissions group graph) through the ORM. This keeps the console a plugin-native feature: no privileged core, and an app that installs only `umbral-admin` still gets plain CRUD with none of these pages.

Each console page is an `AdminView` gated by `.with_permission(codename)`, so the double gate (page load and per-widget data endpoint) already enforces access. Console permissions live in the `umbral-permissions` graph like any other codename (`console.manage_org`, `console.view_audit`, `console.switch_tenant`, `console.impersonate`, `console.export_compliance`).

The consumer opts in:

```rust
AdminPlugin::default()
    .views(umbral_admin::console::org_console_views())   // returns Vec<AdminView>
```

### B1: Org and team management

An `AdminView` at `console/orgs` renders the `Org` / `OrgMembership` graph from `umbral-org`: a `TablePayload` of orgs, and a per-org drill-down showing the active/deprovisioned member roster (`OrgMembership.status`), roles (`member`/`admin`/`owner`), and the provisioning source (`scim`/`jit`/`manual`). Membership edits (invite, change role, suspend) go through the ORM against `OrgMembership` and, for group changes, through the permission membership helpers (`set_user_groups`, `add_user_to_group`) rather than raw writes. Team management is the same view scoped to a group subset. When `umbral-org` is absent the view is simply not registered (the `console::org_console_views()` builder omits pages whose backing plugin is unregistered, the same typo-safe pattern `dashboard_models_only` uses).

### B2: IdP-config UI over the umbral-sso / umbral-org models

`SsoProvider`, `SsoDomain`, `Org`, `OrgDomain`, `ScimToken`, and `GroupMapping` are already registerable admin models. The console adds a purpose-built `AdminView` at `console/identity` that composes them into one operator workflow instead of six disconnected changelists:

- Add/edit an SSO provider (OIDC discovery URL or SAML metadata URL), with a "Refresh discovery/metadata now" and "Test login" action per provider (the same per-provider admin actions the enterprise-identity design already calls for).
- Show each `OrgDomain`'s verification state and the DNS `TXT` record to publish, with a "Verify now" action that runs the DNS check.
- Manage `ScimToken`s (generate, shown-once, revoke) and the `GroupMapping` claim-to-group rules.

Every secret (`SsoProvider.client_secret`, `ScimToken` value, SAML certs) renders through the `Masked` redaction already used by those fields, so the config UI never re-exposes a stored secret. This is a view over models the other design owns; it adds no new identity logic.

### B3: Cross-plugin audit event explorer

Today `AdminAuditLog` is a single admin-owned table surfaced as a read-only changelist. The explorer at `console/audit` turns it into a real investigation surface with a `TablePayload` + `FeedPayload` widget backed by a `WidgetDataFn` that queries `AdminAuditLog` through the ORM with `WidgetFilter` controls for actor, action, model/table, object id, and time range (period filter). "Cross-plugin" means any plugin can contribute audit rows: rather than each plugin inventing its own audit table, the console defines a small `AuditSink` seam (an ORM-backed `audit(actor, action, target, summary, metadata)` call that writes `AdminAuditLog`, generalizing the existing fire-and-forget `log(...)`), so auth events, SSO logins, SCIM deprovisions, and impersonation grants all land in one queryable stream. The explorer never writes; it reads, filters, and exports (via the shared widget CSV export endpoint, gated by the same permission).

This is deliberately the read/query layer. Tamper-evident hash-chaining and an external WORM sink are gaps5 #87 (tf#300) and are noted as the follow-up that hardens the sink underneath this explorer; the explorer works over the existing table today and gains integrity guarantees when #87 lands.

### B4: Tenant switcher

For an operator on a multi-tenant deployment (`umbral-tenants` present), a switcher in the admin topbar lists active `Tenant` rows and lets an authorized operator view the admin AS a chosen tenant. Mechanically this reuses the tenancy foundation directly: the switcher sets a scoped tenant on the admin request via `RouteContext::new().with_tenant(TenantKey::new(schema_name))` wrapped in `route_context_scope`, so every ORM read on that admin page routes to the tenant's schema with zero extra machinery. The switch is permission-gated (`console.switch_tenant`) and, crucially, still passes through any installed `TenantMembership` guard, so a switch cannot become a way to reach a tenant the operator is not bound to. Every switch writes an audit row. When `umbral-tenants` is absent the switcher is not rendered.

### B5: Support impersonation with an approval gate

Impersonation lets a support engineer act as a specific end user to reproduce an issue, behind an approval workflow so it is never unilateral. A new admin-owned model records the grant:

```rust
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(display = "Impersonation grants", icon = "user-check")]
pub struct ImpersonationGrant {
    pub id: i64,
    #[umbral(on_delete = "cascade")] pub requester: ForeignKey<AuthUser>,   // the support engineer
    #[umbral(index, max_length = 64)] pub subject_user_id: String,          // PK-agnostic target
    #[umbral(max_length = 200)] pub reason: String,
    #[umbral(index, max_length = 16)] pub status: String,   // "pending" | "approved" | "denied" | "active" | "ended"
    pub approver: Option<ForeignKey<AuthUser>>,             // the second person who approved
    pub approved_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,                  // grants are time-boxed
    #[umbral(auto_now_add)] pub created_at: DateTime<Utc>,
}
```

The flow:

1. A support engineer requests impersonation of a target user with a written `reason`; a `pending` `ImpersonationGrant` is created.
2. A second authorized person (holding `console.approve_impersonation`, never the requester) approves or denies. Two-person rule: the approver id must differ from the requester id, enforced server-side.
3. On approval, the engineer starts a time-boxed impersonated session. The session is minted through `umbral-sessions` exactly like a normal login (so session-id rotation and the single login terminal are inherited), but flagged as impersonated and carrying the grant id. A persistent banner marks every impersonated page, and mutating actions can be restricted by policy.
4. Ending the impersonation (or `expires_at` passing) closes the session. Request, approval, start, and end each write an audit row into the cross-plugin stream (B3), so the whole episode is reconstructable.

This reuses the auth login terminal and the session rotation rather than minting a session row directly, and it reuses the `AuthChallenge`-style TTL/single-use posture for the grant. It never bypasses MFA or the normal inactive-user gate on the target.

### B6: Compliance exports (ties gaps5 #86 tf#299)

An `AdminView` at `console/compliance` drives export/delete workflows tied to model metadata. The console layer here is the operator-facing surface; the heavy lifting (DSAR data-subject-access export, retention automation, consent ledger, processing-purpose metadata) is gaps5 #86 and is expected to arrive as a `umbral-compliance` plugin whose models this view edits, the same relationship the identity console (B2) has with `umbral-sso`/`umbral-org`. What the console owns:

- A small `ComplianceExportJob` admin-owned model (subject, kind = `export`|`erasure`, status, requested_by, approver, artifact key) so an export is an approvable, auditable, resumable job rather than an ad-hoc script.
- A "request export for subject" / "request erasure for subject" action that creates a `pending` job, an approval gate (export and especially erasure of a user's data are second-person-approved, reusing the B5 two-person rule), and a completed-artifact download that streams the generated bundle through the existing widget/export endpoint machinery, gated by `console.export_compliance`.
- The generated artifact is produced by the `umbral-compliance` plugin (#86) walking model metadata for fields marked as personal data; when that plugin is absent the export view degrades to "no compliance provider installed" rather than fabricating an incomplete export.

### Admin surface and permissions summary (Section B)

| Page (`AdminView`) | Backing models / seam | Permission | Absent when |
|---|---|---|---|
| `console/orgs` | `Org`, `OrgMembership`, permission helpers | `console.manage_org` | `umbral-org` unregistered |
| `console/identity` | `SsoProvider`, `SsoDomain`, `OrgDomain`, `ScimToken`, `GroupMapping` | `console.manage_idp` | `umbral-sso`/`umbral-org` unregistered |
| `console/audit` | `AdminAuditLog` via `AuditSink` | `console.view_audit` | never (core admin model) |
| tenant switcher (topbar) | `Tenant`, `route_context_scope`, `TenantMembership` | `console.switch_tenant` | `umbral-tenants` unregistered |
| `console/impersonation` | `ImpersonationGrant`, sessions login | `console.impersonate` / `console.approve_impersonation` | never (admin-owned) |
| `console/compliance` | `ComplianceExportJob`, `umbral-compliance` metadata | `console.export_compliance` | export/erasure artifacts need `umbral-compliance` (#86) |

All row-level reads and writes go through the ORM (no raw `sqlx::query` in the plugin), per the plugin rule. Every new model is migrated through the normal loop. The console localizes through Section A's `t` filter, so a French operator gets a French console once an `umbral-admin` French catalog exists.

---

## Phasing (honest sequencing)

Each phase is independently useful and independently reversible.

1. **Phase 1: i18n core (`umbral-i18n`, gettext + Fluent).** The locale resolver middleware + `CURRENT_LOCALE` task-local, `t`/`tn`/`tctx`, catalog loading, and `merge_ambient_value` injection of `locale`/`dir`. Highest leverage, unblocks admin localization. Ships with one format engine wired end-to-end (Fluent) and the gettext loader behind it.
2. **Phase 2: i18n formatting.** `datetime`/`date`/`time`/`number`/`tz` filters on the ICU + chrono-tz stack, and the per-request timezone override layered onto Gap 106. Separable from Phase 1 because translation is useful before locale-aware number/date formatting is.
3. **Phase 3: admin localization.** Ship the `umbral-admin` English catalog and register the i18n filters into the admin's private environment. Proves the plugin against a real, large catalog.
4. **Phase 4: console read surfaces (`console/audit`, tenant switcher, `console/orgs`, `console/identity`).** All read/compose over existing models; no new persisted state except the `AuditSink` generalization. Lowest risk of the console work.
5. **Phase 5: console write workflows (impersonation, compliance exports).** The two flows that add models and approval gates. Sequenced last because they carry the real security weight (two-person approval, time-boxed impersonated sessions) and because compliance exports depend on the #86 plugin for the artifact.

Follow-ups explicitly not gating any phase: tamper-evident audit hash-chaining and a WORM sink (gaps5 #87), the full `umbral-compliance` plugin (gaps5 #86), and RTL-specific admin CSS polish (the `dir` attribute lands in Phase 1; a fully mirrored admin layout is a later slice).

## Cross-cutting summary

| Concern | i18n (A) | Console (B) |
|---|---|---|
| Ambient per-request state | `CURRENT_LOCALE` task-local, mirroring `CURRENT_USER`/`CURRENT_CSRF` | `route_context_scope` tenant, mirroring the tenants middleware |
| Template seam | `Plugin::template_registrars()` adds `t`/`tn`/formatting filters | `AdminView` + widget payloads render the pages |
| Untrusted input | locale validated against the `supported` allowlist; no per-request FS path | tenant switch passes the `TenantMembership` guard; impersonation is two-person-approved |
| Persistence | catalogs on disk, compiled once at boot | ORM-only; new models (`ImpersonationGrant`, `ComplianceExportJob`) migrated normally |
| Absent-dependency behavior | no i18n plugin -> raw strings render, admin fully usable | a console page is not registered when its backing plugin is absent |

Every stored secret in the identity console renders through `Masked`; every console mutation writes an `AdminAuditLog` row through the generalized `AuditSink`.

## Open questions for the maintainer

1. Catalog default: Fluent (richer message model, less translator-tool support today) vs gettext (universal tooling, weaker plural/gender model). The draft defaults to Fluent with gettext as a first-class alternative; a gettext default is defensible if translator-workflow compatibility outranks message expressiveness.
2. Whether locale routing (`/fr/...` path prefix) should be a first-class default or opt-in. The draft makes it one configurable source among cookie/header, off unless listed, so URLs stay clean for apps that do not want per-locale paths.
3. Whether the org/ops console should stay inside `umbral-admin` (as this draft proposes, since it is pure `AdminView` composition) or become a separate `umbral-console` plugin. Separating it keeps `umbral-admin` lean but duplicates the widget/permission wiring; folding it in keeps one admin surface. The draft folds it in and gates each page on plugin presence.
4. Impersonation policy: whether an impersonated session should be strictly read-only by default, or allow mutations behind an extra confirmation. The draft allows policy-restricted mutations but defaults to a visible banner + full audit rather than a hard read-only lock, on the argument that reproducing a write bug sometimes needs the write.
5. Whether admin string localization should key off `#[umbral(display = "...")]` values directly (so existing display strings become translation keys automatically) or require an explicit translation-key attribute. The draft supports both: an explicit key wins, the display string is the fallback key.
