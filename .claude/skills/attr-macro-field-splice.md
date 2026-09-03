---
name: attr-macro-field-splice
description: Use when a proc-macro attribute must INJECT fields into a struct before its derives run (e.g. #[model(base=X)] flat base embedding), or when emitting a macro_rules! from quote! that itself contains #[...] attributes.
---

# Attribute macro that splices fields into a struct before the derives

## Context

A derive macro can only READ its struct and APPEND items — it cannot add fields. To make an inherited base's columns show up as REAL fields (so `x.id` works, not `x.base.id`), you need an attribute macro that runs BEFORE the derives and rewrites the field list. But an attribute macro only sees the tokens of the item it's on — it has no cross-item reflection, so it can't know another struct's (the base's) field list. This is exactly the `#[model(base = X)]` problem in umbral (gaps4 #62/#64/#67, `crates/umbral-macros/src/lib.rs`).

## Approach

Two-macro handoff, mirroring how `mixin_cols!` / `__umbral_base_cols_<Base>` already worked:

1. The **base's** `#[derive(...)]` (here `ModelBase`, `EmitMode::Base` arm) emits, alongside its other output, a `#[macro_export] macro_rules! __umbral_base_fields_<Base>` that HOLDS the base's raw field tokens and pattern-matches a struct through its braces:
   ```
   ( $(#[$a:meta])* $vis:vis struct $name:ident { $($user:tt)* } ) => {
       $(#[$a])* $vis struct $name { #(#raw_fields,)* $($user)* }
   };
   ```
   Collect the raw fields with `quote!{ #field }` per `syn::Field` — `ToTokens` reproduces `#[attr] vis name: Ty` verbatim.

2. The **attribute macro** on the model (`#[proc_macro_attribute] pub fn model`) does almost nothing: parse `base = <TypePath>`, resolve the companion macro path (same crate-root `#[macro_export]` rule as `base_cols_macro_path` — factored into `base_companion_macro_path(base, prefix)`), and emit `#macro_path! { #item }`. It repackages the untouched struct tokens; rustc expands the field-splice macro next (ordinary recursive expansion, NOT eager).

3. Result is a plain flat struct with an ordinary `#[derive(Model, ...)]` — the derive's existing flatten path never triggers. Purely additive.

### Ordering: attribute MUST sit ABOVE the derive line

Attributes expand top-to-bottom, and each non-derive attr receives everything below it (incl. the not-yet-expanded `#[derive(...)]`). Put `#[model(...)]` below the derives and the derive runs first on the un-spliced struct → confusing error. Same rule as `serde_with::serde_as`. Add a best-effort `derives_mention_model` self-check (parse item as `DeriveInput`, scan for a `Model` derive) that emits a clear ordering hint; return `true` (don't block) if it doesn't parse as a struct.

## Why

- Field-splice via a base-emitted `macro_rules!` is the ONLY way to get the base's tokens to the model site — there's no compile-time reflection across items.
- Letting the flat fields become the model's OWN fields means `#[derive(Model)]` auto-emits their typed column consts (`X::CREATED_AT`) for free — so do NOT also auto-invoke `mixin_cols!`/`__umbral_base_cols_` in the splice macro; it double-defines the consts (E0034 "multiple ... found"). This closed #67 more simply than the design sketch, which assumed the mixin was still needed.

## Pitfalls

- **Literal `#` in `quote!`.** `quote!` reserves `#` for interpolation, so the `#[...]` inside the emitted `macro_rules!` can't be typed directly. Build it: `let pound = proc_macro2::Punct::new('#', Spacing::Alone);` then write `$( #pound [ $a:meta ] )*`. The `$`-fragments (`$name`, `$user`) are just literal `$ ident` tokens in `quote!` — no escaping needed (the existing `$model` in `__umbral_base_cols_` proves it).
- **Multi-base needs continuation-passing (shipped, gaps4 #89).** Nesting `__umbral_base_fields_B!{...}` inside A's argument does NOT pre-expand (macro expansion isn't eager), so A's matcher can't see B's fields. The fix: give each base companion macro `@compose` arms that re-invoke the NEXT base's macro from a queue after prepending their own fields; the last (queue-empty) emits the flat struct. The `model` proc-macro seeds the outermost call and the reversed queue so declaration order is preserved. Two techniques make it work: (1) **re-emitting captured `Name !` tokens forms a valid invocation** — `$($next:tt)* { … }` where `$next` captured `path !` expands the call, which sidesteps the hard rule that `$m:path ! (…)` invocation-by-metavariable is forbidden in a macro body; (2) each queue element is a bracket group `[ path ! ]` so the matcher `q[ [ $($next:tt)* ] $($rest:tt)* ]` can peel one base at a time. No `mixin_cols!` in the compose arms — spliced base columns become the model's OWN fields, so the derive emits their consts (same #67 reasoning as single-base).
- **Column module name is the STRUCT name** (snake_case), not the `#[umbral(table=...)]` name — `country::SLUG`, even with `table = "mab_country"`.
- **Attribute not in scope.** In a test using fully-qualified derives, write `#[umbral::model(...)]`, not bare `#[model(...)]` (bare needs `use umbral::prelude::*`).

## See also
- `.superpowers/research/modelbase-attr-macro-design.md` — the full design.
- `crates/umbral-macros/src/lib.rs` — `model`, `base_companion_macro_path`, the `EmitMode::Base` arm.
- `crates/umbral-core/tests/model_attr_base.rs`, `crates/umbral-macros/tests/model_attr_macro.rs`.
