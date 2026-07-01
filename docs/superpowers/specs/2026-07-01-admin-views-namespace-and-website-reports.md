# Admin custom-view URL namespace + umbral_website Reports view — design

Date: 2026-07-01
Area: `plugins/umbral-admin` (framework) + `umbral_website` (consumer)
Follows: the custom admin views feature (features.md #76, shipped earlier today) and its follow-ups (gaps3 #6-8).
Status: design, pending implementation plan

## Problem

Custom admin views (shipped today) mount at `<admin_base>/<path>` — the same shape as the model changelist route `<admin_base>/<table>/`. Two consequences surfaced when wiring the feature into `umbral_website`:

1. **Trailing-slash miss.** The whole admin uses trailing-slash URLs (`/admin/`, `/admin/{table}/`). A view registered at `/admin/reports` is NOT reachable at `/admin/reports/` (the conventional URL) — that request matches `/admin/{table}/` instead and returns "no model with table `reports`". `SlashRedirect::Append` doesn't help: it only rewrites *no-trailing → trailing* on a 404, never the reverse.
2. **Namespace collision.** A view path shares the single-segment space with table names, so a view can shadow (or be confused with) a real model's changelist, and even a `views/`-style prefix could clash with a model literally named `View`/`Views`.

## Decision

Give custom views their own URL namespace: **`<admin_base>/custom-views/<path>/`** (trailing slash).

- **Hyphenated prefix = collision-proof.** Table names are snake_case (underscores only; derived from struct names) and can never contain a hyphen, so `custom-views` can never be a table name. This removes every collision class: built-in routes, model tables, and the reserved-prefix concern all go away.
- **Trailing slash** matches the admin's convention and works with `SlashRedirect::Append` (a no-trailing hit 404s and redirects to the trailing form) and `Off` (all admin URLs are trailing anyway).
- `AdminView::new("reports", "Reports")` → served at `/admin/custom-views/reports/`. Multi-segment paths (`AdminView::new("ops/audit", …)`) → `/admin/custom-views/ops/audit/`.

## Framework changes (`plugins/umbral-admin`)

1. **Mount** (`src/lib.rs`, the per-view route loop, ~line 890): `route(&format!("/custom-views/{}/", v.path()), &self.base_path)` (was `format!("/{}", v.path())`).
2. **`route_paths()`** (`src/lib.rs`, ~line 974): the per-view `RouteSpec` path → `format!("{}/custom-views/{}/", self.base_path, v.path())`.
3. **Sidebar href** (`src/view.rs`, `view_groups`): `"href": format!("{}/custom-views/{}/", base, v.path())`.
4. **Handler login-redirect path** (`src/handlers/custom_view.rs`): the `current_path` passed to `require_staff` → `format!("{}/custom-views/{}/", base, slug)` so the post-login bounce returns to the view.
5. **Simplify `resolved_custom_views`** (`src/lib.rs`): drop the `RESERVED_FIRST` list — under the `custom-views/` namespace a view can't collide with a built-in route or a table. Keep only: reject empty path, reject duplicate path (two views at the same `/custom-views/x/` still conflict in axum). Update the doc-comment to say the namespace is what prevents built-in/table collisions. (This retires gaps3 #7's leftover "single-segment view shadows a table" caveat entirely.)
6. **Tests** (`tests/custom_views.rs`, `tests/custom_views_sidebar.rs`): update every custom-view URL to the `/admin/custom-views/<path>/` form (page render, widget-data reachability is unchanged — that endpoint is `/admin/api/dashboard/widgets/{key}/data`, not affected; permission-gate page URLs; the sidebar href assertion → `/admin/custom-views/<path>/`). The `resolved_custom_views_drops_reserved_and_duplicate_paths` unit test → drop the reserved cases, keep the duplicate + (optionally) empty case; rename to `…_drops_duplicate_paths`.
7. **Docs**: `documentation/docs/v0.0.1/admin/custom-views.mdx` — update the example/URL to `/admin/custom-views/…`. Skill `.claude/skills/admin-custom-views-and-widget-permissions.md` — update the routing section (namespace + trailing slash; note the reserved-prefix logic is gone).

No change to: the `AdminView` builder API, the widget flatten/catalog, the widget-data endpoint, the permission model, or `AdminPlugin::view()`. Only the mounted URL scheme + the now-simpler path validation.

## Website changes (`umbral_website`)

1. **`src/widgets/reports.rs`** (new, re-exported from `widgets/mod.rs`): `pub fn reports_view() -> AdminView`.
   - `AdminView::new("reports", "Reports").with_icon("bar-chart-3").with_group("Insights")` with three sections reusing the existing builders:
     - **Composition** — `source_mix_donut`, `status_mix_donut`, `submissions_bar`, `status_maturity_heatmap`
     - **Trends** — `submissions_chart` (7d), `activity_chart` (7d)
     - **Gauges & rankings** — `audit_coverage_radial`, `plugins_by_maturity`, `shipped_kpi`
   - Each reused widget is re-keyed to avoid colliding with the dashboard's `pd_*` copies in the global catalog, via a private helper `fn rekey(mut w: Widget, key: &'static str) -> Widget { w.key = key; w }` and `rpt_*` literal keys (e.g. `rpt_source_mix_donut`). Same data fns → same aggregates as the dashboard, independently-keyed cells.
2. **`src/main.rs`**: append `.view(widgets::reports_view())` to the `AdminPlugin` chain (after the dashboard sections). Dashboard untouched.

Result: `/admin/custom-views/reports/` renders the three analytics sections with the neutral theme + card system; the sidebar gains an **Insights → Reports** link.

## Testing / verification

- Framework: `cargo test -p umbral-admin` — the updated `custom_views` + `custom_views_sidebar` suites pass at the new URLs; the `resolved_custom_views` unit test covers the duplicate case; full suite green except the proven-pre-existing `sidebar_home_link_plain_when_flag_off`.
- Website: `cd umbral_website && cargo build` (compiles against the new admin; confirms `reports_view()` wiring). No `cargo run` (per the standing rule — the user verifies `/admin/custom-views/reports/` on their running dev server).
- Manual (user): the Reports page renders the charts, the sidebar shows Insights → Reports, and the link resolves.

## Out of scope

- Per-view permission on the Reports view (any staff; not gated).
- New widgets (reuse only).
- Dashboard changes.
- A website test harness (none exists; not adding one).
