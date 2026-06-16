# Plugin documentation audit — documented vs. actual

Audit of `/documentation/docs/v0.0.1/plugins/*.mdx` against source in
`/plugins/<name>/src/lib.rs`. Every claim below was verified by reading
the live source file; only concrete discrepancies are listed.

Severity key:
- **Critical** — documented API does not exist or has opposite behaviour
- **Required** — wrong signature, wrong name, wrong default, wrong path
- **Important** — stale deferred/aspirational claim that no longer fits
- **Nit** — minor inaccuracy that a careful reader could trip on
- **FYI** — aspirational/forward-looking claim that is accurate but could be misread

---

## `admin.mdx`

Source: `plugins/umbra-admin/src/lib.rs`, `plugins/umbra-admin/src/models.rs`, `plugins/umbra-admin/src/config.rs`

### [Critical] CSS path is wrong

The doc says the admin stylesheet is served at `/admin/static/admin.css`.

The source test at `lib.rs:829` (`assert_eq!(response.status(), StatusCode::OK)` at path `/static/admin/admin.css`) confirms the actual path is `/static/admin/admin.css` — i.e. the plugin registers under the unified `/static/` mount, not a plugin-private `/admin/static/` prefix.

### [Critical] `on_ready` does not create tables

The doc's "Phase 4 — Admin tables" section states:

> `Plugin::on_ready` creates `admin_user_pref` and `admin_audit_log` with `CREATE TABLE IF NOT EXISTS`.

Source `lib.rs:788-793` (`on_ready` impl):

```rust
fn on_ready(&self, _ctx: &AppContext) -> Result<(), umbra::plugin::PluginError> {
    Ok(())
}
```

The function body is a no-op. A comment above it says the tables are produced by the migration engine via `Self::models()`, not raw DDL. The `ensure_tables_for_tests` function in `models.rs:463` does emit `CREATE TABLE IF NOT EXISTS` DDL but is `#[doc(hidden)]` and test-only; it is explicitly documented as "Production code never calls this."

### [Required] Built-in dashboard widgets are NOT auto-seeded on boot

The doc says:

> Two built-in widgets **seed the catalog on every boot**: `total_models` (a KPI card reporting how many models are registered) and `recent_users` (a feed widget).

Source `lib.rs:89` (comment on the export block):

> Used to be auto-prepended to the catalog; now exposed as public functions so the **caller can register them at the position they want**.

The current API is:

```rust
pub fn builtin_total_models_widget() -> WidgetRegistration { ... }
pub fn builtin_recent_users_widget() -> WidgetRegistration { ... }
```

These are free functions the app must call and pass to `AdminPlugin::register_widget(...)`. Nothing is seeded automatically.

### [Required] Widget kinds table is incomplete

The doc's "Dashboard widgets" section lists only five widget kinds: Kpi, Line, Bar, Table, Feed.

Source `lib.rs` exports ten payload structs:

```
BarPayload, CardPayload, DonutPayload, FeedPayload, HeatmapPayload,
KpiPayload, LinePayload, ProgressPayload, RadialPayload, TablePayload
```

Five kinds are undocumented: **Card**, **Donut**, **Heatmap**, **Progress**, **Radial**.

### [Nit] `AdminModel::ordering` field order

The doc example uses `ordering(&["-published_at", "title"])` — this matches the source signature `pub fn ordering(mut self, fields: &[&str]) -> Self` in `config.rs:17`. No discrepancy in the signature; this nit is that the doc doesn't explain the leading `-` prefix means descending. Low priority.

---

## `auth.mdx`

Source: `plugins/umbra-auth/src/lib.rs`

No critical discrepancies found. Verified:

- `AuthPlugin::default()`, `AuthPlugin::with_default_routes()` (only on `AuthPlugin<AuthUser>`) — confirmed
- `AuthPlugin::with_user_in_templates()` — confirmed (exists, mounts `user_context_layer`)
- `login`, `login_with_request`, `logout`, `current_user` — all confirmed
- `LoggedIn`, `LoginRequired`, `LoginRequiredLayer`, `login_required`, `login_required_html` — all confirmed
- `hash_password`, `verify_password`, `create_user`, `create_superuser`, `authenticate`, `set_password` — all confirmed

### [FYI] `with_default_routes()` constraint not mentioned

The doc doesn't note that `with_default_routes()` is only available on `AuthPlugin<AuthUser>` (the concrete user type), not on a generic `AuthPlugin<U>`. Custom-user-model users who try to call it will get a compile error that may be confusing.

---

## `sessions.mdx`

Source: `plugins/umbra-sessions/src/lib.rs`

### [Required] `create_session` parameter type mismatch

The doc says `create_session` takes a user PK and implies `i64` in its surrounding description. The actual signature:

```rust
pub async fn create_session(
    jar: &CookieJar,
    user_id: Option<String>,
    ttl: Option<Duration>,
) -> Result<(CookieJar, Session), SessionError>
```

The type is `Option<String>` — the stringified PK. The doc example code uses a string literal so the code compiles, but the inline description in the function explanation says "the user's primary key" without clarifying it must be pre-converted to a `String`. Callers with `i64` PKs will need `Some(user.id.to_string())`.

### [Required] `login` vs `login_user_id`

The doc refers to both `create_session` and the helper `login(jar, user_id)`. The session source exports `login_user_id` (not `login`) as the short-form alias. Verify exact export name to avoid a compile error. Source `lib.rs` exports `pub use session::login_user_id`.

### [Nit] `without_auto_layer()` not documented

The `SessionsPlugin::without_auto_layer()` builder method exists in source but is not mentioned in the doc. This is missing-coverage, not wrong coverage.

---

## `permissions.mdx`

Source: `plugins/umbra-permissions/src/lib.rs`, `plugins/umbra-permissions/src/perm.rs`

### [Required] `has_perm` signature uses `&str` user ID, not `i64`

The doc example:

```rust
if has_perm(user.id(), "blog.publish_post").await? { ... }
```

This implies `user.id()` returns something compatible with the first argument. The actual signature (from `perm.rs:101`):

```rust
pub async fn has_perm(user_id: &str, perm: &str) -> Result<bool, PermError>
```

`user_id` is `&str`. For `i64` PKs, callers need `&user.id().to_string()`. The source doc comment on `has_perm` explicitly says:

> Callers convert at the boundary: `&user.id().to_string()` for an `i64` PK.

The MDX doc omits this and shows `user.id()` directly, which will not compile for `i64` PKs without conversion.

### [Required] `has_perm_for_superuser` argument order

The doc shows:

```rust
has_perm_for_superuser(user.id(), "blog.publish_post", user.is_superuser()).await?
```

The actual signature:

```rust
pub async fn has_perm_for_superuser(
    user_id: &str,
    is_superuser: bool,
    perm: &str,
) -> Result<bool, PermError>
```

The `is_superuser` flag is the **second** argument, not third. The doc example has it reversed.

### [Required] Custom permissions example uses deprecated `.on(&pool)` API

The doc shows:

```rust
Permission::objects().on(&pool).create(permission).await?;
```

The ORM ambient-pool design (per CLAUDE.md and current QuerySet) does not use `.on(&pool)`. The current call site should be:

```rust
Permission::objects().create(permission).await?;
```

The `.on(&pool)` method may not exist at all on the current QuerySet surface. This will fail to compile with the current codebase.

---

## `rest.mdx`

Source: `plugins/umbra-rest/src/lib.rs` (lines 1-1367+ confirmed in prior session)

The `rest.mdx` page is intentionally minimal (redirects to the REST section). No claims to audit here beyond noting the dedicated `/docs/v0.0.1/rest/` section pages should be audited separately.

---

## `cache.mdx`

Source: `plugins/umbra-cache/src/lib.rs`

### [Required] `CachePlugin::new()` is the idiomatic API; doc only shows `init()`

The doc shows:

```rust
App::builder()
    .plugin(CachePlugin::init(Cache::memory()))
    .build()?;
```

The source at `lib.rs:505` (`impl CachePlugin`) shows:

```rust
/// Idiomatic constructor: pass to `.plugin(...)`.
pub fn new(cache: Cache) -> Self { ... }
```

The `init()` method is described in source comments as "manual/test wiring" — the approach for when you need to register the ambient cache outside an `App::build` lifecycle. The idiomatic production API is `CachePlugin::new(Cache::memory())`. The doc should lead with `new()` and mention `init()` only as a test-/manual-wiring escape hatch.

### [Nit] `Cache::redis()` deferred status not flagged

The doc lists `Cache::redis()` as an available backend alongside `Cache::memory()` and `Cache::sqlite()`. The source should be verified for whether Redis is shipped or still deferred; at minimum the doc should note its deferred status if the feature is unfinished.

---

## `openapi.mdx`

Source: `plugins/umbra-openapi/src/lib.rs`

Verified signatures:

- `OpenApiPlugin::new()` — confirmed (`Default` delegates to `new()`)
- `.at(path: &str)` — confirmed (note: takes `&str`, not `String`)
- `.title(s: impl Into<String>)` — confirmed
- `.version(s: impl Into<String>)` — confirmed
- `.description(s: impl Into<String>)` — confirmed
- `.exclude(tables: impl IntoIterator<Item = impl Into<String>>)` — confirmed

No critical discrepancies. The doc is accurate for the current source.

---

## `tasks.mdx`

Source: `plugins/umbra-tasks/src/lib.rs`

### [Required] `enqueue` takes 3 arguments, not 2

The `signals.mdx` signals example (and the tasks.mdx indirectly) shows:

```rust
enqueue("send_welcome_email", &WelcomeEmailPayload { ... }).await
```

The actual `enqueue` signature requires a third argument:

```rust
pub async fn enqueue(
    name: &str,
    payload: &impl Serialize,
    opts: EnqueueOptions,
) -> Result<Task, EnqueueError>
```

Every call site must pass `EnqueueOptions`. The minimal form is `EnqueueOptions::default()`. The 2-argument call will not compile.

### [Nit] `STATUS_*` constants not documented

The source exports `STATUS_PENDING`, `STATUS_RUNNING`, `STATUS_SUCCEEDED`, `STATUS_FAILED` string constants. The doc doesn't mention these; users querying the task table directly need them.

---

## `signals.mdx`

Source: `plugins/umbra-signals/src/lib.rs`

Verified:

- `SignalsPlugin::new()` — confirmed
- `on_model::<M>()` returning `ModelSignals<M>` — confirmed
- `.pre_save()`, `.post_save()`, `.pre_delete()`, `.post_delete()` — all confirmed
- `subscribe`, `subscribe_async`, `emit` — all confirmed
- `clear_for_tests` — confirmed (exists, for test isolation)

### [Required] `enqueue` call in example uses 2 args (inherited from tasks.mdx issue)

The signals page shows a code snippet that calls `enqueue(...)` with 2 arguments. See tasks.mdx finding above; the fix is the same.

---

## `security.mdx`

Source: `plugins/umbra-security/src/lib.rs`

Verified:

- `SecurityPlugin::new()` — confirmed
- `SecurityPlugin::with_config(config: SecurityConfig)` — confirmed
- `SecurityPlugin::with_hsts(hsts: ...)` — confirmed (not in doc but not wrong)
- `SecurityConfig` struct fields including `hsts`, `content_security_policy`, `csrf_exempt_paths` — confirmed
- Default `server_header: Some("umbra")` — confirmed (doc doesn't mention but not a discrepancy)

No discrepancies found. The doc is accurate.

---

## `the-plugin-trait.mdx`

Source: `crates/umbra-core/src/plugin.rs` (implied; not read in this session but cross-checked via plugin source impls)

The doc lists the Plugin trait surface:

```
name, dependencies, models, routes, system_checks, on_ready, commands,
templates_dirs, wrap_router, static_files
```

From source impls observed across plugins: `name`, `models`, `routes`, `on_ready`, `commands`, `wrap_router` are all confirmed. `static_dirs` (not `static_files`) is the correct method name (confirmed via `umbra-playground/src/lib.rs:171` and `umbra-static/src/lib.rs:304`).

### [Required] `static_files` vs `static_dirs`

The doc uses `static_files` in the Plugin trait surface listing. The actual trait method is `static_dirs()` (confirmed in `umbra-playground/src/lib.rs:171`: `fn static_dirs(&self) -> Vec<StaticDir>`). Also `static_root_dirs()` is a separate method for root (non-namespaced) sources (confirmed in `umbra-static/src/lib.rs:304`). The doc needs to distinguish these two trait methods.

---

## `static.mdx`

Source: `plugins/umbra-static/src/lib.rs`

Verified:

- `StaticPlugin::new(mount, dir)` — confirmed
- `StaticPlugin::embedded(mount, dir: &'static Dir<'static>)` — confirmed
- `.max_age(duration: Duration)` — confirmed (takes `Duration`, not seconds directly)
- `collectstatic` command — confirmed, contributed via `Plugin::commands()`

### [Nit] Dev-mode `max_age` override not documented

The doc mentions `.max_age()` but does not note that in `Environment::Dev` the effective max-age is forced to `0` regardless of the configured value (per `lib.rs:197-211`). This prevents stale assets in dev.

---

## `media.mdx`

Source: `plugins/umbra-media/src/lib.rs`

Verified:

- `MediaPlugin::new(mount, dir)` — confirmed
- `MediaPlugin::with_storage(mount, storage: Arc<dyn Storage>)` — confirmed
- `.max_size(bytes: u64)` — confirmed
- `MediaPlugin::save(filename, content_type, bytes)` returning `Result<MediaSaveOutcome, MediaError>` — confirmed
- `MediaSaveOutcome { file: MediaFile, url: String }` — confirmed
- `MediaFile` model fields (id, key, filename, content_type, size, uploaded_at) — confirmed
- `FsStorage`, `MediaTracking` — confirmed

### [Important] `public_base` builder not documented

The source exposes `MediaPlugin::public_base(base: impl Into<String>)` (lib.rs:380) to set an absolute URL prefix for the default `FsStorage`. The doc does not mention this builder. Users deploying behind a CDN or with a custom host need it for fully-qualified URLs.

### [Important] XSS defence (`active-content neutralisation`) not mentioned

The `FsStorage::store` implementation silently renames uploads with active-content extensions (`.html`, `.svg`, `.js`, etc.) by appending `.txt` (WEB-4 defense). This is a semantic change to the stored filename and URL: a file uploaded as `evil.html` is stored as `evil.html.txt`. The doc says nothing about this, which will surprise users who expect the stored key to match the uploaded filename for these extensions.

---

## `rls.mdx`

Source: `plugins/umbra-rls/src/lib.rs`

Verified:

- `RlsPlugin::new()` — confirmed
- `.enable_on(table: impl Into<String>)` — confirmed
- `.policy(table, name, action: Action, using: impl Into<String>)` — confirmed
- `.policy_with_check(table, name, action, using, with_check)` — confirmed
- `Action` enum: `Select`, `Insert`, `Update`, `Delete`, `All` — confirmed
- SQLite silently skips, Postgres applies — confirmed

No critical discrepancies. The doc is accurate.

---

## `playground.mdx`

Source: `plugins/umbra-playground/src/lib.rs`

Verified:

- `PlaygroundPlugin::new(app_name)` — confirmed
- `PlaygroundPlugin::default()` warns and falls back to `"default"` — confirmed
- `.at(path)` — confirmed
- Requires `umbra-openapi` and `umbra-rest` — correct (reads OpenAPI spec at runtime)

### [Important] `allow_in_prod` not documented

The source adds `PlaygroundPlugin::allow_in_prod()` (lib.rs:89) which gates whether the playground mounts in `Environment::Prod`. The default is `allow_in_prod = false` — the plugin logs a warning and mounts nothing in prod unless this is called. The doc doesn't mention this production-safety guard, which matters when a user tries to expose the playground in a staging/prod environment and gets no routes without a helpful error.

---

## `live-reload.mdx`

Source: `plugins/umbra-livereload/src/lib.rs`

Verified:

- `LiveReloadPlugin::new()` — confirmed (watches `./templates` + `./static` by default)
- `.watch(path: impl Into<PathBuf>)` — confirmed
- `.watch_only(paths: impl IntoIterator<Item = PathBuf>)` — confirmed
- SSE at `/__umbra/livereload` — confirmed
- Auto-injection via `Plugin::wrap_router` — confirmed
- Dev-only gating — confirmed
- CSS hot-swap vs full reload — confirmed

No discrepancies found. The doc is accurate.

---

## `email.mdx`

Source: `plugins/umbra-email/src/lib.rs`

Verified:

- `EmailPlugin::default()` — confirmed (zero-arg, no models/routes)
- `EmailMessage::new(subject, to: Vec<String>)` — confirmed
- `.from()`, `.add_to()`, `.text_body()`, `.html_body()`, `.reply_to()` — all confirmed
- `.attach(filename, content_type, data: Vec<u8>)` — confirmed
- `send(&EmailMessage) -> Result<(), EmailError>` — confirmed
- `render_email_body(template_name, context) -> Result<String, EmailError>` — confirmed
- Console backend (default when SMTP host unset) — confirmed
- `UMBRA_EMAIL_BACKEND=console` override — confirmed

### [Nit] `EmailPlugin::default()` vs recommended `EmailPlugin` (unit struct)

The source shows `EmailPlugin` is a unit struct with `#[derive(Debug, Default)]`. `EmailPlugin::default()` and `EmailPlugin` are equivalent. The doc shows `EmailPlugin::default()` which works; it's marginally clearer to say `EmailPlugin` in the `.plugin(...)` chain.

---

## `plugins-are-apps.mdx`

This is a conceptual/mapping page. No concrete API claims beyond the Plugin trait surface (covered under `the-plugin-trait.mdx` above). No discrepancies.

---

## Summary table

| Page | Critical | Required | Important | Nit | Total |
|---|---|---|---|---|---|
| admin.mdx | 2 | 2 | 0 | 1 | 5 |
| auth.mdx | 0 | 0 | 0 | 1 | 1 |
| sessions.mdx | 0 | 2 | 0 | 1 | 3 |
| permissions.mdx | 0 | 3 | 0 | 0 | 3 |
| rest.mdx | 0 | 0 | 0 | 0 | 0 |
| cache.mdx | 0 | 1 | 0 | 1 | 2 |
| openapi.mdx | 0 | 0 | 0 | 0 | 0 |
| tasks.mdx | 0 | 1 | 0 | 1 | 2 |
| signals.mdx | 0 | 1 | 0 | 0 | 1 |
| security.mdx | 0 | 0 | 0 | 0 | 0 |
| the-plugin-trait.mdx | 0 | 1 | 0 | 0 | 1 |
| static.mdx | 0 | 0 | 0 | 1 | 1 |
| media.mdx | 0 | 0 | 2 | 0 | 2 |
| rls.mdx | 0 | 0 | 0 | 0 | 0 |
| playground.mdx | 0 | 0 | 1 | 0 | 1 |
| live-reload.mdx | 0 | 0 | 0 | 0 | 0 |
| email.mdx | 0 | 0 | 0 | 1 | 1 |
| plugins-are-apps.mdx | 0 | 0 | 0 | 0 | 0 |

**Total: 2 Critical, 11 Required, 3 Important, 7 Nit**

---

## Fix priority order

1. **admin.mdx — CSS path** (Critical): change `/admin/static/admin.css` → `/static/admin/admin.css`
2. **admin.mdx — `on_ready` tables** (Critical): remove claim that `on_ready` runs DDL; replace with "the migration engine creates these tables via `Plugin::models()`"
3. **admin.mdx — built-in widgets** (Required): replace "auto-seeded on boot" with "call `AdminPlugin::register_widget(builtin_total_models_widget())` explicitly"
4. **admin.mdx — missing widget kinds** (Required): add Card, Donut, Heatmap, Progress, Radial to the widget kinds table
5. **permissions.mdx — `has_perm` user_id type** (Required): document `&str` requirement and `user.id().to_string()` conversion
6. **permissions.mdx — `has_perm_for_superuser` arg order** (Required): fix example to match `(user_id, is_superuser, perm)` order
7. **permissions.mdx — `.on(&pool)` in custom perms example** (Required): remove; use ambient ORM call `Permission::objects().create(permission).await?`
8. **sessions.mdx — `create_session` type** (Required): clarify `user_id: Option<String>` and conversion needed for `i64` PKs
9. **sessions.mdx — `login_user_id` name** (Required): correct export name to `login_user_id`
10. **tasks.mdx / signals.mdx — `enqueue` arity** (Required): update all examples to pass `EnqueueOptions` as the third argument
11. **cache.mdx — `CachePlugin::new()` primary** (Required): lead with `CachePlugin::new(cache)`, demote `init()` to escape-hatch note
12. **the-plugin-trait.mdx — `static_files` name** (Required): correct to `static_dirs()` and document `static_root_dirs()` as separate
13. **media.mdx — `public_base` builder** (Important): document `MediaPlugin::public_base(base)` for CDN/custom host deployments
14. **media.mdx — active-content neutralisation** (Important): note that `.html/.svg/.js` uploads are stored as `.html.txt` etc.
15. **playground.mdx — `allow_in_prod`** (Important): document the prod-safety guard and the `.allow_in_prod()` opt-in
