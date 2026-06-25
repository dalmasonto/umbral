# Umbral Admin — Backend PRD (data & APIs)

| | |
|---|---|
| **Scope** | How `umbral-admin` is generated, scoped, powered, and persisted |
| **Audience** | Back-end / framework implementation |
| **Status** | Draft v0.1 · May 30, 2026 |
| **Companion** | `umbral-admin-design-prd.md` (UI/UX) · `arch.md` (architecture) |

---

## 1. Where this fits

`umbral-admin` is a **built-in plugin** like any other (it implements `Plugin`, owns its own
migrations, registers routes). It introspects models registered with the ORM and serves a
JSON/data API that the admin front-end (designed in the companion PRD) consumes. Nothing here is
special-cased; the admin is proof the plugin contract is strong enough to build a product on.

The front-end is a single-page app served by the plugin; all dynamic content comes from the
**Admin Data API** (§5). The design PRD's "some part is for the backend" is precisely this:
*model metadata, the dashboard widget catalog, widget data, permissions, and saved layouts are
all backend-defined and API-served; the front-end composes and renders them.*

---

## 2. Model registration (`AdminModel`)

A developer opts a model into the admin and customizes its presentation through a per-model
configuration type:

```rust
admin.register::<Post>(AdminModel {
    list_display:   &["title", "author", "published", "published_at"],
    list_filter:    &["published", "tags", "author"],
    search_fields:  &["title", "body"],
    ordering:       &["-published_at"],
    list_per_page:  25,
    inlines:        &[inline::<Comment>()],     // related-model inline editing
    actions:        &[publish_selected, ...],   // bulk actions
    readonly_fields:&["created_at", "updated_at"],
    ..Default::default()
});
```

The admin derives form fields, column types, filter widgets, and FK/M2M editors from ORM model
metadata (field types, relationships, choices, nullability). Unregistered models do not appear.

File/image fields are configurable on `AdminModel` (`upload_to`, `max_size`, `accept` mime
allow-list, whether to generate thumbnails); each serializes to the file descriptor in §5.7 so
the UI can preview or offer download. Row and bulk **actions** are declared here too (the
`actions` field) and surface via §5.6.

---

## 3. Per-app / per-plugin scoping

The sidebar groups models **by the plugin that owns them**, because registration is plugin-scoped:

- Each `Plugin` exposes its admin registrations (models + dashboard widgets) via an admin hook
  (e.g. an `admin(&self, reg: &mut AdminRegistry)` method, or by calling `admin.register` in
  `on_ready`).
- The `AdminRegistry` records, per registration, the **owning app/plugin name** (`plugin.name()`).
- The sidebar tree is built from `registry.apps()` → each app → its registered models. This is
  what produces the *Blog / Auth / Newsletter* grouping in the design with zero manual config.

---

## 4. Dashboard widget system (the customizable, backend-defined core)

The dashboard separates **definition (backend)** from **composition (per-user)**:

### 4.1 Widget definitions — registered in Rust
Plugins/developers register the *available* widgets and how to compute their data:

```rust
admin.register_widget(Widget {
    key:        "users_total",
    title:      "Total Users",
    kind:       WidgetKind::Kpi,            // Kpi | Line | Bar | Donut | Table | Feed | QuickActions | Custom
    default_span: Span { cols: 3, rows: 1 },
    permission: Some("auth.view_user"),
    data:       WidgetData::query(|ctx| async move {
        // returns a typed payload matching `kind` (see §5.3 contracts)
        kpi(User::objects().count().await?, /*delta*/ ...)
    }),
    config_schema: None,                    // optional per-instance settings (e.g. time range)
});
```

- The widget **catalog** = all registered widgets the current user has permission to see.
- Widget data is computed **on the backend** (a query or closure), returned via the data API —
  the front-end never queries the DB directly.
- A plugin can ship dashboard widgets about its own models (Blog ships "Posts over time"; Auth
  ships "Signups this week"), so the dashboard grows with installed plugins.

### 4.2 Default dashboard
Developers can declare a **default layout** (which widgets, where) shipped with the app, used for
first-run and for users who haven't customized. A "reset to default" returns to it.

### 4.3 Per-user composition
Each admin user arranges their own grid from the catalog (add/remove/move/resize, per-instance
config like chart range). This **layout is persisted per user** (§6).

---

## 5. Admin Data API

All endpoints are authenticated, permission-checked (§7), CSRF-protected for mutations, and
namespaced under `/admin/api/`.

### 5.1 Metadata & navigation
| Endpoint | Returns |
|---|---|
| `GET /admin/api/nav` | apps → models the user may see (+ counts) → builds the sidebar |
| `GET /admin/api/models/{model}/schema` | fields, types, filters, form layout, inlines, actions |

### 5.2 Changelist & records (CRUD)
| Endpoint | Purpose |
|---|---|
| `GET /admin/api/models/{model}` | list: `?search=&filter[...]=&ordering=&page=&page_size=` → rows + facets + page meta |
| `GET /admin/api/models/{model}/{id}` | detail (incl. inline children) |
| `POST /admin/api/models/{model}` | create |
| `PATCH /admin/api/models/{model}/{id}` | update |
| `DELETE /admin/api/models/{model}/{id}` | delete |
| `POST /admin/api/models/{model}/actions/{action}` | bulk action over selected ids |

### 5.3 Relation options (powers async FK/M2M selectors)
The admin **must not** load entire related tables into the form (the naive approach that fails on
large tables). FK and M2M selectors fetch options on demand from a dedicated endpoint:

| Endpoint | Purpose |
|---|---|
| `GET /admin/api/models/{model}/{field}/options` | searchable, paginated choices for a relation field |
| `GET /admin/api/models/{model}/{field}/options/resolve?ids=…` | resolve labels for already-selected ids |

- Query params: `?search=&page=&page_size=` (default ~20). Filtering/matching run **in SQL**
  against the related model's configured `search_fields`; results are paginated and ordered
  (most-recent or a configured `ordering`).
- Response: `{ items: [{ value, label }], page, has_more }`. The front-end combobox infinite-
  loads via `has_more`.
- `resolve` returns labels for a small set of preselected ids (edit form initial values) without
  fetching a full page — a single `WHERE id IN (…)` lookup.
- Each relation field declares an **option label** (e.g. `__str__`-equivalent) and the
  `search_fields` used for matching, configurable on `AdminModel`; sensible defaults are derived
  from model metadata.
- Permission-checked: a user who cannot view the related model gets no options.

Reuse the same query/serialization layer that `umbral-rest` builds on where possible — the admin
is essentially an internal, permission-scoped REST consumer with extra metadata.

### 5.4 Dashboard
| Endpoint | Purpose |
|---|---|
| `GET /admin/api/dashboard/catalog` | widgets the user may add (key, title, kind, default span, config schema) |
| `GET /admin/api/dashboard/layout` | the user's saved layout (or default) |
| `PUT /admin/api/dashboard/layout` | save the user's layout |
| `GET /admin/api/dashboard/widgets/{key}/data?config=…` | compute & return one widget's data |

**Typed payload contracts per `kind`** (so the front-end renders without guessing):
- `Kpi`: `{ value, unit?, delta?, sparkline?: number[] }`
- `Line`/`Bar`: `{ series: [{ name, points: [{x, y}] }], x_type }`
- `Donut`: `{ slices: [{ label, value }] }`
- `Table`: `{ columns, rows, row_link? }`
- `Feed`: `{ items: [{ actor, verb, object, object_link?, at }] }`

### 5.5 DataTable list contract
The list endpoint (§5.2) returns everything the reusable DataTable needs to render without extra
round-trips:
- `columns`: `[{ key, label, type, sortable, width?, align?, hidden_default? }]` where `type`
  drives cell rendering (`text|number|bool|choice|date|datetime|fk|file|image|json`).
- `rows`: serialized records; `fk` cells carry `{ label, model, id }` (→ open that record's sheet);
  `file`/`image` cells carry the file descriptor from §5.7 (thumbnail + `preview_kind`).
- `facets`: available filter values/ranges for the active `list_filter` fields.
- `page`: `{ page, page_size, total, total_pages }` for the full pager (§ design 7.4).
- `row_actions`: per row, the **permission-filtered** action keys available (so the action column
  only shows what this user may do to that object).

### 5.6 Actions catalog (extensible row & bulk actions)
Actions are declared in the admin plugin (Rust) and exposed to the UI:

| Endpoint | Purpose |
|---|---|
| `GET /admin/api/models/{model}/actions` | action descriptors the user may use |
| `POST /admin/api/models/{model}/actions/{key}` | run an action over `{ ids: [...] }` or `{ all_matching: <filters> }` |

An **action descriptor**:
```rust
Action {
    key:        "publish",
    label:      "Publish",
    icon:       "send",            // Lucide icon name
    variant:    Variant::Default,  // Default | Danger
    scope:      Scope::Both,       // Row | Bulk | Both
    confirm:    Some("Publish the selected posts?"),
    permission: Some("blog.change_post"),
    handler:    |ctx, ids| async move { /* mutate; return ActionResult */ },
}
```
`ActionResult` tells the UI what to do next: `Toast`, `RefreshTable`, `OpenSheet(id)`,
`Download(file)`, or `Redirect(url)`. Built-in preview/edit/delete are expressed as actions too,
so custom ones render beside them uniformly. Bulk runs report progress + a summary result.

### 5.7 Files & previews
A file/image field serializes to a **descriptor** the UI switches on — the backend resolves the
preview kind so the front-end never sniffs bytes:
```jsonc
{
  "filename": "report.pdf",
  "size": 184320,
  "mime": "application/pdf",
  "preview_kind": "pdf",        // image|pdf|video|audio|csv|spreadsheet|text|code|document|download
  "url": "/admin/api/files/<token>",        // streamed, auth + range support
  "thumbnail_url": "/admin/api/files/<token>/thumb",  // images (server-generated) else null
  "language": null               // for `code`: e.g. "python" (drives syntax highlighting)
}
```
- **Kind resolution:** MIME + extension → `preview_kind`; unknown/binary/archive → `download`.
  `document` (doc/docx) and `spreadsheet` (xlsx) attempt server-side conversion to an HTML/PDF or
  table preview where a converter is available, else fall back to `download`.
- **Serving:** `GET /admin/api/files/{token}` streams with `Content-Disposition`, honors HTTP
  range (media seeking), and is permission-checked. Thumbnails generated and cached server-side.
- **CSV/`text`/`code`** previews stream the raw content (size-capped; "show first N rows/lines"
  for large files); CSV is rendered by the DataTable component client-side.

---

## 6. Persistence (admin owns its migrations)

`umbral-admin` owns tables created via its plugin migrations:

- `admin_dashboard_layout` — per-user widget layout (user_id, dashboard_key, JSON of
  widget instances: key, position, span, per-instance config).
- `admin_user_prefs` — theme (`light`/`dark`/`system`), density, sidebar collapsed state, etc.
- `admin_audit_log` — actor, action, model, object id, timestamp, diff summary — powers the
  activity feed widget and the per-object history link.

These appear automatically when you run `migrate` once the admin plugin is registered - the same
mechanism every plugin uses.

---

## 7. Permissions

- Every nav entry, model endpoint, action, and widget is gated by permissions resolved from
  `umbral-auth` (e.g. `app.view_model`, `app.change_model`, `app.delete_model`, custom perms).
- The **nav, catalog, and schema responses are pre-filtered** so the front-end only ever knows
  about what the user may access (no hidden-but-present items).
- Object-level permissions are supported via an optional hook on `AdminModel`.
- Staff/superuser flags gate admin access entirely.

---

## 8. Theming & customization persistence

- Theme + density + sidebar state live in `admin_user_prefs`; the front-end reads them on load
  (server-rendered initial theme to avoid flash) and writes on change.
- **Developer custom theme:** the admin exposes all design tokens as CSS variables and loads a
  developer-supplied **`admin.css`** *last* (configurable path in admin settings), so brand/color
  overrides apply without forking the admin (see design PRD §3.1). Per-user theme prefs and the
  developer's `admin.css` compose: `admin.css` sets the palette; the user picks light/dark.
- Dashboard layouts live in `admin_dashboard_layout`; "reset to default" restores the
  developer-declared default.

---

## 9. Security

- All mutations CSRF-protected; all endpoints require an authenticated admin session.
- Server-side authorization on **every** request (never trust the client's filtered nav).
- Custom Markdown/HTML widgets sanitized server-side before storage/return.
- Audit-log all create/update/delete and bulk actions.
- **File serving:** user-uploaded files are streamed through permission-checked, tokenized URLs
  with `Content-Disposition` and a restrictive `Content-Type`; never served inline as executable
  HTML/SVG-with-script. Previews render in sandboxed contexts; anything not safely previewable
  resolves to the download card. Enforce `max_size` and the `accept` allow-list on upload.

---

## 10. Non-goals (v1)

Multiple dashboards/tabs (single dashboard first), saved changelist views, real-time widget
push (poll/refresh first), a public widget-plugin marketplace. **Office-doc preview** (doc/docx/
xlsx) depends on an available server-side converter; without one it falls back to download — full
fidelity conversion is not a v1 guarantee. Revisit after the core admin is solid.

---

*Front-end layout, components, theming tokens, and Stitch prompts live in
`umbral-admin-design-prd.md`. This document defines the APIs and registration those screens
consume.*
