---
name: model-base-flatten
description: Use when working on #[derive(ModelBase)] / #[umbral(flatten)] (reusable model bases, the Django abstract-base pattern) — how the base's columns splice into a model's FIELDS, how PK-in-base is wired, and why it reuses expand_model.
---

# ModelBase + `#[umbral(flatten)]` (reusable model bases)

## Context
A `#[derive(ModelBase)]` struct declares shared columns once (e.g. `id` + `created_at` + `updated_at`); a `#[derive(Model)]` struct embeds it as a nested field marked `#[umbral(flatten)] #[serde(flatten)] #[sqlx(flatten)]` and inherits those columns as if written inline. This is umbral's answer to Django's abstract base model.

## Approach (how it's wired)

The whole design turns on one constraint: **a proc-macro sees only its own struct's tokens**, so `#[derive(Model)]` on the embedding struct cannot read the base's field names. Everything below works around that.

1. **Values ride on serde/sqlx flatten, for free.** The typed INSERT path is serde-based — `serialize_to_map(&instance)` = `serde_json::to_value(instance)` into a column→value map (`orm/queryset/write_helpers.rs`). `#[serde(flatten)]` on the base field flattens its values into that map; `#[sqlx(flatten)]` handles the read. So the derive never needs the base's field names for **values** — only for **column metadata**.

2. **Columns splice into `const FIELDS` at compile time.** `FieldSpec` is `Copy`, so `orm::concat_field_specs::<N>(&[parts])` (a const fn, `model.rs`) concatenates the model's own `&[FieldSpec]` runs with each base's `<Base as ModelBase>::BASE_FIELDS` — in declaration order. `N` is computed as `own_run_len + Base::BASE_FIELDS.len() + …` (a const expr on the concrete base type). With no flatten fields, the emission is byte-identical to the old literal `&[ … ]`, so existing models are untouched.

3. **`ModelBase` trait** (`orm/model.rs`) exposes `BASE_FIELDS`, `BASE_PK: Option<&str>`, `type BasePrimaryKey`, and `fn base_primary_key(&self)`. The last one lets the embedding model read the base's PK value without naming the base's `id` field: `ModelBase::base_primary_key(&self.<flatten_field_ident>)` — the derive DOES know the flatten field's ident (`base`), just not the base's inner field name.

4. **`#[derive(ModelBase)]` reuses `expand_model`** via an `EmitMode` param (`Model` | `Base`) — a base is structurally a model minus the table/trait parts, so the entire field-parsing loop is shared (full `#[umbral(...)]` attribute fidelity, zero duplication). In `Base` mode the fn returns early with just the `impl ModelBase`, skipping table name / relations / column consts / `objects()`.

5. **PK-in-base**: PK detection is deferred until *after* the field loop (the flatten bases are only known then). Resolution: own PK wins; else exactly one flatten base supplies it (`type PrimaryKey = <Base as ModelBase>::BasePrimaryKey`, `primary_key()`/`pk_as_json` route through `base_primary_key`); else the classic "no PK" error.

## Why
- Const-concat (not a runtime `field_specs()` accessor) because `T::FIELDS` is read directly in ~30 hot ORM paths; changing all of them would be far more invasive than a compile-time splice.
- Mode-flag reuse (not extracting the ~400-line FieldSpec block) keeps a base's attribute support identical to a model's and avoids drift.

## Pitfalls
- **Typed column consts for base fields don't exist yet** (`article::CREATED_AT`). Own fields get them; inherited ones must be queried by name (`.order_by("-created_at")`). Reason: same cross-struct-visibility limit; a fix needs a `ModelBase`-exported `macro_rules!` invoked as `mixin_cols!(Model: Base)`, and cross-crate macro resolution is the hard part. Tracked as `gaps5 #105`.
- A base cannot itself embed another base via `#[umbral(flatten)]` yet (the derive errors).
- The user MUST write all three attrs on the field: `#[umbral(flatten)]` (columns) + `#[serde(flatten)]` + `#[sqlx(flatten)]` (values). Missing serde/sqlx flatten → values won't round-trip.

## See also
- `crates/umbral-core/src/orm/model.rs` — `ModelBase`, `concat_field_specs`, `FieldSpec::PLACEHOLDER`.
- `crates/umbral-macros/src/lib.rs` — `EmitMode`, the flatten interception at the loop top, the post-loop PK resolution + `fields_const_tokens`.
- `crates/umbral-core/tests/model_base.rs` — behavioral coverage (FIELDS composition + live round-trip).
- `documentation/docs/v0.0.1/orm/model-base.mdx` — user-facing page.
