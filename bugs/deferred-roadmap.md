# Deferred items — what's left after the testBugs / playground-openapi sweep

This file captures every item from `bugs/tests/testBugs.md` and
`bugs/playground-openapi-gaps.md` that the recent sweep deliberately
did **not** ship, with the rationale + a sketch of the implementation
shape. The closed items each carry a fingerprint of what landed; the
open items each carry enough scope detail that the next person
(human or agent) can pick one and execute in a single coherent
commit.

Last updated: 2026-06-03 after commits `851728a` (gap #71) through
`a45379a` (`#[umbra(example)]`).

## Closed in this sweep

| Tag | What landed | Commit |
|---|---|---|
| BUG-1 | Boolean POST round-trip pinned for SQLite; the original "TEXT in column" report turned out to be IMP-2's column default issue. | `bdbcc3f` |
| BUG-2 | scaffold env `"dev"` → `"Dev"` so figment-deserialised `Environment` parses. | `997e817` |
| BUG-3 | scaffold `async fn on_ready` → sync (matches `Plugin` trait). | `997e817` |
| BUG-4 | `#[umbra(index)]` emits `CREATE INDEX IF NOT EXISTS idx_<table>_<col>` on both backends. | `92f0964` |
| BUG-5 | `#[umbra(auto_now)]` / `#[umbra(auto_now_add)]` populate via `Utc::now()` on the dynamic write path. Typed path stays user-controlled. | `a6c1325` |
| BUG-15 | `OneToOne` shape = `#[umbra(unique)] ForeignKey<T>`. FK render branch fixed to also emit `UNIQUE`. | `0531f5c` |
| BUG-17 | `--local <PATH>` on `startproject` / `startapp` / `startplugin` writes path-deps. | `2ad0102` |
| BUG-18 | `LoggedIn<U>` gained `Deref` / `DerefMut` / `Serialize`. | `997e817` |
| BUG-19 | scaffold templates now point at `/openapi/`. | `997e817` |
| IMP-1 | `auto_migrate()` skipped when a CLI subcommand was supplied. | `2ad0102` |
| IMP-2 | SQLite bool defaults `'true'` / `'false'` → integer `1` / `0`. | `997e817` |
| IMP-4 | `startapp` writes a `src/models.rs` stub + `pub mod models;` in lib.rs. | `2ad0102` |
| Playground-openapi #5 | `#[umbra(help = "...")]` → OpenAPI `description`. | `eb27811` |
| Playground-openapi #6 | `#[umbra(example = "...")]` → OpenAPI `example`. | `a45379a` |

Plus gap #71 (playground app-scoping, `851728a`) and gap #65 follow-up (full diff widening, `f85ed06`).

## Open — model / field attributes

### BUG-6: `#[model(unique_together = [...])]` — composite UNIQUE

Single-column unique already works (`#[umbra(unique)]`). The
struct-level shape is what's missing.

**Shape:**

```rust
#[derive(Model)]
#[umbra(unique_together = [["tenant_id", "slug"], ["author_id", "year"]])]
pub struct Post { ... }
```

**Implementation sketch:**
- Add `ModelMeta.unique_together: Vec<Vec<String>>` (new field on the snapshot).
- Parse the struct-level attribute in `parse_umbra_struct_attr`.
- DDL: `CONSTRAINT "<table>_<col1>_<col2>_key" UNIQUE ("<col1>", "<col2>")` on Postgres; SQLite supports the same `UNIQUE (col1, col2)` inline. Emit alongside the column defs in `render_operation_*::CreateTable`.
- Diff: any change to the list emits an `AlterTable` op. Could land later — v1 just supports CreateTable.
- Tests: round-trip the unique_together list through the snapshot, assert the DDL contains the constraint, drive a duplicate insert through SQLite and assert it fails.

Effort: ~250 LOC across model.rs, migrate.rs, the macro, plus 60-something hand-written ModelMeta sites that gain `unique_together: vec![]`. Half a day.

### BUG-7: `#[model(indexes = [["a", "b"]])]` — multi-column indexes

Single-column already covered by `#[umbra(index)]` from BUG-4 above.

**Shape:**

```rust
#[derive(Model)]
#[umbra(indexes = [["tenant_id", "created_at"], ["status"]])]
pub struct Post { ... }
```

**Implementation sketch:** same pattern as `unique_together` — add `ModelMeta.indexes: Vec<Vec<String>>`, parse, emit `CREATE INDEX IF NOT EXISTS idx_<table>_<col1>_<col2>` after `CREATE TABLE`. Diff widening can come later.

Effort: same as #6, ~300 LOC. Could share a struct-level-attribute helper with #6 — bundle the two into one commit if feasible.

### BUG-8: `#[model(ordering = ["-published_at", "id"])]` — default ordering

**Shape:**

```rust
#[derive(Model)]
#[umbra(ordering = ["-published_at", "id"])]
pub struct Post { ... }

// Now this works:
Post::objects().fetch().await?;  // ORDER BY published_at DESC, id ASC implied
```

**Implementation sketch:**
- `ModelMeta.ordering: Vec<(String, OrderDir)>`.
- Macro parses the strings; `-prefix` → DESC.
- `QuerySet::fetch()` checks if any `.order_by(...)` was explicitly added; if not, walks `T::ORDERING` and applies the defaults.
- Same metadata feeds the admin list view's default sort.

Effort: ~200 LOC. The runtime hook is small once the metadata is there.

### BUG-9: `#[model(singleton)]` — single-row admin marker

Used for "site settings" tables that hold exactly one row. Django shipped `django-solo` as a community plugin for this; umbra would bake it into the admin.

**Shape:**

```rust
#[derive(Model)]
#[umbra(singleton)]
pub struct SiteSettings {
    pub id: i64,
    pub title: String,
    pub maintenance_mode: bool,
}
```

**Effect:**
- Admin list view auto-redirects to the (single) row's edit page.
- Admin doesn't show a "+ New" button.
- `Plugin::on_ready()` ensures the row exists (insert with the column defaults).

**Implementation sketch:** add `ModelMeta.singleton: bool`. Admin reads it in the list-view router and the form-builder. Default-row seeding lives in a small helper users can call from their `Plugin::on_ready`. Effort: ~150 LOC.

### IMP-3: `#[umbra(min = N)]` / `#[umbra(max = N)]` — numeric validators

**Shape:**

```rust
pub struct Review {
    pub id: i64,
    #[umbra(min = 1, max = 5)]
    pub rating: i32,
}
```

**Effect:**
- DDL emits a `CHECK (rating >= 1 AND rating <= 5)` constraint.
- OpenAPI emits `minimum: 1, maximum: 5` on the property schema.
- Admin form adds HTML5 `min` / `max` attributes.
- REST plugin's dynamic write path pre-validates and returns a structured 400.

**Implementation sketch:** add `FieldSpec.min: Option<i64>` and `max: Option<i64>` (also support `f64` for floats — bigger metadata). DDL emission similar to gap #65 unique branch. The CHECK constraint name `<table>_<col>_range_check` follows the same convention as `<col>_check` for choices. Effort: ~250 LOC.

### IMP-5: `#[umbra(backend = "postgres")]` — field-level backend gate

A field type only available on Postgres (`Array<T>`, `Inet`, `Cidr`, JSONB operators) should fail at boot when the app's connected backend is SQLite, not at query time.

**Shape:**

```rust
pub struct Server {
    pub id: i64,
    #[umbra(backend = "postgres")]
    pub allowed_cidrs: Vec<ipnetwork::IpNetwork>,
}
```

The framework's `App::build()` already has a system-check hook (mentioned in `arch.md` §4). Plumb the per-field backend gate through it — at boot, walk every registered model, every field whose `supported_backends` is non-empty must include the active backend.

**Implementation sketch:** the `supported_backends: &'static [&'static str]` field on `FieldSpec` already exists. The macro just doesn't accept the `backend` attribute yet. Wire it in `parse_umbra_field_attr` + emit the matching slice in the FieldSpec const + add a system-check that walks models and fails the boot with a clear message. Effort: ~150 LOC.

## Open — new field types

### BUG-10: `Decimal` field type (highest priority for money columns)

Money/price columns need `rust_decimal::Decimal`. Today users fall back to `String` or `f64`, both bad.

**Shape:**

```rust
pub struct Product {
    pub id: i64,
    pub price: rust_decimal::Decimal,
}
```

**Implementation sketch:**
- Add `SqlType::Decimal { precision: u8, scale: u8 }` (or carry precision/scale in `FieldSpec` separately so the variant stays `Copy`).
- DDL: `NUMERIC(precision, scale)` on both backends.
- sqlx `Decimal` support is already in the `sqlx` feature list — verify with the `rust_decimal` feature on.
- Field type classifier recognises `rust_decimal::Decimal` and `decimal::Decimal`.
- `#[umbra(precision = 10, scale = 2)]` attribute attaches the (precision, scale) pair.

Effort: ~400 LOC. The precision/scale handling adds a notch of complexity to the snapshot.

### BUG-11 / BUG-12 / BUG-13: `Slug` / `Email` / `Url` — text + validation

All three are `String` with type-level guarantees and validation. Each could be a separate wrapper type with `Deref` to `str` + a `validate()` method.

**Implementation sketch:**
- New crate `umbra-validators` (or fold into umbra-core's orm module) exporting `Slug(String)`, `Email(String)`, `Url(String)`.
- Each provides `pub fn new(s: impl Into<String>) -> Result<Self, ValidationError>` and `pub fn unchecked(s: String) -> Self`.
- Macro classifies the type as `SqlType::Text` with a marker for the admin form to render the right widget (HTML5 `type="email"` / `type="url"` for the latter two; slug regex hint for the first).
- REST plugin's dynamic path runs `Validator::validate` on insert/update.

Effort: low per type (~150 LOC each), but the three together carry a lot of boilerplate. Bundle as one commit.

### BUG-14: `ImageField` / `FileField`

Couples to file storage. The `umbra-media` plugin already exists; this gap is about the field type that pairs with it. **Defer** until a concrete media-aware app drives the requirements (single-bucket vs multi-tenant, signed URLs, image variants).

### BUG-16: `ManyToMany<T>` — big design

Wants implicit join-table generation, reverse accessors (`user.posts`), `prefetch_related`. Each is its own substantial design. The current manual-join-table pattern (`PostTag { post_id: FK<Post>, tag_id: FK<Tag> }`) works fine — explicit is clearer for a v1 framework. **Defer to its own dedicated spec.**

## Open — OpenAPI spec emission

### Playground-openapi-gaps #2: FK columns → schema `$ref`

Today FK columns emit `{ type: integer, format: int64, x-umbra-fk-target: "user" }`. The standard OpenAPI idiom is `{ $ref: "#/components/schemas/User/properties/id" }`. Generated clients (openapi-generator, orval) can then navigate from a `Post` to its `User`.

**Implementation sketch:** `column_schema()` already gets the column; need to look up the target schema name from the registry. The `build_spec` walker already computes `(table → schema_name)` mappings. Pass that map through to `column_schema`. Effort: ~80 LOC.

### Playground-openapi-gaps #3: pagination parameters

The REST plugin paginates list responses (`{count, results: [...]}`); the OpenAPI spec just doesn't say so. Once the convention is settled — `?page` / `?page_size`, `?limit` / `?offset`, or both — emit them as `parameters` on every list operation.

**Implementation sketch:** add a `paginated: bool` to whatever the OpenAPI plugin reads from RestPlugin (or a new `RestPlugin::pagination_style()`). For each list operation, push two `parameters` entries (one each for `page` and `page_size`, or `limit` / `offset`). Effort: ~100 LOC.

### Playground-openapi-gaps #4: `securitySchemes`

`AuthPlugin` registers cookie + bearer + session backends today. The OpenAPI spec is silent — Swagger UI's "Authorize" button doesn't appear; generated clients can't generate auth-aware code.

**Implementation sketch:** RestPlugin exposes which `Authentication` classes are wired. The OpenAPI plugin's `build_spec` reads that list and emits matching `securitySchemes`:
- `BearerAuthentication` → `{ type: "http", scheme: "bearer" }`
- `SessionAuthentication` → `{ type: "apiKey", in: "cookie", name: "umbra_session" }`
- `ChainAuthentication([Session, Bearer])` → both, with `security: [{ session: [] }, { bearer: [] }]` at the operation level so clients can pick either.

The challenge is RestPlugin doesn't currently expose the chain shape — just `Arc<dyn Authentication>`. The `Authentication` trait would need a `security_scheme() -> Option<openapi::SecurityScheme>` method, or a downcast for the built-in classes. Effort: ~300 LOC + a trait method addition.

## Open — Plugin trait extension

### BUG-20: OpenAPI plugin doesn't see `Plugin::routes()`

The OpenAPI spec describes auto-CRUD routes (one per model) but ignores routes contributed by plugins via `Plugin::routes()` — including the four `/api/auth/*` routes from `AuthPlugin::with_default_routes()`.

**Implementation sketch:** add `Plugin::openapi_paths(&self) -> Vec<openapi::PathItem>` to the Plugin trait with a default `Vec::new()` impl. Plugins that want their routes documented override it. `OpenApiPlugin::build_spec` walks every registered plugin and merges the contributions. AuthPlugin's impl returns the four `/api/auth/*` paths with their request/response shapes.

Effort: ~400 LOC. Most of it is in AuthPlugin describing its DTOs as schemas. The trait extension itself is one method.

## Open — Admin

### BUG-21: Rich FK / M2M / 1to1 pickers + `Manager::admin_search()`

Phased multi-week work. Today the admin has a functional async-combobox for FK columns; M2M and 1to1 are unmodelled. The phased plan in `bugs/tests/testBugs.md` is the right shape:

- Phase 1 — ORM modeling: needs BUG-16 (ManyToMany) first.
- Phase 2 — form widgets: shadcn Select, true M2M chip picker, 1to1 reuse-FK-with-uniqueness-hint.
- Phase 3 — `Manager::admin_search(query)` + `Manager::admin_display_list()`.

**Defer** until BUG-16 lands. The admin work hangs off it.

## Open — Playground frontend

### Items 7–12 (history replay, schema navigation, value pickers, per-record delete, history cap, import/export)

Each is real frontend work. None block the framework's correctness or usability for the common case. **Defer** as a frontend-track punch-list — the file at `bugs/playground-openapi-gaps.md` lines 30-40 already captures the shape of each.

## How to pick a next item

Default order:

1. **OpenAPI gaps #4 (securitySchemes)** — biggest UX win per LOC for the playground. Swagger's "Authorize" button is the visible payoff.
2. **BUG-20 (Plugin::openapi_paths)** — composes with #4 to make the spec actually complete.
3. **BUG-10 (Decimal)** — every "real" app with money columns needs this; no good workaround.
4. **BUG-6 / BUG-7 / BUG-8** — bundle as one struct-level-attributes commit.
5. **IMP-5 (backend gate)** — small, catches Postgres-only models on SQLite at boot.

The big skips (BUG-14, BUG-16, BUG-21) all want their own dedicated spec before code lands.
