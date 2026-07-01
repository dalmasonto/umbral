---
name: admin-custom-views-and-widget-permissions
description: Use when adding/modifying admin custom views (AdminPlugin::view / AdminView), dashboard widgets, or the widget permission gates — explains how a "custom view" is wired and how widget permissions are enforced across render + data endpoint + sidebar.
---

# umbral-admin custom views + widget permissions

## Context
`AdminPlugin::view(AdminView::new("reports/sales","Sales").section(...))` registers a widget page served at `{admin_base}/custom-views/reports/sales/` (the view's path lives under the dedicated, hyphenated `custom-views/` URL namespace + a trailing slash). It's "a dashboard at any path" — it reuses the existing widget system rather than introducing a parallel one. This skill maps the wiring so you don't re-discover it. Shipped 2026-07-01 (features.md #76); spec/plan in `docs/superpowers/specs/2026-07-01-admin-custom-views-design.md` + the sibling plan.

## How a custom view is wired (all in `plugins/umbral-admin`)
1. **`AdminView`** (`src/views.rs`) — builder. NOTE the setter/accessor split: setters are `with_subtitle/with_icon/with_group/with_permission/hide()/section()/add_sections()`; accessors are the BARE names (`path/slug/title/subtitle/icon/group/permission/hidden/sections`). Rust forbids a same-name setter+accessor (E0592) — that's why setters carry `with_`/`hide`. `slug()` == the normalized path.
2. **Registration** (`src/lib.rs`) — `AdminPlugin.custom_views: Vec<AdminView>`; `.view()/.views()` builders. In `routes()`, each view's section widgets are **flattened into the same flat `catalog: Vec<Widget>`** that backs `AdminState.widget_catalog`, so the existing `GET {base}/api/dashboard/widgets/{key}/data` endpoint serves them with zero new plumbing. Dup widget keys → `tracing::warn!` (keys are global). Each view mounts `GET {base}/custom-views/{path}/` (trailing slash) via a closure that clones the view's slug per-invocation (so the handler stays `Clone`). The `custom-views/` prefix is hyphenated so it can never be a snake_case table name → no collision with the `{table}/` changelist, no built-in-route collision, no table shadowing. `resolved_custom_views()` therefore only drops empty + duplicate paths (a duplicate would still panic axum's router). The trailing slash matches the admin convention and works with `SlashRedirect::Append`.
3. **Page handler** (`src/handlers/custom_view.rs`) — `require_staff` → `require_codename(view.permission())` → build the widget-section JSON via the shared helper → render `templates/custom_view.html` (which `{% extends "admin/base.html" %}` and reuses the `widget_grid` macro).
4. **Shared rendering** — `templates/_macros/widget_grid.html` is the ONE widget-grid macro, used by both `dashboard.html` and `custom_view.html` (imported `{% from "admin/_macros/widget_grid.html" import widget_grid %}`). Register any new macro template in `src/engine.rs` (`add_template`) or the `{% from %}` import fails.
5. **Sidebar** (`src/view.rs::view_groups` + `templates/base.html`) — non-hidden, permission-filtered views grouped by `.with_group()` (default "Pages"). Threaded into the dashboard, changelist, AND custom-view contexts (all three handlers pass `view_groups`). The href uses `| safe` because minijinja OWASP-escapes `/` to `&#x2f;` in serde_json string fields — the path is developer-authored (compile-time), never user input, same rationale as `admin_base`'s `from_safe_string`.

## Widget permissions — three enforcement points (all must agree)
Permission is a codename string checked via `permcheck::has_codename(user, code)` / `require_codename` (graceful no-op → `true` when `PermissionsPlugin` absent). There are TWO permission sources and THREE enforcement sites:

- **Sources:** a view's `.with_permission(code)` (gates the whole view) AND a widget's own `Widget::permission: Option<&'static str>` field (gates one widget anywhere).
- **Site 1 — page + sidebar:** `require_codename(view.permission())` in the page handler; `view_groups` filters gated views out of the sidebar.
- **Site 2 — render filter:** `view::accessible_widget_sections_json(sections, user)` OMITS any widget whose `widget.permission` the user lacks. Used by BOTH the dashboard and custom-view handlers (it also de-dups the widget-section JSON builder). An all-filtered section renders nothing (the grid template guards `{% if section.widgets | length > 0 %}`).
- **Site 3 — data endpoint:** `handlers::dashboard::dashboard_widget_data` DUAL-gates: (a) the `AdminState.widget_gates` map (`widget_key → view_codename`, built at `routes()` for custom-view widgets) AND (b) the widget's own `widget.permission`. Both must pass. **This is the load-bearing gate** — the page/sidebar gates are UX; the data endpoint is the real boundary (a staff user can hit `/api/dashboard/widgets/{key}/data` directly).

## Why the data-endpoint gate matters
The final review of #76 caught that gating the PAGE while leaving the widget-DATA endpoint on `require_staff`-only leaks the data (`gate the door, not the window`). If you add any new widget-data surface, gate it the same way. "Secure by default" (CLAUDE.md) is why all three sites exist.

## Pitfalls
- Adding a widget to a view but forgetting it lands in the GLOBAL `widget_catalog` — a duplicate key silently resolves to the FIRST match (dashboard wins); watch the dup-key warn.
- Editing `dashboard.html`'s grid inline instead of the `widget_grid` macro — the macro is shared; edit it once.
- Forgetting to register a new template in `engine.rs` → `{% from %}`/`{% extends %}` panics at first render (`.expect(...)`).
- `cargo fmt -p umbral-admin` reformats the WHOLE crate (the committed code isn't rustfmt-canonical); `git restore` unrelated churn before committing. Never bare `cargo fmt`.
- gaps3 #6-8 are SHIPPED: the catalog endpoint filters by `widget.permission`; the render filter batches permission checks via `join_all`; view-path validation drops duplicates. The earlier "single-segment view shadows a table" caveat is GONE — custom views now live under the hyphenated `/custom-views/` namespace, which can't be a table name, so there's nothing left to shadow. `resolved_custom_views()` only rejects empty + duplicate paths now.

## See also
- `docs/superpowers/specs/2026-07-01-admin-custom-views-design.md`
- `.claude/skills/admin-tailwind-theme-pipeline.md` (the CSS side of the admin)
- CLAUDE.md → "Plugins use the ORM" + "Secure by default" + "Fix, don't patch".
