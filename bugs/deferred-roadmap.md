# Deferred items — what's left after the testBugs / playground-openapi sweep

This file captures every item from `bugs/tests/testBugs.md` and
`bugs/playground-openapi-gaps.md` that the recent sweep deliberately
did **not** ship, with the rationale + a sketch of the implementation
shape. The closed items each carry a fingerprint of what landed; the
open items each carry enough scope detail that the next person
(human or agent) can pick one and execute in a single coherent
commit.

Last updated: 2026-06-04 after BUG-11/12/13 (`e2661f8`).

## Closed in this sweep

| Tag | What landed | Commit |
|---|---|---|
| BUG-1 | Boolean POST round-trip pinned for SQLite; the original "TEXT in column" report turned out to be IMP-2's column default issue. | `bdbcc3f` |
| BUG-2 | scaffold env `"dev"` → `"Dev"` so figment-deserialised `Environment` parses. | `997e817` |
| BUG-3 | scaffold `async fn on_ready` → sync (matches `Plugin` trait). | `997e817` |
| BUG-4 | `#[umbra(index)]` emits `CREATE INDEX IF NOT EXISTS idx_<table>_<col>` on both backends. | `92f0964` |
| BUG-5 | `#[umbra(auto_now)]` / `#[umbra(auto_now_add)]` populate via `Utc::now()` on the dynamic write path. Typed path stays user-controlled. | `a6c1325` |
| BUG-6 | `#[umbra(unique_together = [[..]])]` → inline `UNIQUE (col1, col2)` on `CREATE TABLE`. | `d2cdc54` |
| BUG-7 | `#[umbra(indexes = [[..]])]` → follow-up `CREATE INDEX IF NOT EXISTS` after the table. | `d2cdc54` |
| BUG-8 | `#[umbra(ordering = ["-col", "col"])]` → default `ORDER BY` applied when no explicit `.order_by`. Django semantics: explicit ordering REPLACES the default. | `d2cdc54` |
| BUG-9 | `#[umbra(singleton)]` flips `Model::SINGLETON` + `ModelMeta.singleton`. | `5a5b18c` |
| BUG-10 | `rust_decimal::Decimal` field type (Postgres-only, gated by the field-backend system check). | `dac7c99` |
| BUG-11 | `Slug` wrapper type → `text_format = "slug"` marker + OpenAPI `pattern`. | `e2661f8` |
| BUG-12 | `Email` wrapper type → `text_format = "email"` marker + OpenAPI `format: email`. | `e2661f8` |
| BUG-13 | `Url` wrapper type → `text_format = "url"` marker + OpenAPI `format: uri`. | `e2661f8` |
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
 
## Open — new field types

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

Default order (post-BUG-11/12/13):

The remaining open items (BUG-14 file/image field, BUG-16 ManyToMany,
BUG-21 admin pickers, plus the playground frontend punch-list) are
all "needs its own spec before code lands" — none has a clean
half-day shape like the items just shipped. The next session should
either pick one and design it, or wait for a real consumer to drive
the requirements.
