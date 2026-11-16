# review_4 · umbral-macros

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 8,403 LOC (`crates/umbral-macros`) |
| **Reviewer** | claude |
| **Verdict** | minor work |
| **Findings** | 0 critical · 1 high · 2 medium · 2 low · 1 info — 6 new, 3 known |

## Summary

- One crate, one 7,397-line `lib.rs`, eleven proc-macro entry points: `Model`/`ModelBase`/`New`/`mixin_cols!`/`#[model]`, `Form`, `#[task]`, `#[umbral::main]`, `Choices`, `Validate`, `Dto`. Expansions are exercised behaviourally by ~32 integration files in `crates/umbral-core/tests/` plus this crate's own suite (trybuild UI-reject tests for `#[task]`, unit tests for the casing/rename transforms).
- Overall state: healthy. The four review_3 macro findings are genuinely fixed (single `classify_model_field`, Form skips `privileged`, PascalCase routed through `to_pascal_case`, kebab straight-replace + the now-true `min_length` Validate-only doc note). Diagnostics carry real spans, and there is no `unsafe`.
- Biggest risk (MACROS-1): `#[derive(Dto)]` silently ignores `#[serde(flatten)]`. The generated TypeScript client compiles and is wrong at runtime — the exact failure the same file's `rename_all` policy and the user-facing docs call the worst available, "because nothing errors". Fix first.
- Second (MACROS-2): review_3's Form/`privileged` fix landed in only one of two skip lists; the sibling copy in `fields()` references an ident nothing defines → E0425 for the very Model+Form structs the fix targeted. One-line mirror or de-duplication.
- Third (MACROS-3): no keyword/raw-ident handling in String→`Ident` construction — a model named `Match` emits `pub mod match` (won't compile) and a field named `r#type` panics the macro. Bounded and loud; both fixed by one shared helper.
- Compile-time cost is linear per field with no quadratic emission; per-field attrs are re-parsed on each pass (~6 passes, bounded and documented in the code).

## Findings

### MACROS-1 · HIGH · correctness · self-verified
**`#[derive(Dto)]` silently ignores `#[serde(flatten)]`, emitting a TypeScript interface that does not match the JSON serde actually produces.**
- **Where**: `crates/umbral-macros/src/lib.rs:7199-7222` (`dto_field_serde`), policy at `7147-7152`, the struct-level twin's error at `7182-7193`, the field loop at `7284-7305`; user docs `documentation/docs/v0.0.1/rest/custom-response-types.mdx` ("serde attributes are honoured" + the `rename_all` compile-error callout).
- **Evidence**:
  ```rust
  // dto_field_serde (7199-7222):
  if meta.path.is_ident("rename") { /* … */ }
  else if meta.path.is_ident("skip") || meta.path.is_ident("skip_serializing") { skip = true; }
  else if let Ok(v) = meta.value() { let _ = v.parse::<syn::Lit>(); }
  Ok(())
  ```
  `flatten` is a value-less path: it matches none of the branches (`meta.value()` returns `Err` for a path-only meta), so it falls through to `Ok(())` — silently ignored — and the field loop then emits the field as an ordinary property:
  ```rust
  // expand_dto (7284-7297):
  let (rename, skip) = dto_field_serde(&field.attrs);
  if skip { continue; }
  let raw = field.ident.as_ref().expect("named field").to_string();
  ```
  The crate's own stated policy for this file (7149-7152): "Honoured, or REJECTED — never ignored. … Silence is the one option that is not available." The struct-level analogue implements exactly that (7182-7193): an untranslatable `rename_all` style is a hard error whose text explains that emitting the Rust names "would produce a generated client that compiles and is WRONG against the server that produced it".
- **Impact**: `#[derive(Serialize, Dto)] struct MemberCard { id: i64, #[serde(flatten)] stamps: TimeStamps }` — serde inlines the `TimeStamps` fields into the wire object, while `typescript_for_dtos` declares `stamps: TimeStamps`. The generated client type-checks, and every read of `created_at`/`updated_at` is `undefined` at runtime. Reachable and idiomatic: the model-base docs recommend the `#[umbral(flatten)] #[serde(flatten)] #[sqlx(flatten)]` trio (`documentation/docs/v0.0.1/orm/model-base.mdx`), and `Dto` exists precisely to type hand-shaped response structs. Self-verified: re-read `dto_field_serde` and `expand_dto` in full; refuted "handled elsewhere" (`grep flatten` in the crate: only `#[umbral(flatten)]` model-base handling, no Dto site) and "documented limitation" (the docs list `rename`/`rename_all`/`skip` only). Meets HIGH by the sweep's explicit calibration: codegen whose failure is silent.
- **Fix**: match `meta.path.is_ident("flatten")` in `dto_field_serde`, thread it to `expand_dto`, and return the same error shape as the unknown `rename_all` style ("cannot yet inline a flattened field into TypeScript; list its fields explicitly or drop `flatten`"). Inlining the flattened struct's fields into the interface is the richer alternative; either beats silence.

### MACROS-2 · MEDIUM · correctness/codegen · self-verified
**The Form derive's `fields()` skip list is a stale copy of the `validate()` one and omits `privileged` and `auto_uuid`, so the exact Model+Form structs review_3's mass-assignment fix targeted now fail to compile.**
- **Where**: `crates/umbral-macros/src/lib.rs:5801-5816` (canonical `skip_for_form`) vs `6254-6273` (`field_var_idents` filter), emitted at `6298-6301`.
- **Evidence**:
  ```rust
  // 5801-5816 — validate()'s skip set, with the review_3 fix:
  let skip_for_form = model_attr.noform || model_attr.primary_key || model_attr.auto_now
      || model_attr.auto_now_add || model_attr.auto_uuid || is_implicit_pk
      || model_attr.privileged                 // ← review_3 fix (line 5812)
      || form_field_is_masked(&field.ty) || form_is_reverse_relation(field);
  ```
  ```rust
  // 6260-6267 — fields()'s set: the same list minus auto_uuid and privileged
  if attr.noform || attr.primary_key || attr.auto_now || attr.auto_now_add
      || is_implicit_pk || form_field_is_masked(&f.ty) || form_is_reverse_relation(f)
  { None } else { Some(format_ident!("_{}_field", ident)) }
  ```
  `fields()` then evaluates `vec![ #(#field_var_idents),* ]` (6300) after re-running only the builder loop, which `continue`s for skipped fields (5817) — so a skipped field's ident is referenced with nothing defining it.
- **Impact**: `#[derive(Model, Form)] struct Account { id: i64, email: String, #[umbral(privileged)] is_admin: bool }` (the documented "a single struct can serve as BOTH the persisted Model AND the user-submittable Form" pattern, 5791-5798) → `error[E0425]: cannot find value `_is_admin_field` in this scope`, pointing into macro-generated code. Same mechanism for `#[umbral(auto_uuid)]`. This is a regression *introduced by* the review_3 fix, not a re-report of it: the fix landed at 5812 and its sibling copy was not updated. No test covers it — across `crates/umbral-core/tests/` and `crates/umbral-macros/tests/`, no file contains both `privileged` and `umbral::forms::Form` (nor `auto_uuid` + `Form`); `form_derive.rs`'s `SignupForm { is_admin: bool }` is Form-only, with no `#[umbral]` attr (which could not even appear without a `Model` derive in scope). Self-verified: re-read both filters and the emission; refuted "the field builder is still emitted" (the `continue` at 5817 precedes every `field_builders.push`) and "the ident list is unused" (`FormValidate::fields()` is used by `render_html` and asserted in `form_derive.rs`).
- **Fix**: derive `field_var_idents` from the same iteration that pushes `field_builders` (one skip decision per field — the two-copy shape is the bug), or add the two missing predicates at 6260-6266. Add a `Model + Form + privileged` compile case to `form_derive.rs`.

### MACROS-3 · MEDIUM · correctness/hygiene · self-verified
**Every String→`Ident` construction in the derive lacks keyword/raw-ident handling: a model named `Match` emits `pub mod match`, and any `r#`-escaped name panics the macro inside `format_ident!`.**
- **Where**: `crates/umbral-macros/src/lib.rs:1752` (column module), `2459` (choices const), `2895`/`3039` (reverse trait names), `3484` (base-mode consts), `5139` (`column_const_for`), `5635` (`#[task]` handle); `crates/umbral-casing/src/lib.rs:59-61` (non-uppercase characters pass through).
- **Evidence**:
  ```rust
  // 1752:  let module_name  = format_ident!("{}", to_snake_case(&struct_name.to_string()));
  // 5139:  let const_ident  = format_ident!("{}", to_screaming_snake_case(field_name));
  // 5635:  let handle_ident = format_ident!("{}", to_pascal_case(&fn_name.to_string()));
  ```
  The argument is a `String`, so `quote`'s `mk_ident` keeps it verbatim and strips only a literal lowercase `r#` prefix before calling `Ident::new` (which panics on any other invalid ident, and accepts keywords). `to_snake_case` passes `#` through unchanged, so `to_screaming_snake_case("r#type") == "R#TYPE"` → `Ident::new("R#TYPE")` → "'R#TYPE' is not a valid Ident". Where the argument is an `&Ident` — `register_{}` (5605), `_{}_field` (6270), `_{}_raw` (5856) — `IdentFragment` strips the `r#` and the call is safe; the bug is specific to the String path.
- **Impact**: (a) `struct Match { id: i64, /* … */ }` — a realistic sports/competition model — expands to `pub mod match { pub const ID: /* … */ }` and dies with `expected identifier, found keyword match` inside generated code; no attribute overrides the module name (`Move`, `Type`, `Const`, `Ref`, `Box`, … behave the same). (b) `pub r#type: String` on a model — the shape `inspectdb` is expected to emit; `audit_2 core-migrate #15` defers the inspectdb side and its note says "r#-escaped idents need umbral-macros to unraw the column name" — panics the proc macro with no span at the offending field. Self-verified: re-read all seven sites; refuted "the casing helper sanitises it" (its own doc/tests show `#` passes through) and "`Ident::new` rejects keywords" (proc-macro2 deliberately accepts them; that is why the keyword case survives expansion and fails later).
- **Fix**: one helper — `fn umbral_ident(name: &str, span: Span) -> Ident` that strips a leading `r#`, then uses `syn::parse_str::<Ident>(name).unwrap_or_else(|_| Ident::new_raw(name, span))` — used at the seven sites; unraw the field name before deriving column/const names so `r#type` maps to column `type`, not `r#type`.

### MACROS-4 · LOW · drift · self-verified
**`#[serde(rename_all = "SCREAMING-KEBAB-CASE")]` is rejected by the allow-list although the transformer fifteen lines below implements it and a unit test asserts it — that arm is unreachable from the derive.**
- **Where**: `crates/umbral-macros/src/lib.rs:7174-7181` (allow-list; the error text at 7189-7191 tells users to "Use a supported style (…, SCREAMING_SNAKE_CASE)"), `7255` (`"SCREAMING-KEBAB-CASE" => name.to_uppercase().replace('_', "-")`), unit test at `7391-7394`.
- **Impact**: a user whose serialized struct uses serde's SCREAMING-KEBAB-CASE gets a compile error claiming the style cannot be translated, even though the translation exists and is tested; the workarounds are a different style or per-field `rename`. Self-verified: read all three sites — the transform arm and its test are dead code from the derive's perspective because `expand_dto` only calls `apply_rename_all` with a style the allow-list approved.
- **Fix**: add `"SCREAMING-KEBAB-CASE"` to the allow-list match (7174) and to the "supported style" list in the message; the existing transform and test then become live.

### MACROS-5 · LOW · consistency
**`#[umbral(geometry|geography)]` silently reclassifies ANY field kind, unlike every sibling attribute.**
- **Where**: `crates/umbral-macros/src/lib.rs:2080-2086`; contrast the cidr/xml/ltree/bit block ending at `2050` (no-op on foreign kinds), `precision` at `2091-2098` (no-op), and trim/lowercase/case_insensitive at `2271-2279` (field-spanned compile error).
- **Evidence**:
  ```rust
  kind = match kind {
      FieldKind::NullableGeometry(_) => FieldKind::NullableGeometry(spec),
      // A bare (non-Option) gis::Geometry, or any other kind the
      // attribute is (mis)placed on — the attribute is authoritative
      // for the spatial classification.
      _ => FieldKind::Geometry(spec),
  };
  ```
- **Impact**: `#[umbral(geometry = "point")] name: String` (a stray or typo'd attribute) makes the model metadata and the migration DDL declare a spatial column for a non-spatial Rust field; the symptom lands later — the boot-time backend check on SQLite, or a Postgres type error on first write — instead of at the attribute. The comment shows the fallback is deliberate for a bare `gis::Geometry`, but the `_` arm silently makes every other kind spatial too.
- **Fix**: keep the intentional bare-`Geometry` upgrade, but error (field-spanned, like the trim/lowercase path) when the kind is not a geometry kind.

### MACROS-6 · LOW · docs
**Rustdoc and unknown-attribute messages advertise surfaces the derive doesn't have, and the accepted-key lists have rotted behind the implementation.** (Six one-line drifts, same category.)
- **Where**/**Evidence**:
  - `lib.rs:108-110` and the struct-level unknown-attr error at `1516-1518`: "Other attributes (max_length, db_index, default, choices, on_delete) land as plugin authors need them" — `db_index` exists nowhere (the attribute is `index`, 1255), while max_length/default/choices/on_delete are all implemented, so the sentence misleads in both directions.
  - `lib.rs:100-101`: "The struct must have a field named `id`" — false since the PK lift; this crate's own test declares `#[umbral(primary_key)] pub code: String` with no `id` field (`tests/form_fk_string_pk.rs:25-27`), and `expand_model` errors only when there is neither a PK field nor a base (`3408-3415`).
  - `lib.rs:1246-1262`: the field-level "known keys are" list omits accepted keys `flatten`, `geometry`, `geography`, `srid`, `precision`, `scale`.
  - `lib.rs:4874-4875`: `is_serde_json_value`'s doc says the qualifier may be "`serde_json` or `json`" and "the segment before it (if any)", but the code (`4893-4897`) requires at least two segments and only `serde_json`.
  - `lib.rs:5607-5617`: the `#[task]` comment still explains the `Box::leak` override path; the code below it (`5618-5622`) emits the `&'static str` literal directly.
  - `lib.rs:641-643`: `FieldAttrs::privileged` still says "Still shown on forms." — ambiguous once the Form derive skips privileged fields (5812); say "admin forms" (staff still see `is_staff` there).
- **Impact**: a user following the `db_index` hint or the mandatory-`id` claim restructures working code; the rest is comment rot the next maintainer trips on. **Fix**: correct the six strings in place — the code paths themselves are fine.

### MACROS-7 · INFO · drift-hazard
- **Where**: `crates/umbral-macros/src/lib.rs:4523`, `4549`, `4574`, `4599`, `4630` — `foreign_key_inner`, `multichoice_inner`, `one_to_one_inner`, `reverse_set_inner`, `m2m_inner`.
- Each body is the same ~20 lines (`Type::Path` destructure, `segments.last()?`, leaf-ident comparison, one type argument) differing only in the type-name string, with `m2m_inner` alone accepting a second (PK) type argument; the doc comments cross-reference each other ("Mirrors [`m2m_inner`]") as the manual drift guard. Worth collapsing into one `wrapper_inner(ty, name, max_type_args)` next time one needs a change — the same consolidation that worked for the O2O rewrite inside `classify_model_field`.

## Strengths

- **One canonical classifier.** `classify_model_field` is the single place that decides `FieldKind`/`force_unique` and is called by both the emission and the relation passes — the review_3 O2O-rewrite duplication is genuinely gone, with the OneToOne→unique-FK rewrite performed once and carried as a verdict.
- **Diagnostics are field-spanned, not token-walls.** Field-level attribute errors go through `meta.error(...)` on the offending path (`lib.rs:1245`), the parser deliberately consumes a stray `= value` first so the outer parser cannot add a misleading second error (`1232-1239`), and the main loop turns one bad field into a single `compile_error!` and `continue`s (`1998-2004`). The later passes' `unwrap_or_default()`/`.ok()` calls (1718, 2529, 3656) are safe precisely because that first pass already emitted the error — they are not silent swallows of a reachable failure.
- **Invalid shapes get first-class messages.** No `fields.first().unwrap()` anywhere; a model with no PK and no base errors with "umbral::Model requires a primary-key field — either name a field `id` …, mark one with `#[umbral(primary_key)]`, or embed a `#[derive(ModelBase)]` base" (`3408-3415`), and `#[model(base)]` turns "derive missing or written below" into an explicit ordering message (`356-365`) instead of a downstream trait-bound failure.
- **Ident-arg vs String-arg discipline is mostly right.** Generated locals (`_{}_raw`, `_{}_parsed`, `_{}_field`, `register_{}`) are built from `&Ident` arguments, so `IdentFragment` handles raw idents for free (`5605`, `5856-5857`, `6270`); the failure surface is only the String path (MACROS-3). Emitted tokens are fully qualified (`::umbral::…`, `::core::…`, `::std::…`), so expansion does not depend on the call site's `use`s.
- **Tests.** trybuild UI-reject tests with asserted messages cover `#[task]` misuse (`tests/task_macro.rs`); the casing/`rename_all` transforms have unit tests including the serde-parity edges review_3 found (trailing underscore, double underscore); and expansions are exercised end-to-end by behavioural tests in `crates/umbral-core/tests/` (form submit → validate → read back, DTO → TS, choices round-trip, PK-shape variants).
- **No `unsafe`** anywhere in `src/` (grep), and no raw SQL (a generator emits none).

## Clean areas checked

- Attribute-vocabulary parity (Model vs Validate, the review_3 focus): the documented asymmetry is now true — the Model parser has no `min_length` branch at all (every `min_length` site is the Form parser's `#[form(...)]` at 5234+ or Validate's at 6788+), and the notes at `6770-6780` and `7021-7028` state it correctly.
- Casing canon: `umbral-casing::to_snake_case` is the only snake-case derivation, matching the model `TABLE` canonicality claim; no second implementation exists in the macros crate.
- serde rename parity: `apply_rename_all` is a straight `_`→`-` replace for kebab (7254) and routes camel/Pascal through `to_pascal_case` (7243-7251) — review_3's divergence is fixed and the empty-segment test exists (7391-7394).
- Expansion volume / compile-time cost: every field pass is a single `for field in fields` with no field×field nesting; `FIELDS`, column consts, assoc-const aliases and reverse-relation registries are each built once and emitted once. Nothing quadratic found. (Per-field attributes are re-parsed ~6 times across passes — bounded and documented as deliberate.)
- Panic audit: every `unwrap`/`expect` site is on a named-field invariant guarded by an earlier `Fields::Named` check (1546-1547, 6891-6897, 7267-7273) or a `len >= 2` pre-check (381); the only reachable panics are the raw-ident ones in MACROS-3.
- Review_3's "fixed" list re-verified in the tree: single classification (1832 region), `|| model_attr.privileged` in the Form skip list (5812 — but its sibling copy is MACROS-2), `to_pascal_case` in `expand_task` (5635), kebab straight-replace (7254), Model/Validate vocab note (6770-6780), `set_null`-on-non-nullable rejection (1305-1312).

## Known issues already tracked elsewhere (not counted in the totals)

- `audit_2 core-migrate #15` — inspectdb emits raw column names (`type`/`match`) as field idents; deferred, and the entry itself notes the macros-side `r#` unraw is what's missing. MACROS-3 is that macros-side half (the per-field proc-macro panic is the part the entry does not describe).
- `review_3 claude 02_triage` (macros findings) — O2O rewrite duplication, Model/Validate attr-vocab drift, PascalCase triplication, kebab divergence: all verified fixed; the `#[derive(Form)]` `privileged` omission is recorded fixed and its un-migrated residual is MACROS-2.
- `gaps2 #78` — `lib.rs` module split (deferred): context for the 7.4k-line single file, not re-reported.

## Out-of-scope notes

- `documentation/docs/v0.0.1/rest/custom-response-types.mdx` — the "serde attributes are honoured" section should gain the `flatten` outcome (error text or inlining) in the same change that fixes MACROS-1; the docs bug is not independent, only the page is another crate's file.
- `crates/umbral-core/src/inspect.rs:636-644` — the `r#`-escaping side of MACROS-3 lives in umbral-core's `inspectdb` codegen (`audit_2 core-migrate #15`, deferred); routed to the umbral-core reviewer so the two halves are fixed together.
- `crates/umbral-core/tests/` — the Model+Form+`privileged` compile case MACROS-2 needs belongs in `form_derive.rs`/`privileged_field.rs`, which are umbral-core files; flagged so the fix's test lands with the fix rather than being re-litigated.
- `crates/umbral-macros/tests/` — there are no trybuild UI tests for the `Model`/`Validate`/`Dto` attribute-error messages (only `#[task]` has them); the message lists MACROS-6 fixes would otherwise rot again. Test-suite gap, not a code defect.
