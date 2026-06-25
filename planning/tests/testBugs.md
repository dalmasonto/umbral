# Test Bugs found on building the `shop` example app

Date: 2026-06-03
Tester: Claude (automated build of examples/shop)

## Summary

Built a full e-commerce + content example app (`examples/shop`) using:
- `umbral startproject shop`
- `umbral startapp content` (all content models: Category, Tag, Post, Comment, Page, Faq, Menu, MenuItem, Banner, Testimonial, ContactMessage, Subscriber, MediaAsset, Redirect, SiteSetting)
- `umbral startapp ecommerce` (e-commerce models: Brand, Product, ProductImage, ProductVariant, Customer, Address, Order, OrderItem, Payment, Shipment, Coupon, Review + join tables for M2M)

Wired plugins: auth, sessions, admin, rest, openapi, content, ecommerce.
Ran makemigrations + migrate successfully on SQLite.
REST endpoints work for list/retrieve/create.

---

## Critical Bugs (break runtime / compile)

### BUG-1: REST plugin stores JSON boolean as TEXT in SQLite, causing decode failure on SELECT
**Where:** `plugins/umbral-rest` — POST to `/api/product/` with `"is_featured": false`
**What:** The REST handler inserts `"false"` (TEXT) into a SQLite `boolean` column. On subsequent SELECT, sqlx errors: "Rust type `bool` (as SQL type `BOOLEAN`) is not compatible with SQL type `TEXT`". This breaks list/retrieve endpoints as soon as any row has a boolean field inserted via REST.
**Workaround:** Send `0` / `1` (integer) instead of `true` / `false` in JSON POST body.
**Fix suggestion:** In the REST plugin's INSERT builder, convert JSON bool values to integer 0/1 for SQLite backend before binding to sqlx.

### BUG-2: `startproject` scaffold generates `environment = "dev"` but settings parser expects `"Dev"`
**Where:** `crates/umbral-cli/src/scaffold.rs` line 476
**What:** The generated `umbral.toml` has `environment = "dev"` which causes `Settings::from_env()` to fail with: `UnknownVariant("dev", ["Dev", "Test", "Prod"])`.
**Workaround:** Manually change to `environment = "Dev"`.
**Fix suggestion:** Update scaffold.rs to emit `environment = "Dev"`.
**Personal Take:** Use lowercase env var names.

### BUG-3: `startplugin` scaffold generates `async fn on_ready` but `Plugin` trait requires sync `fn on_ready`
**Where:** `crates/umbral-cli/src/scaffold.rs` in `scaffold_plugin()`
**What:** The richer scaffold template wraps `on_ready` in `#[async_trait]` but `Plugin::on_ready` is defined as `fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError>` (sync). Compiling a freshly generated plugin fails with lifetime mismatch error.
**Workaround:** Remove `#[async_trait]` and `async` keyword from generated `on_ready`.
**Fix suggestion:** Fix scaffold_plugin template to match the sync trait signature.
**Personal Take:** If on_ready was to be async, is there any issue? If yes, remove else, you can add it back so that devs can do async stuff here.

---

## ORM / Macro Missing Features (compile-time rejections)

### BUG-4: `#[umbral(index)]` attribute not supported by `#[derive(Model)]`
**Where:** `crates/umbral-macros/src/lib.rs` — field-level attribute parser
**What:** The spec documents `index` for column-level indexes, but the macro rejects it with: "unknown field-level umbral attribute `index`".
**Workaround:** Omit `index` attribute; indexes can be added manually via raw SQL.
**Fix suggestion:** Add `index` to `UmbralFieldAttr` and emit `CREATE INDEX` in migration renderer.

### BUG-5: `#[umbral(auto_now_add)]` and `#[umbral(auto_now)]` not supported
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** These Django-style timestamp auto-populate attributes are documented in the spec but rejected by the macro.
**Workaround:** Use plain `DateTime<Utc>` fields and set values manually in code.
**Fix suggestion:** Add these attributes to the macro and have the ORM write layer auto-populate them on `create` / `update`.

### BUG-6: `#[model(unique_together = [...])]` not supported
**Where:** `crates/umbral-macros/src/lib.rs` — struct-level attribute parser
**What:** Composite unique constraints are documented but not implemented.
**Workaround:** Add `UNIQUE` constraints manually via raw SQL after migration.
**Fix suggestion:** Add struct-level `unique_together` parsing and emit composite `UNIQUE` in DDL renderer.

### BUG-7: `#[model(indexes = [...])]` not supported
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** Model-level explicit index lists are not parsed.
**Workaround:** Same as BUG-4.
**Fix suggestion:** Add `indexes` to struct-level attribute parsing.

### BUG-8: `#[model(ordering = [...])]` not supported
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** Default ordering meta attribute is not parsed.
**Workaround:** Order explicitly in QuerySet chains.
**Fix suggestion:** Add `ordering` to struct-level attribute parsing and use as default in `QuerySet::fetch`.

### BUG-9: `#[model(singleton)]` not supported
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** Singleton model marker is not parsed.
**Workaround:** Use normal model and enforce single-row logic in application code.
**Fix suggestion:** Add `singleton` parsing; admin should skip list view and go straight to edit.

---

## Missing Field Types (compile-time rejections)

### BUG-10: `Decimal` type not supported by derive macro
**Where:** `crates/umbral-macros/src/lib.rs` — `classify_field_type()`
**What:** Money/price fields cannot use `rust_decimal::Decimal`. Spec uses it extensively.
**Workaround:** Use `String` (or `f64` for simple cases) and do formatting in application code.
**Fix suggestion:** Add `Decimal` to the type catalogue, mapping to `NUMERIC` / `DECIMAL` in DDL.

### BUG-11: `Slug` type not supported
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** Dedicated slug field type with auto-normalization is not available.
**Workaround:** Use `String` with manual validation.
**Fix suggestion:** Add a `Slug` wrapper type or derive.

### BUG-12: `Email` type not supported
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** Dedicated email field type with validation is not available.
**Workaround:** Use `String` with manual validation.
**Fix suggestion:** Add `Email` wrapper type (or at least a validator trait).

### BUG-13: `Url` type not supported
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** Dedicated URL field type is not available.
**Workaround:** Use `String` with manual validation.
**Fix suggestion:** Add `Url` wrapper type.

### BUG-14: `ImageField` and `FileField` not supported
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** File upload fields are not available.
**Workaround:** Use `String` for file path/URL.
**Fix suggestion:** Add `ImageField` and `FileField` types that integrate with `umbral-media`.

### BUG-15: `OneToOne<T>` not supported
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** One-to-one relationships have no dedicated type. At DB level it's same as FK, but ORM could enforce uniqueness.
**Workaround:** Use `ForeignKey<T>`.
**Fix suggestion:** Add `OneToOne<T>` wrapper around `ForeignKey<T>` with uniqueness enforcement.

### BUG-16: `ManyToMany<T>` not supported
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** Many-to-many fields are not supported by the derive macro.
**Workaround:** Create explicit join models with two `ForeignKey` fields (e.g., `ProductTag`, `CouponProduct`).
**Fix suggestion:** Add `ManyToMany<T>` support that auto-generates join table operations in migrations.

---

## Plugin / Scaffold Issues

### BUG-17: `startapp` / `startplugin` scaffold uses git dependencies instead of local paths
**Where:** `crates/umbral-cli/src/scaffold.rs`
**What:** Generated plugin Cargo.toml depends on `umbral = { git = "..." }`, which doesn't work when developing locally in the same repo.
**Workaround:** Manually change to `path = "../../../crates/umbral"`.
**Fix suggestion:** Add `--local` flag to `startproject` / `startapp` / `startplugin` that emits path dependencies relative to the repo root. Or detect if running inside the umbral repo.

### BUG-18: `LoggedIn<U>` does not implement `Serialize`
**Where:** `plugins/umbral-auth/src/login_required.rs`
**What:** `LoggedIn<U>` is a tuple struct wrapping `U`. It has no `Deref` impl and no `Serialize` impl, so it cannot be passed directly to template contexts via `context!(user)`.
**Workaround:** Extract fields manually: `let username = user.0.username().to_string();`
**Fix suggestion:** Either add `Deref` to `LoggedIn<U>` or add a `Serialize` impl that delegates to the inner `U`.

---

## Scaffold / Documentation Issues

### BUG-19: Scaffolded `main.rs` says `/api/docs/` but `OpenApiPlugin` mounts at `/openapi/`
**Where:** `crates/umbral-cli/src/scaffold.rs` — generated `main.rs` comments
**What:** The scaffold template prints a comment `OpenApiPlugin: Swagger UI at /api/docs/` in the generated `src/main.rs`, but `OpenApiPlugin::new()` actually mounts the Swagger UI at `/openapi/` (and `/openapi/openapi.json`) by default. The `.at("/api/docs")` builder method exists for users who *want* to override the mount point, but the scaffold doesn't call it. This causes a 404 when a new user follows the scaffold comments.
**Workaround:** Visit `/openapi/` instead, or call `.at("/api/docs")` when constructing `OpenApiPlugin::new()`.
**Fix suggestion:** Update the scaffold comment to say `/openapi/` instead of `/api/docs/`.

---

### BUG-20: `OpenApiPlugin` only documents model tables, not custom plugin routes (e.g., auth endpoints)
**Where:** `plugins/umbral-openapi/src/lib.rs` — `build_spec()`
**What:** The spec generator walks `umbral::migrate::registered_plugins()` and `models_for_plugin()` to emit one path per model table (`/api/{table}/`, `/api/{table}/{id}`). Custom routes contributed by plugins via `Plugin::routes()` — such as `/api/auth/register`, `/api/auth/login`, `/api/auth/logout`, `/api/auth/me` from `AuthPlugin::with_default_routes()` — are invisible to the spec generator. They don't appear in the Swagger UI or the `/openapi/openapi.json` spec.
**Workaround:** None. Custom routes are undocumented in the OpenAPI spec.
**Fix suggestion:** Extend `build_spec()` to also walk `Plugin::route_paths()` (or add a new `Plugin::openapi_paths()` hook) and merge custom route documentation into the spec. Alternatively, `AuthPlugin` could register its DTOs as pseudo-models so the generator picks them up.

---

## Improvements / Suggestions

### IMP-1: `auto_migrate()` in scaffolded `main.rs` interferes with manual `makemigrations` CLI
**Where:** `crates/umbral-cli/src/scaffold.rs` — generated `main.rs`
**What:** Because `auto_migrate()` runs on every boot before CLI dispatch, running `cargo run -- makemigrations` actually triggers `make()` + `migrate()` first, then the CLI command sees "no changes detected". This is confusing for developers who expect the CLI to be the sole driver.
**Suggestion:** Either remove `auto_migrate()` from the scaffold or make it conditional on the absence of CLI args.

### IMP-2: `#[umbral(default = "true")]` on bool stores string literal `true` as TEXT default in SQLite
**Where:** Migration engine — SQLite DDL renderer
**What:** For a boolean column, `default = "true"` renders as `DEFAULT 'true'` in SQLite, but SQLite boolean semantics expect `1` / `0`. While this doesn't break compilation, it's inconsistent and may confuse ORM consumers.
**Suggestion:** The SQLite renderer should translate bool defaults to integer `1` / `0`.

### IMP-3: `#[umbral(min = N)]` / `#[umbral(max = N)]` validators not supported
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** Min/max validators (e.g., `Review.rating` should be 1..=5) are documented in spec but not implemented.
**Suggestion:** Add these to the macro and wire into the Form / REST validation layers.

### IMP-4: `startapp` scaffold doesn't include `models.rs` or `handlers.rs`
**Where:** `crates/umbral-cli/src/scaffold.rs`
**What:** `startapp` generates only `lib.rs` with a stub Plugin. `startplugin` generates a richer template with `models.rs` + `handlers.rs`. Most users will want `startapp` to also generate `models.rs`.
**Suggestion:** Either make `startapp` generate `models.rs` too, or document when to use `startplugin` vs `startapp` more clearly.

### IMP-5: No `#[umbral(backend = "postgres")]` field-level gate
**Where:** `crates/umbral-macros/src/lib.rs`
**What:** The spec documents `backend = "postgres"` for ArrayField so the boot system check can fail early on SQLite. Not implemented.
**Suggestion:** Add backend gating to the macro and system check framework.

---

### BUG-21: Admin form editor: FK/M2M/1to1 relation fields lack rich selection UI; Choice fields use plain `<select>` instead of shadcn

**Where:** `plugins/umbral-admin` — `view.rs`, `field_editor.html`, `fk_picker.rs`, ORM `#[derive(Model)]`

**What is already there (FK only):**
- `fk_picker.rs` implements `GET /admin/api/{table}/{field}/options` (paginated async search) and `/options/resolve` (label lookup for pre-selected IDs) — this powers a working HTMX async combobox for `SqlType::ForeignKey` fields.
- The `field_editor.html` template renders FK fields with a hidden input + searchable text input + HTMX dropdown. Edit forms correctly resolve the FK's current value label on load.

**What is missing / broken:**

1. **M2M fields are not modeled by the ORM, so the admin has nothing to render.**
   - `#[derive(Model)]` does not support `ManyToMany<T>`, so no model ever produces a field with `kind == "m2m"`.
   - The template has a stub `m2m` branch (lines 188–205 of `field_editor.html`) but it has no `hx-get` endpoint wired, and the M2M picker JS is non-functional because there's no data source.
   - When users create manual join tables (e.g., `ProductTag { product: FK<Product>, tag: FK<Tag> }`), the admin treats them as ordinary models, not as M2M bridges. There is no UI to attach/detach tags from a Product edit form.

2. **OneToOne fields are not modeled by the ORM.**
   - `#[derive(Model)]` does not support `OneToOne<T>`. At the DB level it's identical to FK, but semantically it should be a unique FK. The admin has no way to enforce "only one" in the form UI, and there's no dedicated picker type for it.

3. **Choice fields render as a plain HTML `<select>`, not a shadcn Select component.**
   - Currently `field.kind == "select"` emits a standard `<select><option>...</option></select>` in `field_editor.html`.
   - The user wants a rich shadcn-style Select (searchable dropdown, keyboard nav, virtualised for long lists). The admin's Material Design 3 theme is already shadcn-based, so the component exists but isn't wired into the form editor.

4. **The ORM lacks a dedicated `admin_search` abstraction for relation lookups.**
   - `fk_picker.rs` uses `DynQuerySet::search()` + `fetch_as_strings()` with manually-picked label/PK columns. This works but is ad-hoc.
   - The user wants a first-class ORM method — something like `Model::objects().admin_search(query).fetch_for_admin()` — that:
     - searches across configurable fields (text, numbers)
     - returns `(pk, display_label)` pairs
     - uses the model's `#[umbral(string)]` field (or first non-PK text column) as the display label
     - respects the backend dialect (SQLite vs Postgres)
   - This abstraction would then power FK, M2M, and any admin autocomplete uniformly.

**Django comparison:**
- Django admin renders FKs with a `<select>` (or `raw_id_fields` popup search). For large tables it uses `autocomplete_fields` with a search-powered AJAX dropdown.
- Django renders M2M with a dual multi-select widget (left = available, right = selected) or a filter_horizontal/filter_vertical widget.
- Django's `Model.__str__()` is the display label for FK/M2M options. Umbral's equivalent is `#[umbral(string)]` on a field, but there's no uniform "get display string for this row" API.

**Workaround:**
- For FK: the existing async combobox works for moderate tables. For large tables the current `page_size=20` search is functional but not as rich as a shadcn Select.
- For M2M: none. Users must edit the join table directly as a standalone model.
- For 1to1: use `ForeignKey<T>` and enforce uniqueness via application code.
- For choices: live with the plain `<select>`.

**Fix suggestion (phased):**

*Phase 1 — ORM modeling:*
- Add `OneToOne<T>` to the derive macro type catalogue. At DDL level emit FK + UNIQUE constraint.
- Add `ManyToMany<T>` to the derive macro. At DDL level emit an implicit join table (`{model}_{field}_{target}`). The join table should be transparent to the user — they write `tags: ManyToMany<Tag>` and the framework handles the bridge.

*Phase 2 — Admin form widgets:*
- Replace the plain `<select>` for `kind == "select"` with a shadcn Select component (HTMX-backed or pure JS) that supports search/filter.
- Wire the existing `fk_picker.rs` endpoints into a shadcn Select-style widget for FK fields (search input inside the dropdown, infinite scroll pagination).
- Implement a true M2M chip picker in the form editor:
  - A search input that calls `/admin/api/{fk_table}/{field}/options`
  - Selected items render as removable chips
  - The hidden input stores comma-separated IDs
  - On form submission, the admin handler transparently creates/deletes join-table rows
- For OneToOne: reuse the FK picker widget but add a uniqueness hint in the UI.

*Phase 3 — Unified ORM admin search:*
- Add `Manager<T>::admin_search(query: &str) -> QuerySet<T>` that searches across `Model::ADMIN_SEARCH_FIELDS` (defaulting to all text + numeric columns).
- Add `Manager<T>::admin_display_list() -> Vec<(T::PrimaryKey, String)>` that returns PK + display label, using `#[umbral(string)]` or first non-PK text column.
- Make `fk_picker.rs` use these methods instead of raw `DynQuerySet` + manual column picking.

---

## Verified Working ✅

| Feature | Status |
|---|---|
| `startproject` scaffold | ✅ (with workaround for env case) |
| `startapp` scaffold | ✅ (with workaround for git→path deps) |
| `#[derive(Model)]` on basic types | ✅ |
| `ForeignKey<T>` (nullable + non-nullable) | ✅ |
| `#[derive(Choices)]` enums | ✅ |
| `#[umbral(unique)]` | ✅ |
| `#[umbral(default = "...")]` | ✅ |
| `#[umbral(max_length = N)]` | ✅ |
| `#[umbral(on_delete = "...")]` | ✅ |
| `#[umbral(choices)]` | ✅ |
| Migrations: `CreateTable` | ✅ |
| Migrations: `AddColumn` | ✅ |
| Migrations: cross-plugin FK ordering | ✅ (via `Plugin::dependencies()`) |
| REST: list/retrieve/create | ✅ (with BUG-1 workaround) |
| REST: query-string filtering | ✅ |
| Admin panel redirect to login | ✅ |
| `showmigrations` | ✅ |
| `migrate` | ✅ |
