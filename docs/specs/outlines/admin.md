# Outline — Admin (umbra-admin)

| | |
|---|---|
| **Status** | Outline. Promotes to a deep spec at M11 entry. |
| **Maps to milestone** | M11 |
| **Companions** | `arch.md §6.3`, `02-plugin-contract.md`, `04-orm-model-and-fields.md`, outlines `auth-and-sessions.md`, `templates.md`, `forms.md`, `static-and-media.md` |

## Purpose

`umbra-admin` is the flagship "wow" plugin — register a `Model`, get a paginated, filterable, searchable CRUD UI for free. It is the umbra equivalent of `django.contrib.admin`: the feature most existing Django users will reach for first to confirm the framework feels right. Structurally it is an ordinary plugin built on the contract in `02-plugin-contract.md` — it contributes routes (the admin URL tree), middleware (an auth gate), migrations (an `admin_log` table for audit history), commands (`createsuperuser` lives here in spirit even if it's exported as an auth-plugin command), and an `on_ready` hook that seals the model registry so registrations done from other plugins are visible at request time. The contract has no admin-shaped escape hatch; if `umbra-admin` needs something the `Plugin` trait cannot express, that is a signal the trait is wrong.

## Key concepts

### Model registration

Models opt in through `admin.register(...)`. The configuration is a struct with sensible defaults so a one-line registration produces a usable UI; the same struct is the customisation surface for everything below.

```rust
admin.register::<Post>(PostAdmin {
    list_display:    &[post::title, post::author, post::published_at],
    list_filter:     &[post::published_at, post::author],
    search_fields:   &[post::title, post::body],
    ordering:        &[post::published_at.desc()],
    inlines:         &[CommentInline::new()],
    actions:         &[publish_selected, unpublish_selected],
    ..Default::default()
});
```

Field references are the sibling-module column constants from `04-orm-model-and-fields.md`, so a typo fails to compile and a renamed field forces the admin config to update with it.

### list_display, list_filter, search_fields, ordering

`list_display` is the column set for the changelist table; FK columns render as links to the related object's detail page. `list_filter` produces faceted sidebar filters — distinct values for low-cardinality columns, date-bucket pickers for datetimes, choice values for `#[umbra(choices)]` fields. `search_fields` drives a single search box translated into `LIKE` (or `ILIKE` on Postgres) across the listed columns. `ordering` mirrors the `Meta` attribute and is overridable per-column from the table header.

### Inlines

`TabularInline` and `StackedInline` render related FK rows inline on the parent's detail page — comments under a post, line items under an order. An inline declares the child model, the FK back to the parent, and the same display/customisation surface as a top-level `ModelAdmin`. Edits to the parent and its inlines flow through one form-submit cycle; the deep spec resolves whether the underlying save is one transaction or sequenced ones (likely one, per `03-orm-querysets.md` transaction semantics).

### Bulk actions, fieldsets, readonly fields

Bulk actions are functions over a `QuerySet<T>` exposed in the changelist's action dropdown (`publish_selected(qs).await?`). Fieldsets group fields in the detail form with section headings and collapsible blocks. `readonly_fields` renders fields un-editably and excludes them from the form-binding path — useful for `created_at`, computed properties, or fields the current user lacks permission to write.

### Permission integration

Permission gating leans on `umbra-auth`'s permission model: `add_<model>`, `change_<model>`, `delete_<model>`, `view_<model>` are auto-created per registered model, mirroring Django's contenttypes-derived defaults. The admin middleware short-circuits unauthenticated requests to a login view; per-page guards check the relevant permission against `Auth<User>`. Custom user models flow through unchanged — the admin sees `Auth<User>` from the prelude and never names a concrete user type.

### Rendering substrate

Lean toward server-rendered minijinja templates (the substrate owned by outline `templates.md`), reused by any plugin that needs HTML. The admin ships a small base template set (changelist, detail, login) that downstream apps can override the way Django's `templates/admin/` override path works. An embedded SPA is the alternative and stays on the table for the deep spec — it would buy a richer client-side filter UX at the cost of a JS build pipeline `umbra-admin` would otherwise not need. **The deep spec at M11 entry decides; this outline records the lean.**

### Customisation hooks

Per-`ModelAdmin` overrides for `get_queryset(auth)` (row-level filtering by user), `save_model(auth, form, instance)` (audit-stamp on save), `has_change_permission(auth, instance)` (object-level permissions), and a template-override path for replacing any rendered block. These hooks make the admin extensible without forking it.

## Promote-to-deep trigger

Promote at **M11 entry**, when the milestone begins. The promotion also resolves spec-set-design open question #3 — the deep spec is the place where server-rendered-templates vs embedded-SPA gets pinned, because that decision constrains the dependency footprint of the entire built-in plugin set.

## Open questions

- **Rendering technology choice.** Server-rendered minijinja (lean) vs embedded SPA. Server templates reuse the substrate that the email and forms plugins already need; an SPA buys richer client-side interactivity at the cost of a JS build. Decided in the deep spec.
- **Custom user model in the permission model.** `umbra-auth` owns the swap mechanism (outline open question #5); the admin needs to know how to look up "the change_post permission for *this* user type" without naming the concrete user struct. Likely resolves via a trait the swapped user type implements.
- **File and image field rendering.** `FileField` / `ImageField` in the detail form need an upload widget, thumbnail preview (for images), and a "clear" affordance. The storage half lives in `static-and-media.md`; the admin owns the widget half. Open until both specs land at M11.
- **Action result UX.** Bulk actions can succeed partially (publish 7 of 10), fail with per-row errors, or need confirmation before running on a large selection. How the changelist surfaces these — flash messages, a result page, an audit log entry — is open.
- **Admin-log audit table.** Django records every add/change/delete in `django_admin_log`. Whether `umbra-admin` owns the same table by default (and whether it is queryable through the admin UI itself) decides at deep-spec time.

## Cross-links

- Deep specs that constrain this: `02-plugin-contract.md` (the admin is structurally an ordinary plugin — routes, migrations, middleware, `on_ready`), `04-orm-model-and-fields.md` (the column-constant references in `list_display` / `search_fields` / `ordering`; the field metadata that drives form rendering).
- Sibling outlines: `auth-and-sessions.md` (the permission and custom-user-model machinery the admin gates on), `templates.md` (the rendering substrate — minijinja, autoescape, override path), `forms.md` (admin detail forms reuse the form pipeline — `ModelForm` analog, validators, widgets), `static-and-media.md` (the admin's bundled CSS/JS assets and the storage half of file-field upload widgets).
- `arch.md §6.3` — the one-line built-in-plugin description this outline expands.
