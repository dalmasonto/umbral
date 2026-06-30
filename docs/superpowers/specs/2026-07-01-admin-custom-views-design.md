# Admin custom views — design

Date: 2026-07-01
Area: `plugins/umbral-admin`
Closes: features.md #76 (Admin custom views); the page-level half of gaps2 #6 (dynamic admin pages); gaps.md #43 (extend AdminPlugin with more pages).
Status: design, pending implementation plan

## Goal

Let a developer register arbitrary **admin pages** that aren't tied to a model — e.g. `/admin/reports/sales/` — and have them render the existing dashboard widget kinds (KPI/card/line/bar/donut/radial/heatmap/progress/table/feed). In one sentence: **a dashboard you can mount at any path.**

This makes the admin a control panel, not just a table browser: reports, analytics, and one-off data tools become a few lines of builder code that reuse the entire card/chart pipeline.

## Scope

**In scope (v1):**
- A developer-facing `AdminView` builder (path, title, subtitle, icon, sidebar group, permission, hidden flag, widget sections).
- `AdminPlugin::view(...)` / `views(...)` registration.
- Route mounting under the admin base, wrapped in the admin chrome (sidebar/topbar/breadcrumbs).
- Reuse of the existing widget data endpoint and renderers (cards/charts), via a DRY'd shared widget-grid macro.
- Sidebar integration: auto-listed, grouped, icon'd, permission-filtered, with a `.hidden()` opt-out.
- Codename-based permission gating for pages.

**Out of scope (explicitly deferred):**
- Admin-*authored*-at-runtime pages/widgets (gaps2 #6's harder half — a persisted widget DSL editable from the admin UI). v1 is developer-registered via the builder.
- Page-level shared filters (one date-range driving every widget). v1 relies on the existing per-widget period chips (`WidgetParams`).
- Custom-template / arbitrary-HTML pages (would require a new external template-registration seam; the engine is `include_str!`-compiled today). v1 is widget-sections only.

## Architecture context (what already exists)

- **Widget machinery is decoupled from the dashboard URL.** Widgets render via template macros in `templates/_macros/widgets/*`; each widget cell self-loads from `GET {base}/api/dashboard/widgets/{key}/data` (`handlers/dashboard.rs::dashboard_widget_data`), which looks the widget up in a flat `state.widget_catalog: Arc<Vec<Widget>>`. So any page can render the same widgets by (a) putting its widgets in that catalog and (b) emitting the same cell markup.
- **`Widget` / `WidgetSection` / `WidgetKind` / `WidgetPayload` / `WidgetDataFn`** are defined in `src/widgets.rs`; `WidgetSection::new(title).widget(...)` is the existing grouping API.
- **`AdminPlugin`** (`src/lib.rs`) is a builder holding `registry`, `widget_catalog`, `dashboard_sections`, branding, `base_path`, etc.; `Plugin::routes()` builds a static axum `Router` with `.with_state(AdminState)`. There is **no** extension slot for custom routes today.
- **Chrome**: `wrapper.html` → `base.html` (exposes `{% block content %}`). The sidebar nav is built from the `apps` context var (model groups from `registry.apps()`); there is no slot for non-model links today.
- **Auth/permission**: `auth::require_staff(headers, path)` is the universal gate; `permcheck::{check,require,load}` handles per-model `(plugin, table, Action)` checks and no-ops to staff-only when `PermissionsPlugin` is absent.
- **Routing precedence**: axum/matchit prefers static segments over `{param}`, so `/admin/reports/sales/` wins over `/admin/{table}/` with no special handling.

## Components

### 1. `AdminView` builder — new file `src/views.rs`

```rust
pub struct AdminView {
    /// Subpath under the admin base, normalized (no leading/trailing slash), e.g. "reports/sales".
    path: String,
    /// Page heading and default sidebar label.
    title: String,
    /// Optional caption under the heading.
    subtitle: Option<String>,
    /// Lucide icon name for the sidebar entry.
    icon: Option<String>,
    /// Sidebar group heading; defaults to "Pages" when None.
    group: Option<String>,
    /// Permission codename gate (e.g. "reports.view_sales"); None = any staff.
    permission: Option<String>,
    /// Routable but excluded from the sidebar when true.
    hidden: bool,
    /// The widget sections rendered on the page.
    sections: Vec<WidgetSection>,
}
```

Constructor + builder methods (all `mut self -> Self` except the constructor):
- `AdminView::new(path: impl Into<String>, title: impl Into<String>) -> Self`
- `.subtitle(impl Into<String>)`, `.icon(impl Into<String>)`, `.group(impl Into<String>)`, `.permission(impl Into<String>)`, `.hidden()`
- `.section(WidgetSection)`, `.sections(impl IntoIterator<Item = WidgetSection>)`

Plus read accessors the crate needs: `path()`, `slug()` (the normalized path, used as the sidebar active-state key + uniqueness key), `title()`, `subtitle()`, `icon()`, `group()`, `permission()`, `hidden()`, `sections()`.

Path normalization: trim `/`; collapse to the canonical `a/b/c` form. The mounted route is `{base}/{path}` (trailing-slash handling matches the existing changelist convention).

### 2. Registration — `AdminPlugin`

- New field `custom_views: Vec<AdminView>` (default empty).
- New builders: `fn view(mut self, view: AdminView) -> Self`; `fn views(mut self, it: impl IntoIterator<Item = AdminView>) -> Self`.

In `Plugin::routes()`:
1. After the existing widget merge, **flatten every custom view's section widgets into the same `catalog: Vec<Widget>`** that backs `AdminState.widget_catalog`, so `dashboard_widget_data` resolves view widgets unchanged.
2. **Debug-build duplicate-key check**: while flattening, collect widget keys across dashboard sections + all views; if a key repeats, `tracing::warn!` naming the colliding key (keys must be globally unique because the data endpoint is keyed globally). Warn, don't panic — keep boot resilient.
3. Mount each view: for `v in &custom_views`, add `.route(&route(&format!("/{}", v.path()), base), get(move |state, headers| custom_view(state, headers, slug)))` where `slug = v.slug()` is moved into the closure. (Each route closes over its own slug; the handler looks the view up by slug in `AdminState.custom_views`.)
4. Store `custom_views` (as `Arc<Vec<AdminView>>`) on `AdminState` so the handler and sidebar builder can read them.
5. Extend `route_paths()` with each view's full path (for any introspection/consistency that enumerates admin routes).

### 3. Page handler — new file `src/handlers/custom_view.rs`

```rust
pub(crate) async fn custom_view(
    State(state): State<AdminState>,
    headers: HeaderMap,
    slug: String,   // captured per-route
) -> Response
```

Flow:
1. `require_staff(&headers, &full_path)` → `AuthUser` (redirects to login when unauthenticated; 403 when not staff).
2. Look up the view by `slug` in `state.custom_views`; 404 if absent (defensive — shouldn't happen since routes are derived from the same list).
3. If `view.permission` is `Some(code)`, gate with the new `permcheck::require_codename(&user, code).await` → return its 403 `Response` on deny.
4. Build the sidebar (`view::sidebar_apps(...)` for model groups **and** the new `view_groups` builder — see §5).
5. Build `widget_sections` JSON in the **same shape** the dashboard handler emits (`{ title, subtitle, widgets: [{ key, title, kind, span: { cols, rows } }] }`) from `view.sections`.
6. `render("admin/custom_view.html", context!(user, page_title = view.title, page_subtitle = view.subtitle, widget_sections, apps, view_groups, active_view = slug, breadcrumbs, initial_theme))`.

Breadcrumbs: `Home > {title}`.

### 4. Permission primitive — `src/permcheck.rs`

Add a raw-codename check (views aren't model-bound, so the `(plugin, table, Action)` shape doesn't fit):

```rust
/// True if the user holds `codename` (or PermissionsPlugin isn't installed → staff-only baseline).
pub(crate) async fn has_codename(user: &AuthUser, codename: &str) -> bool

/// Handler guard: Ok(()) when allowed, Err(403 Response) when denied.
pub(crate) async fn require_codename(user: &AuthUser, codename: &str) -> Result<(), Response>
```

Reuse the existing codename-loading path that `check`/`load` already use; preserve the `permissions_installed()` graceful no-op (absent plugin → allow). `has_codename` is also used by the sidebar builder to filter which views a user sees.

### 5. Sidebar — `src/view.rs` + `templates/base.html`

- New builder (in `src/view.rs`) producing a `Vec<ViewGroup>` from `state.custom_views`, filtered to non-`hidden` views the user is allowed to see (`has_codename` when `permission` is set), clustered by `group()` (default heading "Pages"), preserving registration order within a group and a stable group order. Each entry: `{ href: "{base}/{path}", label: title, icon, slug }`.
- `base.html`: after the model-group loop, add a loop over `view_groups` rendering each group heading + its links, using the same nav-item markup/classes as model links, with active-state when `active_view == entry.slug`.

### 6. Template — new `templates/custom_view.html` + DRY refactor

- Extract the dashboard's widget-grid markup (currently inline in `dashboard.html`, the `{% for section in widget_sections %}` … per-widget HTMX cell + per-kind skeleton) into a shared macro `templates/_macros/widget_grid.html` → `{% macro widget_grid(widget_sections, admin_base) %}`.
- `dashboard.html` imports and calls the macro (behavior unchanged).
- `custom_view.html` extends `base.html`, renders the page header (`page_title` + optional `page_subtitle`) and calls `widget_grid(widget_sections, admin_base)`.
- Register both `admin/custom_view.html` and `admin/_macros/widget_grid.html` in `src/engine.rs` via `add_template` (a `{% from "admin/_macros/widget_grid.html" import widget_grid %}`-imported macro must be a registered template — same pattern as `_macros/pagination.html`).

## Data flow

```
AdminPlugin::default().view(AdminView::new("reports/sales","Sales").section(...))
  → routes(): flatten view widgets into widget_catalog; mount GET {base}/reports/sales → custom_view(slug)
GET /admin/reports/sales/
  → require_staff → require_codename(view.permission) → render custom_view.html (chrome + widget_grid)
  → each widget cell hx-get {base}/api/dashboard/widgets/{key}/data  (existing endpoint, finds widget in catalog)
  → period chips + payloads render exactly as on the dashboard
```

## Error handling

- Unauthenticated → redirect to `{base}/login?next=...` (via `require_staff`).
- Not staff → 403.
- Lacking the view's codename (permissions installed) → 403 (`require_codename`).
- Unknown slug at the handler → 404 (defensive).
- Duplicate widget key across dashboard + views → boot-time `tracing::warn!` (non-fatal); the data endpoint resolves the first match.
- View path colliding with a model table name → the view shadows the changelist (axum static-over-dynamic). Documented caveat, not enforced.

## Testing (behavioral, against a real booted admin)

In a new `plugins/umbral-admin/tests/custom_views.rs` (mirror the `phase4_dashboard.rs` harness — real router via `boot()`, `login_session`, `send`):
1. **Page renders**: register an `AdminView` with one widget section; `GET {base}/reports/sales/` → 200; body contains the page title, the admin chrome, and a widget cell `id="widget-<key>"` with the `hx-get .../api/dashboard/widgets/<key>/data`.
2. **Sidebar link**: the dashboard (or the page) sidebar contains a link to the view under its group heading.
3. **Widget data served**: `GET {base}/api/dashboard/widgets/<key>/data` (the view's widget key) → 200 with the widget payload (proves the flatten-into-catalog step).
4. **Permission gate**: with `PermissionsPlugin` installed and a view requiring `reports.view_sales`, a staff user lacking that codename → 403; granting it → 200. (A no-permissions boot → staff sees it, proving the graceful no-op.)
5. **Hidden**: a `.hidden()` view is routable (200) but absent from the sidebar.

## Affected files

- `plugins/umbral-admin/src/views.rs` (new) — `AdminView` + builder.
- `plugins/umbral-admin/src/lib.rs` — `custom_views` field, `.view()/.views()`, route mounting + widget flatten + dup-key warn, `AdminState.custom_views`, `route_paths()`.
- `plugins/umbral-admin/src/handlers/custom_view.rs` (new) + `handlers/mod.rs` wiring.
- `plugins/umbral-admin/src/permcheck.rs` — `has_codename` / `require_codename`.
- `plugins/umbral-admin/src/view.rs` — `view_groups` sidebar builder.
- `plugins/umbral-admin/templates/_macros/widget_grid.html` (new) — extracted grid macro.
- `plugins/umbral-admin/templates/dashboard.html` — use the macro.
- `plugins/umbral-admin/templates/custom_view.html` (new) — page template.
- `plugins/umbral-admin/templates/base.html` — sidebar `view_groups` loop.
- `plugins/umbral-admin/src/engine.rs` — register new templates.
- `plugins/umbral-admin/tests/custom_views.rs` (new).
- `documentation/docs/v0.0.1/admin/custom-views.mdx` (new) — purpose + one example + link to this spec.

## Verification

- `cargo fmt -p umbral-admin && cargo clippy -p umbral-admin --all-targets && cargo build && cargo test -p umbral-admin` clean.
- The five behavioral tests pass.
- Rebuild the admin CSS only if new utility classes are introduced by the new templates (`cd plugins/umbral-admin/css && npm run build`), and commit the regenerated `admin.css`.
- Manual: register a sample view in an example app (or a test) and confirm the page renders cards/charts identically to the dashboard, the sidebar link works, and period chips function.

## Future (noted, not built)

- Admin-authored views/widgets at runtime (persisted definitions, gaps2 #6) — a separate, larger feature on top of this one.
- Page-level shared filters threaded into every widget's data fn.
- Custom-template/arbitrary-HTML views (needs an external template-registration seam).
