# Outline — Templates

| | |
|---|---|
| **Status** | Outline. Promotes at M11 entry (admin) or earlier on need. |
| **Maps to milestone** | M11 (admin), reused by M9 (email) |
| **Companions** | `01-app-and-settings.md`, `02-plugin-contract.md`, `arch.md §4.5`, outlines `admin.md`, `email.md`, `forms.md`, `security-defaults.md` |

## Purpose

`umbral::templates` is the server-side rendering substrate. Its first consumers are the admin (auto CRUD pages) and the email plugin (text and HTML bodies for password reset, welcome mail, and anything else a built-in needs to send), with user-facing pages — any non-API endpoint a developer writes by hand — riding on the same surface. It lives as a cross-cutting outline rather than inside `umbral-admin` because two built-ins already reuse it, and a third (forms widget rendering) is about to: if the admin owned the engine, the email plugin would either depend on the admin or re-implement the same wiring, both of which the plugin contract from `02-plugin-contract.md` rules out. Templates therefore graduate to a shared facility — small, ambient, and rendered through one accessor — long before they become a "feature."

## Key concepts

**Engine choice.** Two credible candidates, mutually exclusive at the default level. `minijinja` is a Jinja-compatible runtime engine: templates live on disk, load at boot (or on change in dev), syntax is the familiar `{% %}` / `{{ }}` shape, and the admin can override built-in templates by placing a same-named file higher in the search path. `askama` is a derive-based, compile-time-checked engine: templates compile into Rust types and missing variables fail `cargo build`. The trade-off is *runtime templates* (overridable, hot-reloadable, the admin's natural fit) vs *compile-time safety* (no missing-variable surprises at runtime, but every customization is a recompile). The default recommendation is `minijinja` as the framework substrate - the admin's flexibility wins - with `askama` exposed as an option an app developer can choose for their own pages when they want the compile-time guarantee.

**Autoescape semantics.** HTML output is autoescaped by default; `{{ value }}` rendered into an HTML template emits `&lt;script&gt;` for `<script>` without the author writing anything. Opt-out is explicit and named, via `{{ value|safe }}` (Jinja shape) or a typed `Safe<T>` wrapper passed in. This implements `arch.md §4.5`'s XSS guarantee: the easy path is the safe path; unsafe output is a deliberate keystroke.

**Template inheritance.** `{% extends "base.html" %}` and `{% block content %}…{% endblock %}` work the conventional way. The admin ships a base template (`admin/base.html`) that user projects override by placing a file with the same path earlier in the search path.

**Custom tags and filters.** Filters and tests register as functions on the engine instance, not via a global registry — registration happens during `Plugin::on_ready()`, so a plugin's filters are scoped to that plugin's installation. Illustrative shape:

```rust
umbral::templates::engine_mut().add_filter("currency", |v: f64| format!("${v:.2}"));
```

**Template discovery.** Each plugin owns a `templates/` directory under its crate, and the engine search path is assembled in plugin dependency order at boot — same ordering rule the migration engine uses. The project's top-level `templates/` directory takes precedence over any plugin's, which is how user overrides of admin templates work without forking the admin.

**Ambient handle.** Rendering goes through one accessor, set in the same `OnceLock` style as the DB pool and task queue from `01-app-and-settings.md`:

```rust
let html = umbral::templates::render("admin/post_list.html", &ctx)?;
```

`ctx` is a serde-serializable value; `Result` is the umbral error enum so `?` flows through handlers without ceremony.

## Promote-to-deep trigger

Promote at M11 entry, when the admin needs concrete templates wired through real plugin overrides. Promote earlier if `umbral-email` (M9) ships templated email bodies before the admin lands — whichever consumer touches the engine first forces the design.

## Open questions

- **Final engine choice (minijinja vs askama).** The deep spec decides. Pragmatically minijinja is the front-runner for the runtime-override and admin-customization wins, but the call is open until the admin's template surface is concrete enough to benchmark both against it.
- **Template discovery model.** Per-plugin `templates/` directories assembled into one search path (the runtime-engine shape) vs a configured-list-of-paths in `Settings` (askama's shape). The two engines bias the answer differently, so this question and the engine question resolve together.
- **Admin base-template override path.** The admin ships `admin/base.html`; user projects override by placing the same path higher in the search path. Open: whether overrides can target individual blocks (`{% block branding %}`) without copying the whole base, or whether the user always copies-then-edits.
- **Custom-tag namespace collisions.** Two plugins register a filter named `currency` — does the second registration shadow, error at boot via the system check, or namespace under the plugin name? Decision lives with the deep spec.
- **i18n is out of scope.** Per PRD §14 umbral ships no `gettext` equivalent in the first iteration. The templates engine must not pretend to support i18n — no `{% trans %}` tag, no `_()` filter — so that adding it later is a clean addition rather than a redesign.

## Cross-links

- Deep specs that constrain this: `01-app-and-settings.md` (the ambient template-engine handle, same `OnceLock` pattern as the DB pool), `02-plugin-contract.md` (plugin-owned `templates/` directories, registration during `on_ready`).
- Sibling outlines: `admin.md` (the primary consumer; its rendering technology question feeds this outline), `email.md` (templated email bodies reuse this engine), `forms.md` (form-widget rendering goes through templates), `security-defaults.md` (autoescape is the XSS defence).
- `arch.md §4.5` (security autoescape note).
