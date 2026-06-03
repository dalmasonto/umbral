# Deferred items — what's left after the testBugs / playground-openapi sweep

This file captures every item from `bugs/tests/testBugs.md` and
`bugs/playground-openapi-gaps.md` that the recent sweep deliberately
did **not** ship, with the rationale + a sketch of the implementation
shape. The closed items each carry a fingerprint of what landed; the
open items each carry enough scope detail that the next person
(human or agent) can pick one and execute in a single coherent
commit.

Last updated: 2026-06-03 after IMP-3 (`6154cb0`).

## Closed in this sweep

| Tag | What landed | Commit |
|---|---|---|
| BUG-1 | Boolean POST round-trip pinned for SQLite; the original "TEXT in column" report turned out to be IMP-2's column default issue. | `bdbcc3f` |
| BUG-2 | scaffold env `"dev"` → `"Dev"` so figment-deserialised `Environment` parses. | `997e817` |
| BUG-3 | scaffold `async fn on_ready` → sync (matches `Plugin` trait). | `997e817` |
| BUG-4 | `#[umbra(index)]` emits `CREATE INDEX IF NOT EXISTS idx_<table>_<col>` on both backends. | `92f0964` |
| BUG-5 | `#[umbra(auto_now)]` / `#[umbra(auto_now_add)]` populate via `Utc::now()` on the dynamic write path. Typed path stays user-controlled. | `a6c1325` |
| BUG-9 | `#[umbra(singleton)]` flips `Model::SINGLETON` + `ModelMeta.singleton`. | `5a5b18c` |
| BUG-10 | `rust_decimal::Decimal` field type (Postgres-only, gated by the field-backend system check). | `dac7c99` |
| BUG-15 | `OneToOne` shape = `#[umbra(unique)] ForeignKey<T>`. FK render branch fixed to also emit `UNIQUE`. | `0531f5c` |
| BUG-17 | `--local <PATH>` on `startproject` / `startapp` / `startplugin` writes path-deps. | `2ad0102` |
| BUG-18 | `LoggedIn<U>` gained `Deref` / `DerefMut` / `Serialize`. | `997e817` |
| BUG-19 | scaffold templates now point at `/openapi/`. | `997e817` |
| BUG-20 | `Plugin::openapi_paths()` extension point; AuthPlugin describes its 4 routes. | `ab17067` |
| IMP-1 | `auto_migrate()` skipped when a CLI subcommand was supplied. | `2ad0102` |
| IMP-2 | SQLite bool defaults `'true'` / `'false'` → integer `1` / `0`. | `997e817` |
| IMP-3 | `#[umbra(min = N)]` / `max = N` → DDL CHECK + OpenAPI minimum/maximum + dynamic-write pre-validation. | `6154cb0` |
| IMP-4 | `startapp` writes a `src/models.rs` stub + `pub mod models;` in lib.rs. | `2ad0102` |
| IMP-5 | `#[umbra(backend = "postgres")]` field-level backend gate. | `23581cf` |
| OpenAPI #2/#3 | FK schema `$ref` (via `x-umbra-fk-ref`) + standard pagination params on list endpoints. | `2487db7` |
| OpenAPI #4 | `components.securitySchemes` block + global `security` derived from the auth chain. | `827757b` |
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

## Open — new field types

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

Default order (post-IMP-3):

1. **BUG-6 / BUG-7 / BUG-8** — struct-level attributes (`unique_together`, `indexes`, `ordering`). Share one fan-out commit since they cost the same ~60-site ModelMeta widening.
2. **BUG-11 / BUG-12 / BUG-13** — `Slug` / `Email` / `Url` wrapper types. Bundle.

The big skips (BUG-14, BUG-16, BUG-21) all want their own dedicated spec before code lands.
