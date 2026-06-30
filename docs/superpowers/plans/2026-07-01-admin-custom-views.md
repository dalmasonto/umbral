# Admin Custom Views Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a developer register widget-based admin pages at arbitrary paths (e.g. `/admin/reports/sales/`) via `AdminPlugin::default().view(AdminView::new("reports/sales","Sales").section(...))`, rendering the existing card/chart widgets inside the admin chrome with sidebar integration and permission gating.

**Architecture:** umbral-admin is a server-rendered MiniJinja + HTMX + axum plugin. The widget system (`Widget`/`WidgetSection`, the `/api/dashboard/widgets/{key}/data` endpoint, the per-kind render macros) is already decoupled from the dashboard URL. A "custom view" therefore reduces to: an `AdminView` builder, a `custom_views` collection on `AdminPlugin`, route mounting that flattens the view's widgets into the existing global catalog, one page handler that reuses a DRY'd widget-grid macro, a raw-codename permission check, and a sidebar loop. The design spec is `docs/superpowers/specs/2026-07-01-admin-custom-views-design.md`.

**Tech Stack:** Rust (axum, minijinja), HTMX, Tailwind (CDN dev / compiled prod), Lucide, ApexCharts.

## Global Constraints

- Plugins use the ORM, never raw `sqlx::query`/`sqlx::query_as` in `plugins/<name>/src/`. (No new DB row access is needed here.)
- No new dependencies. Charts→ApexCharts, icons→Lucide, type→Inter.
- v1 is **developer-registered** (builder) widget-based pages. NOT admin-authored-at-runtime, NOT page-level shared filters, NOT arbitrary-HTML/custom-template pages — those are deferred (see spec "Future").
- New public surface goes in `umbral-admin` AND is re-exported from the `umbral` facade if a plugin author needs it. `AdminView` is public (re-export it next to `AdminModel`/`Widget`/`WidgetSection` wherever those are exported).
- Widget keys are globally unique across the dashboard and all views (the data endpoint is keyed globally). A duplicate is a boot-time `tracing::warn!`, never a panic.
- Permission gating reuses the codename system and MUST keep the graceful no-op: when `PermissionsPlugin` is absent, checks return `true` (staff-only baseline).
- Before each commit: `cargo fmt -p umbral-admin && cargo clippy -p umbral-admin --all-targets && cargo build && cargo test -p umbral-admin` must pass. Use `cargo fmt -p umbral-admin` (NEVER bare `cargo fmt` — it churns the whole workspace; `git restore` any unrelated file it touches). Never `--no-verify`.
- Do not touch the user's dirty working-tree files (`planning/gaps3.md`, `AGENTS.md`, `CLAUDE.md`). Stage only the files each task names.
- Do not `cargo run` / restart the user's example apps.
- Line wrapping: prose in `.md`/`.mdx` is not hard-wrapped.

## Reference facts (verified against current code)

- `AdminPlugin` struct: `src/lib.rs:160-191` (fields: `registry, widget_catalog, dashboard_sections, branding, base_path, dashboard_models, dashboard_models_title, dashboard_models_subtitle, restore_last_path`). `Default` at `:193-207`. Builders start at `:209`.
- `AdminState` struct: `src/lib.rs:482-503` (fields incl. `widget_catalog: Arc<Vec<Widget>>`, `dashboard_sections: Arc<Vec<WidgetSection>>`).
- `routes()`: `src/lib.rs:565-773`. Section merge + flat `catalog` build at `:590-601`; `AdminState` constructed `:603-611`; route table `:612-773`. The `fn route(sub, base)` helper is `:518`.
- Widget-section JSON shape the dashboard handler emits (mirror it exactly): `src/handlers/list.rs:274-299` →
  `{ "title", "subtitle", "widgets": [ { "key", "title", "kind": w.kind.as_str(), "span": { "cols", "rows" } } ] }`.
- The widget-grid markup to extract is `templates/dashboard.html:84-223` (the `{% for section in widget_sections %}` … matching `{% endfor %}`).
- Sidebar model-group loop: `templates/base.html:204-230` (the `{%- for app in apps %}` block); insert the view-group loop after it.
- `permcheck`: `permissions_installed()` `src/permcheck.rs:55`; `check()` `:66` calls `umbral_permissions::has_perm_for_superuser(&user_id, user.is_superuser, &perm)`; `require()` returns `Err((StatusCode::FORBIDDEN, "umbral-admin: permission denied").into_response())` `:91-101`.
- `auth::require_staff(headers, current_path) -> Result<AuthUser, Response>` `src/auth.rs:33`.
- Sidebar builder `view::sidebar_apps(...)` `src/view.rs:55-101`; `engine::render(name, ctx)` `src/engine.rs:365`; macro templates are registered via `env.add_template("admin/_macros/<f>.html", include_str!(...))` (precedent: `_macros/pagination.html`).

## File Structure

| File | Responsibility | Tasks |
|---|---|---|
| `src/views.rs` (new) | `AdminView` builder + accessors. | 1 |
| `src/lib.rs` | re-export `AdminView`; `custom_views` field; `.view()/.views()`; `AdminState.custom_views`; route mount + widget flatten + dup-key warn + `route_paths`. | 1, 4 |
| `src/permcheck.rs` | `has_codename` / `require_codename`. | 2 |
| `templates/_macros/widget_grid.html` (new) | extracted widget-grid macro. | 3 |
| `templates/dashboard.html` | import + call the macro. | 3 |
| `src/handlers/custom_view.rs` (new) + `handlers/mod.rs` | the page handler. | 5 |
| `templates/custom_view.html` (new) | page template (chrome + grid). | 5 |
| `src/engine.rs` | register `custom_view.html` + `_macros/widget_grid.html`. | 3, 5 |
| `src/view.rs` | `view_groups` sidebar builder. | 6 |
| `templates/base.html` | sidebar `view_groups` loop. | 6 |
| `src/handlers/list.rs` (+ dashboard handler) | thread `view_groups` into page contexts. | 6 |
| `tests/custom_views.rs` (new) | behavioral tests. | 7 |
| `documentation/docs/v0.0.1/admin/custom-views.mdx` (new) | doc page. | 7 |

Task order: **1 → 2 → 3** independent of each other (do in any order). **4** needs 1. **5** needs 1+2+3+4. **6** needs 1+2 (and threads into 4's state). **7** is final tests+docs+verify.

---

### Task 1: `AdminView` builder

**Files:**
- Create: `plugins/umbral-admin/src/views.rs`
- Modify: `plugins/umbral-admin/src/lib.rs` (add `mod views;` and re-export `pub use views::AdminView;` next to the other public re-exports)

**Interfaces:**
- Produces: `AdminView` with constructor `new(path, title)`, builders `.subtitle/.icon/.group/.permission/.hidden/.section/.sections`, and accessors `path()/slug()/title()/subtitle()/icon()/group()/permission()/hidden()/sections()`. `slug()` == normalized path (used as the per-route key + sidebar active key). Consumed by Tasks 4, 5, 6.

- [ ] **Step 1: Write the failing test**

Create `plugins/umbral-admin/src/views.rs` with the test module at the bottom (implementation added in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::WidgetSection;

    #[test]
    fn normalizes_path_and_slug() {
        let v = AdminView::new("/reports/sales/", "Sales");
        assert_eq!(v.path(), "reports/sales", "leading/trailing slashes stripped");
        assert_eq!(v.slug(), "reports/sales", "slug mirrors the normalized path");
        assert_eq!(v.title(), "Sales");
    }

    #[test]
    fn defaults_are_sane() {
        let v = AdminView::new("tools/x", "X");
        assert!(v.subtitle().is_none());
        assert!(v.icon().is_none());
        assert!(v.group().is_none(), "group defaults to None (renders under 'Pages')");
        assert!(v.permission().is_none(), "no permission = any staff");
        assert!(!v.hidden(), "shown in sidebar by default");
        assert!(v.sections().is_empty());
    }

    #[test]
    fn builders_populate_fields() {
        let v = AdminView::new("reports/sales", "Sales")
            .subtitle("Revenue")
            .icon("bar-chart")
            .group("Reports")
            .permission("reports.view_sales")
            .hidden()
            .section(WidgetSection::new("This month"));
        assert_eq!(v.subtitle(), Some("Revenue"));
        assert_eq!(v.icon(), Some("bar-chart"));
        assert_eq!(v.group(), Some("Reports"));
        assert_eq!(v.permission(), Some("reports.view_sales"));
        assert!(v.hidden());
        assert_eq!(v.sections().len(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-admin --lib views::`
Expected: FAIL to compile — `AdminView` not defined.

- [ ] **Step 3: Implement `AdminView`**

At the top of `plugins/umbral-admin/src/views.rs`, above the test module:

```rust
//! Custom admin views — developer-registered widget pages mounted at
//! arbitrary paths under the admin base (e.g. `/admin/reports/sales/`).
//! A view renders the existing dashboard widget kinds inside the admin
//! chrome. See `docs/superpowers/specs/2026-07-01-admin-custom-views-design.md`.

use crate::widgets::WidgetSection;

/// A registered admin page that is not tied to a model. Renders one or
/// more [`WidgetSection`]s (the same cards/charts the dashboard uses)
/// inside the admin chrome, mounted at `{admin_base}/{path}`.
#[derive(Debug, Clone)]
pub struct AdminView {
    path: String,
    title: String,
    subtitle: Option<String>,
    icon: Option<String>,
    group: Option<String>,
    permission: Option<String>,
    hidden: bool,
    sections: Vec<WidgetSection>,
}

/// Normalize a developer-supplied path to the canonical `a/b/c` form
/// (no leading/trailing slashes, no empty segments).
fn normalize_path(raw: &str) -> String {
    raw.split('/')
        .filter(|seg| !seg.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

impl AdminView {
    /// Start a view. `path` is the subpath under the admin base
    /// (`"reports/sales"` → `/admin/reports/sales/`); `title` is the page
    /// heading and the default sidebar label.
    pub fn new(path: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            path: normalize_path(&path.into()),
            title: title.into(),
            subtitle: None,
            icon: None,
            group: None,
            permission: None,
            hidden: false,
            sections: Vec::new(),
        }
    }

    /// Optional caption under the page heading.
    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Lucide icon name for the sidebar entry.
    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Sidebar group heading. Defaults to "Pages" when unset.
    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// Permission codename gate (e.g. `"reports.view_sales"`). Unset = any staff.
    pub fn permission(mut self, codename: impl Into<String>) -> Self {
        self.permission = Some(codename.into());
        self
    }

    /// Keep the view routable but hide it from the sidebar.
    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    /// Append one widget section.
    pub fn section(mut self, section: WidgetSection) -> Self {
        self.sections.push(section);
        self
    }

    /// Append many widget sections.
    pub fn sections(mut self, sections: impl IntoIterator<Item = WidgetSection>) -> Self {
        self.sections.extend(sections);
        self
    }

    // --- accessors used by the crate (route mount, handler, sidebar) ---
    pub(crate) fn path(&self) -> &str { &self.path }
    /// Stable key for the per-route handler + sidebar active-state. Equals the normalized path.
    pub(crate) fn slug(&self) -> &str { &self.path }
    pub(crate) fn title(&self) -> &str { &self.title }
    pub(crate) fn subtitle(&self) -> Option<&str> { self.subtitle.as_deref() }
    pub(crate) fn icon(&self) -> Option<&str> { self.icon.as_deref() }
    pub(crate) fn group(&self) -> Option<&str> { self.group.as_deref() }
    pub(crate) fn permission(&self) -> Option<&str> { self.permission.as_deref() }
    pub(crate) fn hidden(&self) -> bool { self.hidden }
    pub(crate) fn sections(&self) -> &[WidgetSection] { &self.sections }
}
```

> Note: the test module references `path()/slug()/...` which are `pub(crate)` — fine, the test is in the same crate. `AdminView::new` and the builders are `pub`.

- [ ] **Step 4: Wire the module + facade re-export**

In `plugins/umbral-admin/src/lib.rs`: add `mod views;` alongside the other `mod` declarations, and add `pub use views::AdminView;` next to the existing public re-exports of `AdminModel` / `Widget` / `WidgetSection` (grep for `pub use ...Widget` to find the spot).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p umbral-admin --lib views::`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
cargo fmt -p umbral-admin && cargo clippy -p umbral-admin --all-targets
git add plugins/umbral-admin/src/views.rs plugins/umbral-admin/src/lib.rs
git commit -m "feat(admin): AdminView builder for custom admin views

The developer-facing type for registering widget-based admin pages at
arbitrary paths. Path normalization + builder for title/subtitle/icon/
group/permission/hidden/sections. No wiring yet.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Raw-codename permission check

**Files:**
- Modify: `plugins/umbral-admin/src/permcheck.rs`

**Interfaces:**
- Produces: `pub(crate) async fn has_codename(user: &AuthUser, codename: &str) -> bool` and `pub(crate) async fn require_codename(user: &AuthUser, codename: &str) -> Result<(), Response>`. Consumed by Tasks 5 (handler gate) and 6 (sidebar filter).

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` in `plugins/umbral-admin/src/permcheck.rs` (it already has tests like `from_codenames_view_only`). If the codename helpers can be unit-tested there, add:

```rust
    // has_codename / require_codename: when the permissions plugin is NOT
    // installed (the unit-test process), both must allow (staff-only baseline).
    #[tokio::test]
    async fn codename_checks_allow_when_permissions_absent() {
        let user = AuthUser {
            id: 1,
            username: "staff".into(),
            is_staff: true,
            is_superuser: false,
            ..Default::default()
        };
        assert!(
            super::has_codename(&user, "reports.view_sales").await,
            "absent permissions plugin → allow"
        );
        assert!(
            super::require_codename(&user, "reports.view_sales").await.is_ok(),
            "require_codename Ok when allowed"
        );
    }
```

> If `AuthUser` cannot be constructed with that literal in this crate's tests, mirror however the existing `permcheck` tests build an `AuthUser` (check the file); the assertion (allow when permissions absent) is the contract.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-admin --lib permcheck::tests::codename_checks_allow_when_permissions_absent`
Expected: FAIL to compile — `has_codename` not defined.

- [ ] **Step 3: Implement the helpers**

Add to `plugins/umbral-admin/src/permcheck.rs` (after `require`, mirroring `check`/`require`):

```rust
/// Check an arbitrary permission codename directly (not the
/// `(plugin, table, action)` triple) — used by custom admin views, which
/// aren't model-bound. Returns `true` when permissions aren't installed
/// (staff-only baseline), the user is a superuser, or the user holds the
/// codename directly / via a group.
pub(crate) async fn has_codename(user: &AuthUser, codename: &str) -> bool {
    if !permissions_installed() {
        return true;
    }
    let user_id = user.id.to_string();
    umbral_permissions::has_perm_for_superuser(&user_id, user.is_superuser, codename)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(
                user_id = user_id.as_str(),
                perm = codename,
                error = %err,
                "codename permission check failed; denying by default"
            );
            false
        })
}

/// Handler-side guard for a raw codename. `Ok(())` when allowed, else a
/// 403 [`Response`]. Mirrors [`require`] for the model-bound path.
pub(crate) async fn require_codename(user: &AuthUser, codename: &str) -> Result<(), Response> {
    if has_codename(user, codename).await {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "umbral-admin: permission denied").into_response())
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p umbral-admin --lib permcheck::tests::codename_checks_allow_when_permissions_absent`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt -p umbral-admin && cargo clippy -p umbral-admin --all-targets
git add plugins/umbral-admin/src/permcheck.rs
git commit -m "feat(admin): has_codename / require_codename permission check

Raw-codename gate for custom admin views (not model-bound, so the
(plugin, table, action) triple doesn't fit). Keeps the graceful no-op:
absent PermissionsPlugin -> allow (staff-only baseline).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Extract the widget-grid macro (DRY)

**Files:**
- Create: `plugins/umbral-admin/templates/_macros/widget_grid.html`
- Modify: `plugins/umbral-admin/templates/dashboard.html` (replace inline grid with a macro call)
- Modify: `plugins/umbral-admin/src/engine.rs` (register the macro template)
- Modify: `plugins/umbral-admin/tests/phase4_dashboard.rs` (add a regression assertion)

**Interfaces:**
- Produces: macro `widget_grid(widget_sections, admin_base)` rendering the section loop + per-widget HTMX cells + per-kind skeletons. Consumed by `dashboard.html` (Task 3) and `custom_view.html` (Task 5).

- [ ] **Step 1: Write the failing test**

Append to `plugins/umbral-admin/tests/phase4_dashboard.rs` (reuse its `boot()/login_session/send` harness; mirror a neighboring dashboard test for request setup). The dashboard must still render a widget cell after the extraction:

```rust
#[tokio::test]
async fn test_dashboard_widget_grid_renders_via_macro() {
    let router = boot().await.clone();
    let session = login_session(router.clone(), "grid_admin", "password123").await;
    let req = axum::http::Request::builder()
        .uri("/admin/")
        .header(axum::http::header::COOKIE, format!("umbral_session={session}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _h, body) = send(router, req).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    // A widget cell still self-loads from the data endpoint after the macro extraction.
    assert!(
        body.contains("/api/dashboard/widgets/") && body.contains("hx-trigger=\"load\""),
        "dashboard still renders widget cells via the shared grid macro"
    );
}
```

> The default admin ships `builtin_total_models_widget` + `builtin_recent_users_widget`, so a booted dashboard has cells. If this test file's `boot()` registers no widgets, register a section in its boot (mirror how phase4_dashboard's boot adds widgets) so a cell exists.

- [ ] **Step 2: Run test to verify it fails (or passes pre-extraction)**

Run: `cargo test -p umbral-admin --test phase4_dashboard test_dashboard_widget_grid_renders_via_macro`
Expected: PASS before extraction (cells render inline) — this is a **regression guard**; it must still PASS after Step 3-4. (If it fails pre-extraction, the boot has no widgets — fix the boot per the note above first.)

- [ ] **Step 3: Create the macro**

Create `plugins/umbral-admin/templates/_macros/widget_grid.html`. Wrap the EXACT current contents of `dashboard.html:84-223` (the `{% for section in widget_sections %}` … matching `{% endfor %}`) in a macro. The body is moved verbatim; only the wrapper is added:

```html
{#
  widget_grid.html — the dashboard widget grid, shared by dashboard.html
  and custom_view.html so custom pages render cards/charts identically.
  Each cell self-loads from {admin_base}/api/dashboard/widgets/{key}/data.

  Params:
    widget_sections : Vec of { title, subtitle, widgets: [{key,title,kind,span:{cols,rows}}] }
    admin_base      : the admin base path
#}
{% macro widget_grid(widget_sections, admin_base) %}
{% for section in widget_sections %}
{% if section.widgets and section.widgets | length > 0 %}
<section class="mb-xl">
  ... (verbatim copy of dashboard.html lines 86-221: the section header,
       the grid div, the per-widget cell with hx-get, and ALL per-kind
       skeleton branches through the final {% endif %} and </div></section>) ...
</section>
{% endif %}
{% endfor %}
{% endmacro %}
```

> Implementer: copy `dashboard.html:84-223` byte-for-byte between `{% macro %}` and `{% endmacro %}`. The block already references `admin_base` and `widget_sections`, which are now the macro params — no edits to the body needed.

- [ ] **Step 4: Register the macro + call it from dashboard.html**

In `plugins/umbral-admin/src/engine.rs`, register the macro template alongside the others (e.g. near the `_macros/pagination.html` registration):

```rust
        env.add_template(
            "admin/_macros/widget_grid.html",
            include_str!("../templates/_macros/widget_grid.html"),
        )
        .expect("admin/_macros/widget_grid.html parses");
```

In `plugins/umbral-admin/templates/dashboard.html`: add the import near the top (after `{% extends %}` / other imports):

```html
{% from "admin/_macros/widget_grid.html" import widget_grid %}
```

Then replace the entire `dashboard.html:84-223` block (the `{% for section in widget_sections %}` … `{% endfor %}`) with one call:

```html
{{ widget_grid(widget_sections, admin_base) }}
```

- [ ] **Step 5: Run the test to verify it still passes**

Run: `cargo test -p umbral-admin --test phase4_dashboard`
Expected: PASS (the new regression test + all existing dashboard tests — markup is identical, now via macro).

- [ ] **Step 6: Commit**

```bash
cargo fmt -p umbral-admin && cargo clippy -p umbral-admin --all-targets
git add plugins/umbral-admin/templates/_macros/widget_grid.html plugins/umbral-admin/templates/dashboard.html plugins/umbral-admin/src/engine.rs plugins/umbral-admin/tests/phase4_dashboard.rs
git commit -m "refactor(admin): extract widget grid into a shared macro

Move the dashboard's widget-section grid into _macros/widget_grid.html so
custom views can render the same cards/charts. Behavior unchanged.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Register custom views — field, builders, state, routing

**Files:**
- Modify: `plugins/umbral-admin/src/lib.rs`

**Interfaces:**
- Consumes: `AdminView` (Task 1).
- Produces: `AdminPlugin::view(AdminView) -> Self` and `AdminPlugin::views(impl IntoIterator<Item = AdminView>) -> Self`; `AdminState.custom_views: Arc<Vec<AdminView>>`; mounted routes `GET {base}/{view.path}` dispatching to `handlers::custom_view::custom_view(state, headers, slug)` (the handler lands in Task 5 — this task adds the field/builders/state/flatten/route wiring and references the handler path; sequence Task 5 to provide the handler before this compiles, OR add a temporary stub — see Step 4 note).

- [ ] **Step 1: Write the failing test**

Append to `plugins/umbral-admin/src/lib.rs`'s test module (or create `#[cfg(test)] mod custom_view_wiring_tests`):

```rust
#[cfg(test)]
mod custom_view_wiring_tests {
    use super::*;
    use crate::views::AdminView;
    use crate::widgets::{Widget, WidgetKind, WidgetSection, WidgetDataFn, WidgetPayload, KpiPayload};

    fn tiny_kpi(key: &'static str) -> Widget {
        Widget {
            key,
            title: "T".into(),
            kind: WidgetKind::Kpi,
            default_span: Default::default(),
            permission: None,
            data: WidgetDataFn::new(|_user| async { WidgetPayload::Kpi(KpiPayload::default()) }),
            default_period: None,
        }
    }

    #[test]
    fn view_registers_and_flattens_widgets_into_catalog() {
        let plugin = AdminPlugin::default().view(
            AdminView::new("reports/sales", "Sales")
                .section(WidgetSection::new("S").widget(tiny_kpi("rpt_sales_total"))),
        );
        // The view is stored.
        assert_eq!(plugin.custom_views.len(), 1);
        assert_eq!(plugin.custom_views[0].path(), "reports/sales");
        // After routes() builds state, the view's widget is in the global catalog
        // so the data endpoint can resolve it. (Assert via the same flatten the
        // routes() builder performs — extract it into a helper if needed.)
    }
}
```

> Adjust the `Widget`/`KpiPayload` literal construction to match the real public shapes in `src/widgets.rs` (the explorer confirmed `Widget` fields are public: `key, title, kind, default_span, permission, data, default_period`; `WidgetDataFn::new` exists). If `KpiPayload` has no `Default`, build it with whatever its minimal constructor is. The contract is: a registered view's widgets become reachable in the flat catalog.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p umbral-admin --lib custom_view_wiring_tests`
Expected: FAIL to compile — `view`, `custom_views` not defined.

- [ ] **Step 3: Add the field + builders**

In `plugins/umbral-admin/src/lib.rs`:

In the `AdminPlugin` struct (`:160-191`), add after `restore_last_path`:
```rust
    /// Developer-registered custom views (widget pages at arbitrary paths).
    custom_views: Vec<AdminView>,
```
In `Default` (`:195-205`), add `custom_views: Vec::new(),`.

In `impl AdminPlugin` (near the other builders), add:
```rust
    /// Register a custom admin view — a widget page mounted at
    /// `{admin_base}/{view.path}`. Chainable.
    ///
    /// ```ignore
    /// AdminPlugin::default().view(
    ///     AdminView::new("reports/sales", "Sales report")
    ///         .icon("bar-chart")
    ///         .section(WidgetSection::new("This month").widget(revenue_kpi())),
    /// )
    /// ```
    pub fn view(mut self, view: AdminView) -> Self {
        self.custom_views.push(view);
        self
    }

    /// Batch form of [`view`](Self::view).
    pub fn views(mut self, views: impl IntoIterator<Item = AdminView>) -> Self {
        self.custom_views.extend(views);
        self
    }
```

- [ ] **Step 4: Add `custom_views` to `AdminState`, flatten widgets, mount routes**

In the `AdminState` struct (`:482-503`) add:
```rust
    /// Developer-registered custom views, for the page handler + sidebar.
    custom_views: Arc<Vec<AdminView>>,
```

In `routes()` (`:590-611`), after the existing `catalog` is built and BEFORE constructing `state`, also flatten the custom-view widgets into `catalog` and warn on duplicate keys:
```rust
        // Custom-view widgets join the same flat catalog so the per-key
        // data endpoint resolves them unchanged. Keys are global → warn on dups.
        let mut catalog = catalog; // make mutable
        let mut seen_keys: std::collections::HashSet<&str> =
            catalog.iter().map(|w| w.key).collect();
        for v in &self.custom_views {
            for w in v.sections().iter().flat_map(|s| s.widgets.iter()) {
                if !seen_keys.insert(w.key) {
                    tracing::warn!(
                        widget_key = w.key,
                        view = v.path(),
                        "duplicate widget key across dashboard/custom views; \
                         the data endpoint resolves the first match"
                    );
                }
                catalog.push(w.clone());
            }
        }
```
Add `custom_views: Arc::new(self.custom_views.clone()),` to the `AdminState { ... }` literal.

Then, after the existing `.route(...)` chain that builds the `Router` (the chain ends near `:773`), bind the router to a variable and append a route per view. Restructure the tail of `routes()` so the big `Router::new().route(...)...` expression is assigned to `let mut router = ...;`, then:
```rust
        for v in &self.custom_views {
            let slug = v.path().to_string();
            let full = route(&format!("/{}", v.path()), &self.base_path);
            router = router.route(
                &full,
                axum::routing::get({
                    let slug = slug.clone();
                    move |state: axum::extract::State<AdminState>, headers: axum::http::HeaderMap| {
                        let slug = slug.clone();
                        async move { crate::handlers::custom_view::custom_view(state, headers, slug).await }
                    }
                }),
            );
        }
        router.with_state(state)
```
(Keep the existing `.with_state(state)` as the final call — move it to after the loop as shown. The per-view route closure clones `slug` per invocation so the handler is `Clone`.)

> Sequencing note: this references `handlers::custom_view::custom_view`, defined in Task 5. Implement Task 5's handler+module FIRST (or in the same change) so this compiles. If executing strictly in order, add `handlers/custom_view.rs` with the handler signature stub now and flesh it out in Task 5. The recommended execution merges Task 4 + Task 5 if your runner can't tolerate a transient non-compiling state.

Also extend `route_paths()` (`:775`) to include each `format!("{}/{}", base, v.path())`.

- [ ] **Step 5: Run the wiring test**

Run: `cargo test -p umbral-admin --lib custom_view_wiring_tests`
Expected: PASS (after Task 5's handler exists so the crate compiles).

- [ ] **Step 6: Commit** (combined with Task 5 if merged — see note)

```bash
cargo fmt -p umbral-admin && cargo clippy -p umbral-admin --all-targets
git add plugins/umbral-admin/src/lib.rs
git commit -m "feat(admin): register custom views (field, builders, routing, catalog)

AdminPlugin::view()/views(); AdminState.custom_views; mount GET
{base}/{path} per view; flatten view widgets into the global catalog so
the data endpoint serves them; dup-key warn; route_paths extended.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Page handler + template

**Files:**
- Create: `plugins/umbral-admin/src/handlers/custom_view.rs`
- Modify: `plugins/umbral-admin/src/handlers/mod.rs` (add `pub(crate) mod custom_view;`)
- Create: `plugins/umbral-admin/templates/custom_view.html`
- Modify: `plugins/umbral-admin/src/engine.rs` (register `custom_view.html`)

**Interfaces:**
- Consumes: `AdminView` (1), `permcheck::require_codename` (2), the `widget_grid` macro (3), `AdminState.custom_views` (4).
- Produces: `pub(crate) async fn custom_view(State<AdminState>, HeaderMap, String) -> Response`.

- [ ] **Step 1: Write the failing test** — covered by Task 7's integration tests (the page-render test). For this task, the gate is "the crate compiles and a booted view returns 200"; the behavioral assertion lives in Task 7. (No separate unit test here — a handler that renders a template is integration-tested.)

- [ ] **Step 2: Implement the handler**

Create `plugins/umbral-admin/src/handlers/custom_view.rs`:

```rust
//! Custom-view page handler. Renders a developer-registered widget page
//! inside the admin chrome. See `src/views.rs` + the design spec.

use axum::extract::State;
use axum::http::HeaderMap;
use minijinja::context;
use umbral::web::{IntoResponse, Response, StatusCode};

use crate::auth::require_staff;
use crate::engine::render;
use crate::permcheck;
use crate::view::sidebar_apps;
use crate::AdminState;

pub(crate) async fn custom_view(
    State(state): State<AdminState>,
    headers: HeaderMap,
    slug: String,
) -> Response {
    let current_path = format!("{}/{}", crate::branding::current().base_path, slug);
    let user = match require_staff(&headers, &current_path).await {
        Ok(u) => u,
        Err(r) => return r,
    };

    let view = match state.custom_views.iter().find(|v| v.slug() == slug) {
        Some(v) => v,
        None => return (StatusCode::NOT_FOUND, "umbral-admin: unknown view").into_response(),
    };

    if let Some(code) = view.permission() {
        if let Err(r) = permcheck::require_codename(&user, code).await {
            return r;
        }
    }

    let apps = sidebar_apps(&state, &user).await;

    // Same widget-section JSON shape the dashboard handler emits.
    let widget_sections: Vec<serde_json::Value> = view
        .sections()
        .iter()
        .map(|section| {
            let widgets_json: Vec<serde_json::Value> = section
                .widgets
                .iter()
                .map(|w| {
                    serde_json::json!({
                        "key":   w.key,
                        "title": w.title,
                        "kind":  w.kind.as_str(),
                        "span":  { "cols": w.default_span.cols, "rows": w.default_span.rows },
                    })
                })
                .collect();
            serde_json::json!({
                "title":    section.title,
                "subtitle": section.subtitle,
                "widgets":  widgets_json,
            })
        })
        .collect();

    let breadcrumbs = vec![serde_json::json!({ "label": view.title(), "url": "" })];

    match render(
        "admin/custom_view.html",
        context! {
            user => user.username,
            page_title => view.title(),
            page_subtitle => view.subtitle(),
            widget_sections => widget_sections,
            apps => apps,
            view_groups => Vec::<serde_json::Value>::new(), // populated in Task 6
            active_view => slug,
            active_table => "",
            breadcrumbs => breadcrumbs,
        },
    ) {
        Ok(html) => html.into_response(),
        Err(e) => e.into_response(),
    }
}
```

> Adjust `user => user.username` / `initial_theme` to match exactly what the dashboard handler passes (grep `handlers/list.rs::index`'s `context!`), so the chrome renders identically (theme toggle, csrf, etc. come through ambient merge). If `breadcrumbs` shape differs in base.html, match base.html's expected `{label,url}`.

- [ ] **Step 3: Create the template**

Create `plugins/umbral-admin/templates/custom_view.html`:

```html
{% extends "admin/base.html" %}
{% from "admin/_macros/widget_grid.html" import widget_grid %}
{% block title %}{{ page_title }} — {{ site_title }}{% endblock %}
{% block content %}
<div class="mb-xl">
  <h1 class="font-h1 text-h1 text-on-surface leading-tight">{{ page_title }}</h1>
  {% if page_subtitle %}
  <p class="text-body-sm text-on-surface-variant mt-xs">{{ page_subtitle }}</p>
  {% endif %}
</div>
{{ widget_grid(widget_sections, admin_base) }}
{% endblock %}
```

- [ ] **Step 4: Register the template + module**

In `plugins/umbral-admin/src/handlers/mod.rs` add `pub(crate) mod custom_view;`.

In `plugins/umbral-admin/src/engine.rs` register:
```rust
        env.add_template(
            "admin/custom_view.html",
            include_str!("../templates/custom_view.html"),
        )
        .expect("admin/custom_view.html parses");
```

- [ ] **Step 5: Build + verify it compiles and renders**

Run: `cargo build -p umbral-admin && cargo test -p umbral-admin --lib custom_view_wiring_tests`
Expected: compiles; wiring test PASS. (Full page-render behavior verified in Task 7.)

- [ ] **Step 6: Commit**

```bash
cargo fmt -p umbral-admin && cargo clippy -p umbral-admin --all-targets
git add plugins/umbral-admin/src/handlers/custom_view.rs plugins/umbral-admin/src/handlers/mod.rs plugins/umbral-admin/templates/custom_view.html plugins/umbral-admin/src/engine.rs
git commit -m "feat(admin): custom-view page handler + template

Render a registered AdminView inside the admin chrome: require_staff +
codename gate, build widget_sections JSON, render custom_view.html which
reuses the shared widget_grid macro.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Sidebar integration

**Files:**
- Modify: `plugins/umbral-admin/src/view.rs` (a `view_groups` builder)
- Modify: `plugins/umbral-admin/templates/base.html` (sidebar loop)
- Modify: `plugins/umbral-admin/src/handlers/list.rs` (dashboard `index` + changelist `list`) and `src/handlers/custom_view.rs` to pass `view_groups` into their contexts.

**Interfaces:**
- Consumes: `AdminState.custom_views` (4), `permcheck::has_codename` (2).
- Produces: a `view_groups` context var (`Vec<{ label, views: [{href, label, icon, slug}] }>`), permission-filtered, `.hidden()`-excluded, grouped by `.group()` (default "Pages"); rendered in `base.html` and highlighting `active_view`.

- [ ] **Step 1: Write the failing test**

Add to `plugins/umbral-admin/tests/custom_views.rs` (created in Task 7, or stage here) a sidebar test (full text in Task 7). The contract: a non-hidden registered view appears as a sidebar link `{base}/{path}`; a `.hidden()` view does not.

- [ ] **Step 2: Implement the `view_groups` builder**

In `plugins/umbral-admin/src/view.rs`, add an async fn that mirrors `sidebar_apps`'s access to `state`:

```rust
/// Build the sidebar's custom-view groups: non-hidden views the user may
/// see (codename-filtered), clustered by `.group()` (default "Pages"),
/// preserving registration order within a group and first-seen group order.
pub(crate) async fn view_groups(
    state: &AdminState,
    user: &umbral_auth::AuthUser,
) -> Vec<serde_json::Value> {
    use std::collections::BTreeMap; // stable iteration; or keep insertion order via Vec of (name, items)
    let base = crate::branding::current().base_path;
    // Preserve first-seen group order.
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    for v in state.custom_views.iter() {
        if v.hidden() {
            continue;
        }
        if let Some(code) = v.permission() {
            if !crate::permcheck::has_codename(user, code).await {
                continue;
            }
        }
        let group = v.group().unwrap_or("Pages").to_string();
        if !groups.contains_key(&group) {
            order.push(group.clone());
        }
        groups.entry(group).or_default().push(serde_json::json!({
            "href":  format!("{}/{}", base, v.path()),
            "label": v.title(),
            "icon":  v.icon().unwrap_or("file-text"),
            "slug":  v.slug(),
        }));
    }
    order
        .into_iter()
        .map(|name| {
            let views = groups.remove(&name).unwrap_or_default();
            serde_json::json!({ "label": name, "views": views })
        })
        .collect()
}
```

> `AdminState` is `pub(crate)`-visible from `view.rs` already (it builds `sidebar_apps`). Match the exact `AuthUser` import path used in `view.rs`.

- [ ] **Step 3: Render in `base.html`**

In `plugins/umbral-admin/templates/base.html`, immediately after the model-group loop (`{%- endfor %}` that closes `{%- for app in apps %}` at ~line 230), add:

```html
        <!-- Custom view groups -->
        {%- for vg in view_groups | default(value=[]) %}
        <div class="sidebar-plugin-group" id="sidebar-group-view-{{ loop.index }}">
            <p class="sidebar-section-label px-md mb-xs font-label-sm text-label-sm text-outline uppercase tracking-wider">{{ vg.label }}</p>
            <div class="space-y-1 px-md">
                {%- for v in vg.views %}
                <a
                    href="{{ v.href }}"
                    class="sidebar-link sidebar-model-link flex items-center gap-2 px-sm py-1.5 rounded-xl border border-transparent {% if active_view is defined and active_view == v.slug %}bg-primary-container text-on-primary-container{% else %}text-on-surface-variant hover:bg-surface-container-high hover:text-on-surface{% endif %} font-body-sm transition-all"
                    data-sidebar-tooltip="{{ v.label }}"
                    aria-label="{{ v.label }}"
                >
                    <i data-lucide="{{ v.icon }}" class="w-[18px] h-[18px] flex-shrink-0"></i>
                    <span class="sidebar-text flex-1 truncate">{{ v.label }}</span>
                </a>
                {%- endfor %}
            </div>
        </div>
        {%- endfor %}
```

> `active_view` is only defined on custom-view pages; the `is defined` guard keeps model pages working. `| default(value=[])` keeps it safe where the handler doesn't pass `view_groups`.

- [ ] **Step 4: Thread `view_groups` into the contexts**

- In `custom_view.rs` (Task 5), replace the `view_groups => Vec::<serde_json::Value>::new()` placeholder with `view_groups => crate::view::view_groups(&state, &user).await`.
- In `handlers/list.rs::index` (dashboard) and `::list` (changelist), add `view_groups => crate::view::view_groups(&state, &user).await,` to their `context!` calls so the custom-view links appear in the sidebar on every admin page (matching how `apps` is universal). Use the same `user` binding each handler already has.

- [ ] **Step 5: Run the sidebar test**

Run: `cargo test -p umbral-admin --test custom_views` (the sidebar assertions from Task 7)
Expected: PASS — non-hidden view link present; hidden absent.

- [ ] **Step 6: Commit**

```bash
cargo fmt -p umbral-admin && cargo clippy -p umbral-admin --all-targets
git add plugins/umbral-admin/src/view.rs plugins/umbral-admin/templates/base.html plugins/umbral-admin/src/handlers/list.rs plugins/umbral-admin/src/handlers/custom_view.rs
git commit -m "feat(admin): custom views in the sidebar nav

Permission-filtered, grouped (default 'Pages'), .hidden()-excluded view
links rendered in base.html alongside model groups; threaded into the
dashboard, changelist, and custom-view page contexts; active-state on the
current view.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Behavioral tests + docs + verification

**Files:**
- Create: `plugins/umbral-admin/tests/custom_views.rs`
- Create: `documentation/docs/v0.0.1/admin/custom-views.mdx`

- [ ] **Step 1: Write the integration tests**

Create `plugins/umbral-admin/tests/custom_views.rs`. Model the harness on `tests/phase4_dashboard.rs` — build an app/router with `AdminPlugin::default().view(...)`, register a tiny widget, log in a staff user, and `send` requests. Implement these tests (adapt helper names + app-boot to the existing test harness in that file family):

```rust
// Behavioral coverage for custom admin views (design spec 2026-07-01).
// 1. A registered view renders inside the chrome with its widget cell.
// 2. The view's widget is reachable via the global data endpoint.
// 3. The view link appears in the sidebar; a .hidden() view does not.
// 4. A codename-gated view 403s without the codename (permissions installed),
//    200s with it; staff-only baseline (no permissions plugin) → 200.
```

Concretely:
- `test_custom_view_page_renders`: register `AdminView::new("reports/sales","Sales report").section(WidgetSection::new("This month").widget(test_kpi("rpt_total")))`; `GET /admin/reports/sales/` → 200; body contains `"Sales report"` and `id="widget-rpt_total"` and `/api/dashboard/widgets/rpt_total/data`.
- `test_custom_view_widget_data_served`: `GET /admin/api/dashboard/widgets/rpt_total/data` → 200 (proves the flatten-into-catalog).
- `test_custom_view_in_sidebar`: dashboard `GET /admin/` body contains `href="/admin/reports/sales/"`.
- `test_hidden_view_routable_but_not_in_sidebar`: a `.hidden()` view → `GET` its path 200, but dashboard body does NOT contain its href.
- (If a permissions-enabled boot is feasible in this harness, add `test_view_permission_gate`: gated view → 403 for a staff user without the codename. If wiring `PermissionsPlugin` into the test app is heavy, assert the staff-only baseline instead — view with a permission set but no permissions plugin installed → 200 — and note the full 403 path is covered by `permcheck`'s own behavior.)

- [ ] **Step 2: Run the tests**

Run: `cargo test -p umbral-admin --test custom_views`
Expected: PASS (all).

- [ ] **Step 3: Write the doc page**

Create `documentation/docs/v0.0.1/admin/custom-views.mdx` with frontmatter (`title`, `description`, `sidebar_position`) and: one paragraph on purpose; the smallest example (register a view with one widget section); a note that views appear in the sidebar (grouped, with icon) and gate via `.permission(codename)`; and a link to the design spec. Keep it minimal (ship-a-feature-ship-a-doc), MDX with Specra components, no component imports.

- [ ] **Step 4: Rebuild CSS only if new utilities were introduced**

The new templates reuse existing classes; if `grep` shows any new Tailwind utility not already in `admin.css`, run `cd plugins/umbral-admin/css && npm run build` and stage the regenerated `src/assets/admin.css`. Otherwise skip.

- [ ] **Step 5: Full workspace verification**

Run (from repo root): `cargo fmt -p umbral-admin && cargo clippy -p umbral-admin --all-targets && cargo build && cargo test -p umbral-admin`
Expected: all clean. (Whole-workspace `cargo build` catches facade-reexport breakage from the `pub use views::AdminView`.)

- [ ] **Step 6: Commit**

```bash
git add plugins/umbral-admin/tests/custom_views.rs documentation/docs/v0.0.1/admin/custom-views.mdx
# add plugins/umbral-admin/src/assets/admin.css only if rebuilt in Step 4
git commit -m "test(admin): custom-view behavioral tests + docs

Page render, widget-data reachability, sidebar link, hidden exclusion,
permission baseline. Plus the admin/custom-views.mdx doc page.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- `AdminView` builder (path/title/subtitle/icon/group/permission/hidden/sections) → Task 1. ✓
- `AdminPlugin::view()/views()` + flatten into catalog + dup-key warn → Task 4. ✓
- Route mounting under admin base, chrome-wrapped → Tasks 4 (route) + 5 (handler/template). ✓
- Reuse widget data endpoint + DRY grid macro → Task 3 (macro) + 4 (flatten) + 5 (render). ✓
- Codename permission gating + graceful no-op → Task 2 + 5 (gate) + 6 (sidebar filter). ✓
- Sidebar integration (grouped, icon, permission-filtered, hidden opt-out, active-state) → Task 6. ✓
- Tests (render, data, sidebar, hidden, permission) + doc page → Task 7. ✓
- Out-of-scope (runtime authoring, page filters, custom templates) → not built; Global Constraints + spec note. ✓

**Placeholder scan:** The verbatim-copy instruction in Task 3 Step 3 points at exact line numbers (`dashboard.html:84-223`) rather than repeating 140 lines — acceptable (repeating risks divergence; the lines are pinned). The "adjust to match real shape" notes in Tasks 1/4/5 name the exact file to grep and the contract — not vague TODOs. No "handle edge cases"/"TBD".

**Type/name consistency:** `AdminView::slug()` == normalized path, used as the per-route closure key (Task 4) and the sidebar `active_view`/`slug` (Tasks 5, 6) and the handler lookup (`v.slug() == slug`, Task 5). `view_groups` shape `{label, views:[{href,label,icon,slug}]}` defined in Task 6 Step 2 matches the base.html loop in Step 3. `has_codename`/`require_codename` defined in Task 2 used in Tasks 5/6. `widget_grid(widget_sections, admin_base)` defined Task 3 called in dashboard.html (3) + custom_view.html (5). The widget-section JSON shape in custom_view.rs (Task 5) matches `handlers/list.rs:274-299`.

**Sequencing note flagged:** Task 4's route wiring references Task 5's handler; the plan explicitly says to provide the handler (Task 5) before/with Task 4's compile, or merge them. A subagent runner should execute Task 5's handler module creation as part of, or immediately before, Task 4's `routes()` edit to avoid a transient non-compiling crate.
