# Forms Learn Relations — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `#[derive(umbral::forms::Form)]` accept `ForeignKey<T>` / forward `OneToOne<T>` (→ `ModelChoice`), `M2M<T>` (→ `ModelMultiChoice` + post-insert junction write), and `#[umbral(choices)]` enums (→ `Select`), auto-skip reverse relations (`ReverseSet`, reverse `OneToOne`), and turn `FormValidate` async so FK/M2M existence checks and option fetches resolve through the ORM.

**Architecture:** Three `InputKind` variants (`Select`, `ModelChoice`, `ModelMultiChoice`) carry the metadata each relation field needs; the `Field` struct grows a parallel set of constructors. `FormValidate` goes `#[async_trait]` so `validate`/`render_html` can run ORM existence/option queries through the ambient `pool_dispatched()`. The macro (`expand_form`) reuses the Model derive's existing field-kind detection (`foreign_key_inner`/`one_to_one_inner`/`m2m_inner`/`reverse_set_inner`/`has_sqlx_skip`/`choices_ty`) so the two derives agree on what each field *is*, and emits the M2M junction write through the existing `set_junction_dynamic` machinery via a new `HydrateRelated::write_pending_m2m` hook called at the tail of the typed `create()` path.

**Tech Stack:** Rust, syn/quote (proc-macros), sqlx, axum, async-trait

---

## File Structure

| File | Create/Modify | Responsibility |
|---|---|---|
| `crates/umbral-core/src/forms.rs` | Modify | `FormValidate` → `#[async_trait]`; new `InputKind::{Select, ModelChoice, ModelMultiChoice}` variants + `PkKind` enum; `Field` constructors (`Field::select`, `Field::model_choice`, `Field::model_multi_choice`); async `render_html` walk + per-field async option fetch helper; `Form<T>` extractor awaits async `validate`; inline unit tests gain `.await` / `#[tokio::test]`. |
| `crates/umbral-core/src/orm/forms_runtime.rs` | Create | Runtime helpers the macro-generated `validate`/`render_html` call: `choice_is_member`, `fk_id_exists` (ORM existence probe via `DynQuerySet`), `fetch_choice_options` (compile-time, choices), `fetch_model_options` (async `(id,label)` fetch through `DynQuerySet`), `parse_multi_ids`. Keeps emitted macro code terse and the SQL out of `plugins/`. |
| `crates/umbral-core/src/orm/m2m.rs` | Modify | Add a `pending: Vec<sea_query::Value>` slot to `M2M<T,P>` plus `set_pending_ids` / `take_pending_ids`; a free `child_pk_kind_for_table` reuse of `pk_meta_for_table`. |
| `crates/umbral-core/src/orm/model.rs` | Modify | Add `HydrateRelated::write_pending_m2m(&mut self) -> impl Future` hook (default no-op) called by typed `create()` after insert. |
| `crates/umbral-core/src/orm/queryset/mod.rs` | Modify | After the post-INSERT `set_m2m_parent_ids()` in `create()` (line ~3064 / ~3093), call `row.write_pending_m2m().await?` so form-submitted M2M ids land as junction rows atomically. |
| `crates/umbral-macros/src/lib.rs` | Modify | `expand_form`: skip reverse relations before classification; classify FK/forward-O2O → `Field::model_choice`; choices → `Field::select`; M2M → `Field::model_multi_choice` + pending-id stuffing; emit async `validate`/`render_html` bodies; emit `write_pending_m2m` arms for M2M form fields on the Model derive's `HydrateRelated` impl. |
| `crates/umbral-core/src/orm/mod.rs` | Modify | `pub mod forms_runtime;` + re-export the runtime helpers used by emitted code. |
| `crates/umbral-core/tests/form_derive.rs` | Modify | Existing tests gain `.await` + `#[tokio::test]`; new tests: reverse-skip absent from `fields()`; choices round-trip + reject. |
| `crates/umbral-core/tests/form_fk.rs` | Create | FK + forward-O2O behavioral round-trip, existence reject (no row inserted), forward-O2O UNIQUE violation surfaces. |
| `crates/umbral-core/tests/form_m2m.rs` | Create | M2M junction-row round-trip, atomicity on bad id (zero junction rows). |
| `crates/umbral-core/tests/csrf_context.rs` | Modify | If any `FormValidate::validate` call exists here, add `.await` (the file is already async/`#[tokio::test]`). |
| `umbral_website/plugins/plugin_directory/src/models.rs` | Modify | Enable `#[derive(... umbral::forms::Form)]` on `PluginComment`, restore `#[form(...)]` attrs, delete the hand-rolled `Default`. |

---

## Task 1 — `FormValidate` becomes async (smallest viable green step) (spec Part 2)

Turn the trait async via `#[async_trait]`, update the macro's emitted impl, the `Form<T>` extractor, and every existing caller/test to `.await`. No new field kinds yet — the suite must stay green on a pure async-ification.

**Files:**
- Modify: `crates/umbral-core/src/forms.rs` (`FormValidate` trait ~71; `Form<T>` `FromRequest` ~954; inline tests ~979-1143)
- Modify: `crates/umbral-macros/src/lib.rs` (`expand_form` emitted impl ~3636-3654)
- Modify: `crates/umbral-core/tests/form_derive.rs` (every `::validate(` / `::render_html(` call)
- Test: `crates/umbral-core/tests/form_derive.rs`

Steps:

- [ ] Write the failing test. Append to `crates/umbral-core/tests/form_derive.rs` (it currently has no async runtime; this forces the trait to be awaitable):
  ```rust
  #[tokio::test]
  async fn async_validate_minimal_form_round_trips() {
      let form = MinimalForm::validate(&data(&[("title", "hello")]))
          .await
          .expect("should validate");
      assert_eq!(form.title, "hello");
  }
  ```
- [ ] Run it, expect FAIL (compile error — `validate` is sync, `.await` on a non-future):
  ```bash
  cd crates && cargo test -p umbral-core --test form_derive async_validate_minimal_form_round_trips
  ```
  Expect: `error[E0277]: ... is not a future` (or `method not found .await`).
- [ ] Implement the trait change in `crates/umbral-core/src/forms.rs`. Add the import near the top (after `use std::collections::HashMap;`):
  ```rust
  use async_trait::async_trait;
  ```
  Replace the `pub trait FormValidate` block (~71-99) with:
  ```rust
  #[async_trait]
  pub trait FormValidate: Sized {
      /// Parse and validate the form's input. Async because FK /
      /// M2M fields verify existence through the ORM before insert.
      async fn validate(data: &HashMap<String, String>) -> Result<Self, ValidationErrors>;

      /// The field declarations this form carries. Sync — kinds /
      /// validators only, no live options.
      fn fields() -> Vec<Field>;

      /// Render every field as an HTML `<label>` + `<input>` pair,
      /// prefilled from `data`. Async because `ModelChoice` /
      /// `ModelMultiChoice` fetch their options from the DB.
      async fn render_html(data: &HashMap<String, String>) -> String {
          let mut out = String::new();
          for field in Self::fields() {
              let value = data.get(&field.name).map(String::as_str).unwrap_or("");
              out.push_str("<div class=\"field\">");
              out.push_str(&format!(
                  "<label for=\"{name}\">{name}</label>",
                  name = field.name
              ));
              out.push_str(&field.render_html_async(value).await);
              out.push_str("</div>");
          }
          out
      }
  }
  ```
  Add a thin async shim on `Field` (right after the existing sync `render_html` ~565) so the default walk compiles before Task 6 adds the real option fetch:
  ```rust
  impl Field {
      /// Async render entry point. For non-relation fields this is
      /// the sync `render_html`; relation fields (Task 6) override
      /// to fetch their `<select>` options first.
      pub async fn render_html_async(&self, value: &str) -> String {
          self.render_html(value)
      }
  }
  ```
- [ ] Update the `Form<T>` extractor (~954). Change `T::validate(&pairs)` to `T::validate(&pairs).await`:
  ```rust
          match T::validate(&pairs).await {
              Ok(value) => Ok(Self::valid(value)),
              Err(errs) => {
                  let write_err: WriteError = errs.into();
                  Ok(Self::invalid(FormErrors::with_raw(write_err, pairs)))
              }
          }
  ```
- [ ] Update the macro emitted impl in `crates/umbral-macros/src/lib.rs` (~3636). Wrap the impl in `#[::async_trait::async_trait]` and make the two methods async:
  ```rust
      let output = quote! {
          #[::umbral::forms::async_trait]
          impl ::umbral::forms::FormValidate for #struct_name {
              async fn validate(
                  data: &::std::collections::HashMap<::std::string::String, ::std::string::String>,
              ) -> ::std::result::Result<Self, ::umbral::forms::ValidationErrors> {
                  let mut errs = ::umbral::forms::ValidationErrors::new();
                  #(#field_builders_iter)*
                  #(#validate_body)*
                  errs.into_result()?;
                  Ok(Self { #(#struct_inits),* #default_tail })
              }

              fn fields() -> ::std::vec::Vec<::umbral::forms::Field> {
                  #(#field_builders_iter2)*
                  vec![ #(#field_var_idents),* ]
              }
          }
      };
  ```
  Re-export `async_trait` from the forms module so `::umbral::forms::async_trait` resolves: in `crates/umbral-core/src/forms.rs` add near the top, below the import:
  ```rust
  #[doc(hidden)]
  pub use async_trait::async_trait;
  ```
  Confirm the facade re-exports `forms` (it does — `umbral::forms`); no facade edit needed because the path goes through `forms::async_trait`.
- [ ] Update the inline unit tests in `crates/umbral-core/src/forms.rs`. The `LoginForm` demo (~1113) impls `validate` by hand and isn't the trait — leave it sync (it does not impl `FormValidate`). Only tests that call a *derived* `validate`/`render_html` need `.await`; there are none inline (the `LoginForm` is a plain inherent method). No change to inline tests required for Task 1.
- [ ] Update `crates/umbral-core/tests/form_derive.rs`: convert every `#[test]` that calls `::validate(...)` or `::render_html(...)` to `#[tokio::test] async fn`, and add `.await` after each `::validate(...)` / `::render_html(...)` call. Example conversion for the first test:
  ```rust
  #[tokio::test]
  async fn minimal_string_form_round_trips_a_valid_input() {
      let form = MinimalForm::validate(&data(&[("title", "hello")]))
          .await
          .expect("should validate");
      assert_eq!(form.title, "hello");
  }
  ```
  Apply the same `#[tokio::test] async` + `.await` mechanical change to: `minimal_string_form_rejects_empty_input`, `signup_form_*`, `product_form_*`, the `render_html` tests (`SignupForm::render_html(&prefill).await`, `MinimalForm::render_html(&prefill).await`), and any other `::validate`/`::render_html` call site in the file.
- [ ] Check `crates/umbral-core/tests/csrf_context.rs` for any derived-form `::validate`/`::render_html` call; if present add `.await` (the file is already `#[tokio::test]`). If none, no change.
- [ ] Run, expect PASS:
  ```bash
  cd crates && cargo test -p umbral-core --test form_derive
  ```
  Expect: all green including `async_validate_minimal_form_round_trips`.
- [ ] Full workspace gate then commit:
  ```bash
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  git add -A && git commit -m "$(cat <<'EOF'
feat(forms): FormValidate goes async via #[async_trait]

ModelChoice/ModelMultiChoice fields need DB access to verify FK
existence and to fetch <select> options. Turn validate + render_html
async so those queries resolve through the ambient pool. No new field
kinds yet — pure async-ification keeping the suite green.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
  ```

---

## Task 2 — Reverse relations auto-skip in the Form derive (spec Part 1a)

`ReverseSet<C>` and reverse `OneToOne<T>` (the `#[sqlx(skip)]` variant) are back-pointers — never user-submittable. The Model derive already drops them; the Form derive must skip them before type classification, and they must be absent from `fields()` — without requiring `#[umbral(noform)]`.

**Files:**
- Modify: `crates/umbral-macros/src/lib.rs` (`expand_form` skip logic ~3430-3445; `field_var_idents` filter ~3607-3624)
- Test: `crates/umbral-core/tests/form_derive.rs`

Steps:

- [ ] Write the failing test. Append to `crates/umbral-core/tests/form_derive.rs`:
  ```rust
  // Reverse relations are back-pointers — the Form derive skips them
  // WITHOUT requiring #[umbral(noform)], and they're absent from fields().
  #[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
  #[umbral(table = "fd_skip_child")]
  struct SkipChild {
      pub id: i64,
      pub title: String,
      pub parent: umbral::orm::ForeignKey<SkipParent>,
  }

  #[derive(Debug, Clone, Default, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model, umbral::forms::Form)]
  #[umbral(table = "fd_skip_parent")]
  struct SkipParent {
      pub id: i64,
      pub name: String,
      // Reverse FK collection — NO #[umbral(noform)].
      #[sqlx(skip)]
      #[serde(skip)]
      #[umbral(reverse_fk = "parent")]
      pub child_set: umbral::orm::ReverseSet<SkipChild>,
      // Reverse OneToOne back-pointer — NO #[umbral(noform)].
      #[sqlx(skip)]
      #[serde(skip)]
      pub profile: umbral::orm::OneToOne<SkipChild>,
  }

  #[test]
  fn reverse_relations_absent_from_fields() {
      let names: Vec<String> = SkipParent::fields().into_iter().map(|f| f.name).collect();
      assert!(names.contains(&"name".to_string()), "scalar field present");
      assert!(!names.contains(&"child_set".to_string()), "ReverseSet skipped");
      assert!(!names.contains(&"profile".to_string()), "reverse OneToOne skipped");
  }
  ```
- [ ] Run it, expect FAIL (the Form derive currently rejects `ReverseSet`/`OneToOne` at the `classify_form_field_type` reject site ~3447):
  ```bash
  cd crates && cargo test -p umbral-core --test form_derive reverse_relations_absent_from_fields
  ```
  Expect: `error: umbral::Form derive: unsupported field type ...` at the `child_set` field span.
- [ ] Implement. In `crates/umbral-macros/src/lib.rs`, add a shared helper near the other `*_inner` helpers (~2473):
  ```rust
  /// True when this field is a reverse relation the Form derive must
  /// skip silently (no `#[umbral(noform)]` required): a `ReverseSet<C>`
  /// or a reverse `OneToOne<T>` (the `#[sqlx(skip)]` back-pointer
  /// variant). A forward `OneToOne<T>` (no `#[sqlx(skip)]`) is a
  /// unique FK and is NOT skipped — it becomes a ModelChoice (Task 4).
  fn form_is_reverse_relation(field: &syn::Field) -> bool {
      if reverse_set_inner(&field.ty).is_some() {
          return true;
      }
      if one_to_one_inner(&field.ty).is_some() && has_sqlx_skip(&field.attrs) {
          return true;
      }
      false
  }
  ```
  In the `expand_form` per-field loop, extend the `skip_for_form` decision (~3430). Replace:
  ```rust
          let skip_for_form = model_attr.noform
              || model_attr.primary_key
              || model_attr.auto_now
              || model_attr.auto_now_add
              || is_implicit_pk;
  ```
  with:
  ```rust
          let skip_for_form = model_attr.noform
              || model_attr.primary_key
              || model_attr.auto_now
              || model_attr.auto_now_add
              || is_implicit_pk
              || form_is_reverse_relation(field);
  ```
  And mirror it in the `field_var_idents` filter (~3607). Replace the closure body's condition:
  ```rust
          .filter_map(|f| {
              let attr = parse_umbral_field_attr(&f.attrs).unwrap_or_default();
              let ident = f.ident.as_ref()?;
              let is_implicit_pk = ident == "id";
              if attr.noform
                  || attr.primary_key
                  || attr.auto_now
                  || attr.auto_now_add
                  || is_implicit_pk
                  || form_is_reverse_relation(f)
              {
                  None
              } else {
                  Some(format_ident!("_{}_field", ident))
              }
          })
  ```
  Note: reverse-relation fields are skipped via `continue` so they never produce a `_<ident>_field` binding, and `any_skipped` is already set — the `..Default::default()` tail fills them.
- [ ] Run, expect PASS:
  ```bash
  cd crates && cargo test -p umbral-core --test form_derive reverse_relations_absent_from_fields
  ```
- [ ] Gate + commit:
  ```bash
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  git add -A && git commit -m "$(cat <<'EOF'
feat(forms): Form derive auto-skips reverse relations

ReverseSet<C> and reverse OneToOne<T> (#[sqlx(skip)]) are back-pointers,
never user-submittable. Skip them before type classification — like the
Model derive already does for FIELDS — so they no longer need a manual
#[umbral(noform)] and are absent from the derived fields().

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
  ```

---

## Task 3 — `#[umbral(choices)]` enum → a `Select` field (spec Part 1b)

A `#[umbral(choices)]` field becomes a `Select` whose options are the enum's compile-time `VALUES`/`LABELS`. `validate()` checks membership (no DB). A non-member produces a field-keyed error and no row.

**Files:**
- Modify: `crates/umbral-core/src/forms.rs` (`InputKind` ~319; `Field` ctors ~368; `render_html` ~539)
- Create: `crates/umbral-core/src/orm/forms_runtime.rs` (membership helper)
- Modify: `crates/umbral-core/src/orm/mod.rs` (`pub mod forms_runtime;`)
- Modify: `crates/umbral-macros/src/lib.rs` (`expand_form` choices arm)
- Test: `crates/umbral-core/tests/form_derive.rs`

Steps:

- [ ] Write the failing test. Append to `crates/umbral-core/tests/form_derive.rs` (round-trip each variant, decode back as the enum; reject out-of-set; no row needs a DB so this is a pure validate test):
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, umbral::orm::Choices, serde::Serialize, serde::Deserialize)]
  #[choices(rename_all = "lowercase")]
  enum Mood {
      #[default]
      Happy,
      Sad,
      Neutral,
  }

  #[derive(Debug, Default, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model, umbral::forms::Form)]
  #[umbral(table = "fd_choice_form")]
  struct ChoiceForm {
      pub id: i64,
      pub body: String,
      #[umbral(choices)]
      pub mood: Mood,
  }

  #[tokio::test]
  async fn choices_field_round_trips_every_variant() {
      for (raw, expected) in [("happy", Mood::Happy), ("sad", Mood::Sad), ("neutral", Mood::Neutral)] {
          let form = ChoiceForm::validate(&data(&[("body", "x"), ("mood", raw)]))
              .await
              .expect("valid variant");
          assert_eq!(form.mood, expected, "decoded back as the enum");
      }
  }

  #[tokio::test]
  async fn choices_field_rejects_out_of_set_value() {
      let err = ChoiceForm::validate(&data(&[("body", "x"), ("mood", "ecstatic")]))
          .await
          .expect_err("out-of-set rejected");
      assert!(err.fields.contains_key("mood"), "error keyed to the field");
  }

  #[test]
  fn choices_field_renders_a_select_with_all_options() {
      let names: Vec<String> = ChoiceForm::fields().into_iter().map(|f| f.name).collect();
      assert!(names.contains(&"mood".to_string()));
  }
  ```
- [ ] Run it, expect FAIL (macro rejects the `Mood` field type — `classify_form_field_type` returns `None` for a choices enum):
  ```bash
  cd crates && cargo test -p umbral-core --test form_derive choices_field_round_trips_every_variant
  ```
  Expect: `error: umbral::Form derive: unsupported field type ...` at the `mood` field.
- [ ] Implement the `InputKind::Select` variant + `Field::select` in `crates/umbral-core/src/forms.rs`. Add to the `InputKind` enum (~339, before the closing `}`):
  ```rust
      /// Closed-set enum (`#[umbral(choices)]`). Options are
      /// compile-time `(value, label)` pairs from `ChoiceField`.
      Select,
  ```
  Update `InputKind::html_type` (~342) — `Select` has no `<input type>`; add a match arm that returns `"text"` as a harmless default (the `render_html` `Select` arm below never calls `html_type`):
  ```rust
              InputKind::Select => "text",
  ```
  Add a `options` field to `Field` (the per-field option list, empty for non-Select). Change the `Field` struct (~361):
  ```rust
  pub struct Field {
      pub name: String,
      pub kind: InputKind,
      pub required: bool,
      pub validators: Vec<Box<dyn Validator>>,
      /// `(value, label)` pairs for `Select` fields. Empty otherwise.
      pub options: Vec<(String, String)>,
  }
  ```
  Add `options: Vec::new()` to every existing `Field` constructor literal (`text`, `integer`, `float`, `boolean` — the others build off `text`). Add the `Field::select` constructor after `Field::boolean` (~490):
  ```rust
      /// New closed-set select field. `options` are `(value, label)`
      /// pairs from a `ChoiceField`'s `VALUES`/`LABELS`. `nullable`
      /// prepends a leading empty option and drops `Required`.
      pub fn select(
          name: impl Into<String>,
          options: Vec<(String, String)>,
          nullable: bool,
      ) -> Self {
          let mut opts = options;
          if nullable {
              opts.insert(0, (String::new(), String::new()));
          }
          Self {
              name: name.into(),
              kind: InputKind::Select,
              required: !nullable,
              validators: Vec::new(),
              options: opts,
          }
      }
  ```
  Add the `Select` render arm to `Field::render_html` (~542, inside the `match self.kind`):
  ```rust
              InputKind::Select => {
                  let mut s = format!("<select name=\"{name}\"{required}>", name = self.name, required = required);
                  for (val, label) in &self.options {
                      let selected = if val == value { " selected" } else { "" };
                      s.push_str(&format!(
                          "<option value=\"{v}\"{selected}>{l}</option>",
                          v = html_escape(val),
                          l = html_escape(label),
                      ));
                  }
                  s.push_str("</select>");
                  s
              }
  ```
- [ ] Create `crates/umbral-core/src/orm/forms_runtime.rs` with the membership helper:
  ```rust
  //! Runtime helpers the `#[derive(Form)]`-generated `validate` /
  //! `render_html` call. Keeps SQL + ORM access in core (never in
  //! plugin code) and the emitted macro tokens terse.

  use crate::forms::ValidationErrors;

  /// Check that `value` is one of the compile-time choice `values`.
  /// On a miss, push a field-keyed error. Empty value on a nullable
  /// field is the caller's responsibility (it passes `nullable`).
  pub fn validate_choice_member(
      field: &str,
      value: &str,
      values: &[&'static str],
      nullable: bool,
      errs: &mut ValidationErrors,
  ) {
      if value.is_empty() {
          if !nullable {
              errs.add(field, format!("{field} is required"));
          }
          return;
      }
      if !values.iter().any(|v| *v == value) {
          errs.add(field, format!("{field} is not a valid choice"));
      }
  }

  /// Build `(value, label)` option pairs from a `ChoiceField`'s
  /// parallel `VALUES` / `LABELS` slices.
  pub fn choice_options(
      values: &[&'static str],
      labels: &[&'static str],
  ) -> Vec<(String, String)> {
      values
          .iter()
          .zip(labels.iter())
          .map(|(v, l)| ((*v).to_string(), (*l).to_string()))
          .collect()
  }
  ```
  Register it in `crates/umbral-core/src/orm/mod.rs` — add `pub mod forms_runtime;` alongside the other `pub mod` lines.
- [ ] Implement the macro choices arm in `crates/umbral-macros/src/lib.rs` `expand_form`. The choices signal is `parse_umbral_field_attr(&field.attrs).choices_ty.is_some()`. Read it before the `classify_form_field_type` call (~3447) and branch. Add, just above the `let Some((kind, is_option)) = classify_form_field_type(...)` line:
  ```rust
          // #[umbral(choices)] enum field → a Select. Options are the
          // enum's compile-time VALUES/LABELS; membership checked in
          // validate (no DB). Nullable (Option<T>) drops Required and
          // prepends an empty option.
          if model_attr.choices_ty.is_some() {
              let is_nullable = option_inner_type(&field.ty).is_some();
              let choice_ty: syn::Type = if let Some(inner) = option_inner_type(&field.ty) {
                  inner.clone()
              } else {
                  field.ty.clone()
              };
              let field_var = format_ident!("_{}_field", field_ident);
              let nullable_lit = if is_nullable { quote!(true) } else { quote!(false) };
              field_builders.push(quote! {
                  let #field_var: ::umbral::forms::Field = ::umbral::forms::Field::select(
                      #field_name,
                      ::umbral::orm::forms_runtime::choice_options(
                          <#choice_ty as ::umbral::orm::ChoiceField>::VALUES,
                          <#choice_ty as ::umbral::orm::ChoiceField>::LABELS,
                      ),
                      #nullable_lit,
                  );
              });
              let raw_var = format_ident!("_{}_raw", field_ident);
              let parsed_var = format_ident!("_{}_parsed", field_ident);
              validate_body.push(quote! {
                  let #raw_var: ::std::string::String =
                      data.get(#field_name).cloned().unwrap_or_default();
                  ::umbral::orm::forms_runtime::validate_choice_member(
                      #field_name,
                      &#raw_var,
                      <#choice_ty as ::umbral::orm::ChoiceField>::VALUES,
                      #nullable_lit,
                      &mut errs,
                  );
              });
              // Parse the validated string back into the enum (or
              // Option<enum>). from_str_ok returns None only when the
              // value isn't a member — already flagged above, so the
              // default fills the slot and the error short-circuits
              // the Ok(Self {...}) construction.
              let parse_expr = if is_nullable {
                  quote! {
                      if #raw_var.is_empty() {
                          ::core::option::Option::None
                      } else {
                          <#choice_ty as ::umbral::orm::ChoiceField>::from_str_ok(&#raw_var)
                      }
                  }
              } else {
                  quote! {
                      <#choice_ty as ::umbral::orm::ChoiceField>::from_str_ok(&#raw_var)
                          .unwrap_or_default()
                  }
              };
              validate_body.push(quote! {
                  let #parsed_var = { #parse_expr };
              });
              struct_inits.push(quote! { #field_ident: #parsed_var });
              continue;
          }
  ```
  Note: the non-nullable branch uses `.unwrap_or_default()` which requires the enum to be `Default`. Every `#[derive(Choices)]` enum in the codebase already derives `Default` (verified on `PluginComment`'s `CommentKind`); the test's `Mood` derives `Default` too. Confirm `ChoiceField` is re-exported as `umbral::orm::ChoiceField` (it is — `orm::choices::ChoiceField`).
- [ ] Run, expect PASS:
  ```bash
  cd crates && cargo test -p umbral-core --test form_derive choices_field
  ```
- [ ] Gate + commit:
  ```bash
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  git add -A && git commit -m "$(cat <<'EOF'
feat(forms): #[umbral(choices)] fields become a Select in Form derive

Options come from the enum's compile-time VALUES/LABELS (no DB).
validate() checks membership and decodes the value back into the enum;
an out-of-set value is a field-keyed error. Nullable (Option<T>) drops
Required and renders a leading empty option.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
  ```

---

## Task 4 — `ForeignKey<T>` / forward `OneToOne<T>` → a `ModelChoice` field (parse only) (spec Part 1c, validate-shape)

Classify FK and forward-O2O fields into a `ModelChoice` `InputKind` carrying the target table, optional label field, and PK kind. This task wires the *field descriptor + id parsing*; the async existence check lands in Task 5 and the async option fetch in Task 6.

**Files:**
- Modify: `crates/umbral-core/src/forms.rs` (`PkKind` enum; `InputKind::ModelChoice`; `Field::model_choice`)
- Modify: `crates/umbral-macros/src/lib.rs` (`expand_form` FK arm; `#[form(label_field="...")]` parse)
- Modify: `crates/umbral-core/src/orm/forms_runtime.rs` (id-parse helper)
- Test: `crates/umbral-core/tests/form_fk.rs` (Create)

Steps:

- [ ] Write the failing test. Create `crates/umbral-core/tests/form_fk.rs` with a real SQLite ambient pool (copy the `annotate_count.rs` boot pattern) and a parent + FK-child form. This task asserts the *happy-path round-trip* (FK id parsed, child created, `resolve()` returns the real parent):
  ```rust
  #![allow(dead_code)]
  use std::collections::HashMap;
  use tokio::sync::OnceCell;
  use umbral::orm::{ForeignKey, Model};
  use umbral::forms::FormValidate;
  use umbral_core::db;

  #[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
  #[umbral(table = "ffk_author")]
  struct Author {
      pub id: i64,
      pub name: String,
  }

  #[derive(Debug, Clone, Default, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model, umbral::forms::Form)]
  #[umbral(table = "ffk_book")]
  struct Book {
      #[umbral(primary_key)]
      pub id: i64,
      #[form(required, length(min = 1, max = 200))]
      pub title: String,
      pub author: ForeignKey<Author>,
  }

  fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
      pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
  }

  static BOOT: OnceCell<()> = OnceCell::const_new();
  async fn boot() {
      BOOT.get_or_init(|| async {
          let settings = umbral::Settings::from_env().expect("figment defaults");
          let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
          umbral::App::builder()
              .settings(settings)
              .database("default", pool.clone())
              .model::<Author>()
              .model::<Book>()
              .build()
              .expect("App::build");
          sqlx::query("CREATE TABLE ffk_author (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
              .execute(&pool).await.expect("create author");
          sqlx::query("CREATE TABLE ffk_book (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, author INTEGER NOT NULL REFERENCES ffk_author(id))")
              .execute(&pool).await.expect("create book");
          sqlx::query("INSERT INTO ffk_author (name) VALUES ('Ada')")
              .execute(&pool).await.expect("seed author");
      }).await;
  }

  #[tokio::test]
  async fn fk_field_parses_and_links_real_parent() {
      boot().await;
      let book = Book::validate(&data(&[("title", "Notes"), ("author", "1")]))
          .await
          .expect("valid FK");
      // The parsed FK carries the submitted id.
      assert_eq!(book.author.id(), 1);
      // Persist + read the parent back through the ORM.
      let created = Book::objects().create(book).await.expect("create book");
      let parent = created.author.resolve(db::pool()).await.expect("resolve parent");
      assert_eq!(parent.name, "Ada", "FK resolves to the actual seeded parent");
  }
  ```
- [ ] Run it, expect FAIL (the `author: ForeignKey<Author>` field is rejected by the Form derive):
  ```bash
  cd crates && cargo test -p umbral-core --test form_fk fk_field_parses_and_links_real_parent
  ```
  Expect: `error: umbral::Form derive: unsupported field type ...` at the `author` field.
- [ ] Implement `PkKind` + `InputKind::ModelChoice` + `Field::model_choice` in `crates/umbral-core/src/forms.rs`. Add the `PkKind` enum (above `InputKind`, ~317):
  ```rust
  /// How to parse a submitted FK id string. Resolved from the target
  /// model's PK SqlType at render/validate time.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum PkKind {
      BigInt,
      Uuid,
      Text,
  }
  ```
  Add to `InputKind` (~339):
  ```rust
      /// FK / forward O2O to another model. Options fetched at render
      /// time (async). `label_field` overrides the default label column.
      ModelChoice {
          target_table: &'static str,
          label_field: Option<&'static str>,
          pk_kind: PkKind,
      },
  ```
  Add an `InputKind::ModelChoice { .. } => "text"` arm to `html_type`. Add the `Field::model_choice` ctor after `Field::select`:
  ```rust
      /// New single-select FK field. `options` are fetched async at
      /// render time (Task 6); at validate time only `pk_kind` is used
      /// to parse the submitted id.
      pub fn model_choice(
          name: impl Into<String>,
          target_table: &'static str,
          label_field: Option<&'static str>,
          pk_kind: PkKind,
          nullable: bool,
      ) -> Self {
          Self {
              name: name.into(),
              kind: InputKind::ModelChoice { target_table, label_field, pk_kind },
              required: !nullable,
              validators: Vec::new(),
              options: Vec::new(),
          }
      }
  ```
- [ ] Add the id-parse helper to `crates/umbral-core/src/orm/forms_runtime.rs`:
  ```rust
  use crate::forms::PkKind;

  /// Resolve a target table's PK kind from the registry, defaulting to
  /// BigInt before the registry is populated (tests build the registry
  /// in App::build, so this only matters pre-boot).
  pub fn pk_kind_for_table(table: &str) -> PkKind {
      match crate::migrate::pk_meta_for_table(table).map(|(_, ty)| ty) {
          Some(crate::orm::SqlType::Uuid) => PkKind::Uuid,
          Some(crate::orm::SqlType::Text) => PkKind::Text,
          _ => PkKind::BigInt,
      }
  }
  ```
- [ ] Implement the macro FK arm in `expand_form`. Detection reuses `foreign_key_inner` (covers `ForeignKey<T>`) and the `Option<ForeignKey<T>>` shape. Forward `OneToOne<T>` (no `#[sqlx(skip)]`) also lands here — it's a unique FK. Add, just below the choices arm and above `classify_form_field_type`:
  ```rust
          // FK / forward O2O / Option<FK> → ModelChoice.
          // Forward OneToOne<T> (no #[sqlx(skip)]) is a unique FK; the
          // reverse variant was already skipped in Task 2.
          let fk_target: Option<syn::Type> =
              foreign_key_inner(&field.ty).cloned()
                  .or_else(|| option_inner_type(&field.ty).and_then(|i| foreign_key_inner(i).cloned()))
                  .or_else(|| {
                      one_to_one_inner(&field.ty)
                          .filter(|_| !has_sqlx_skip(&field.attrs))
                          .cloned()
                  });
          if let Some(target_ty) = fk_target {
              let is_nullable = option_inner_type(&field.ty).is_some() || attrs.optional;
              let label_field_tokens = match &attrs.label_field {
                  Some(lf) => quote!(::core::option::Option::Some(#lf)),
                  None => quote!(::core::option::Option::None),
              };
              let nullable_lit = if is_nullable { quote!(true) } else { quote!(false) };
              let field_var = format_ident!("_{}_field", field_ident);
              field_builders.push(quote! {
                  let #field_var: ::umbral::forms::Field = ::umbral::forms::Field::model_choice(
                      #field_name,
                      <#target_ty as ::umbral::orm::Model>::TABLE,
                      #label_field_tokens,
                      ::umbral::orm::forms_runtime::pk_kind_for_table(
                          <#target_ty as ::umbral::orm::Model>::TABLE,
                      ),
                      #nullable_lit,
                  );
              });
              let raw_var = format_ident!("_{}_raw", field_ident);
              let parsed_var = format_ident!("_{}_parsed", field_ident);
              // Validate presence + parse the id (existence check added
              // in Task 5). For v1 the parsed FK stores the i64 id.
              validate_body.push(quote! {
                  let #raw_var: ::std::string::String =
                      data.get(#field_name).cloned().unwrap_or_default();
                  if #raw_var.is_empty() && !#nullable_lit {
                      errs.add(#field_name, format!("{} is required", #field_name));
                  }
              });
              let parse_expr = if is_nullable {
                  quote! {
                      if #raw_var.is_empty() {
                          ::core::option::Option::None
                      } else {
                          match #raw_var.parse::<i64>() {
                              ::core::result::Result::Ok(v) =>
                                  ::core::option::Option::Some(::umbral::orm::ForeignKey::new(v)),
                              ::core::result::Result::Err(_) => {
                                  errs.add(#field_name, format!("{} must be a valid id", #field_name));
                                  ::core::option::Option::None
                              }
                          }
                      }
                  }
              } else {
                  quote! {
                      match #raw_var.parse::<i64>() {
                          ::core::result::Result::Ok(v) => ::umbral::orm::ForeignKey::new(v),
                          ::core::result::Result::Err(_) => {
                              errs.add(#field_name, format!("{} must be a valid id", #field_name));
                              ::umbral::orm::ForeignKey::new(0)
                          }
                      }
                  }
              };
              validate_body.push(quote! {
                  let #parsed_var = { #parse_expr };
              });
              struct_inits.push(quote! { #field_ident: #parsed_var });
              continue;
          }
  ```
  Note: the non-nullable `ForeignKey::new(0)` placeholder is only reached when the id failed to parse — `errs` is already non-empty so `errs.into_result()?` short-circuits before the `Self { .. }` literal is constructed. No invalid FK ever reaches a created row.
- [ ] Add `label_field` parsing to the form-field attr parser. In `crates/umbral-macros/src/lib.rs`, find the `#[form(...)]` attr struct (`parse_form_attrs`) and add a `label_field: Option<String>` field, parsing `#[form(label_field = "name")]` the same way `regex`/`message` are parsed. (Locate by `grep -n "fn parse_form_attrs" crates/umbral-macros/src/lib.rs` and add a `meta.path.is_ident("label_field")` arm reading a `LitStr`.)
- [ ] Run, expect PASS:
  ```bash
  cd crates && cargo test -p umbral-core --test form_fk fk_field_parses_and_links_real_parent
  ```
- [ ] Gate + commit:
  ```bash
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  git add -A && git commit -m "$(cat <<'EOF'
feat(forms): ForeignKey/forward-OneToOne fields become a ModelChoice

The Form derive classifies ForeignKey<T>, Option<ForeignKey<T>>, and
forward OneToOne<T> (a unique FK) into an InputKind::ModelChoice
carrying the target table, optional #[form(label_field)] override, and
the target PK kind. validate() parses the submitted id into a typed
ForeignKey. Existence check + option render land in following commits.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
  ```

---

## Task 5 — Async FK existence validation (spec Part 2, FK existence check)

`validate()` must verify the submitted FK id points at a live row through the ORM (`DynQuerySet`, never raw SQL). A miss is a field-keyed error and no row is inserted. Forward-O2O additionally relies on the DB UNIQUE constraint surfacing a `WriteError` on a duplicate.

**Files:**
- Modify: `crates/umbral-core/src/orm/forms_runtime.rs` (async existence probe)
- Modify: `crates/umbral-macros/src/lib.rs` (FK arm emits the await call)
- Test: `crates/umbral-core/tests/form_fk.rs`

Steps:

- [ ] Write the failing tests. Append to `crates/umbral-core/tests/form_fk.rs`:
  ```rust
  #[tokio::test]
  async fn fk_field_rejects_nonexistent_parent_and_inserts_no_row() {
      boot().await;
      let before = Book::objects().count().await.expect("count before");
      let err = Book::validate(&data(&[("title", "Ghost"), ("author", "9999")]))
          .await
          .expect_err("nonexistent FK rejected");
      assert!(err.fields.contains_key("author"), "error keyed to the FK field");
      let after = Book::objects().count().await.expect("count after");
      assert_eq!(before, after, "no row inserted on a bad FK");
  }

  // Forward O2O is a unique FK — a duplicate target surfaces as a
  // WriteError from the DB UNIQUE constraint, not a silent second row.
  #[derive(Debug, Clone, Default, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model, umbral::forms::Form)]
  #[umbral(table = "ffk_passport")]
  struct Passport {
      #[umbral(primary_key)]
      pub id: i64,
      #[umbral(unique)]
      pub holder: ForeignKey<Author>,
      #[form(required, length(min = 1, max = 40))]
      pub number: String,
  }

  #[tokio::test]
  async fn forward_o2o_unique_violation_surfaces_as_write_error() {
      boot().await;
      sqlx::query("CREATE TABLE IF NOT EXISTS ffk_passport (id INTEGER PRIMARY KEY AUTOINCREMENT, holder INTEGER NOT NULL UNIQUE REFERENCES ffk_author(id), number TEXT NOT NULL)")
          .execute(&db::pool()).await.expect("create passport");
      let p1 = Passport::validate(&data(&[("holder", "1"), ("number", "A1")]))
          .await.expect("valid o2o");
      Passport::objects().create(p1).await.expect("first o2o row");
      let p2 = Passport::validate(&data(&[("holder", "1"), ("number", "B2")]))
          .await.expect("validates (existence ok); UNIQUE fires at insert");
      let err = Passport::objects().create(p2).await.expect_err("duplicate target");
      // A unique violation, not a silent second row. (WriteError variants
      // verified at write.rs:48 — UniqueViolation / Multiple / Sqlx.)
      assert!(matches!(err, umbral::orm::write::WriteError::UniqueViolation { .. }
          | umbral::orm::write::WriteError::Multiple { .. }
          | umbral::orm::write::WriteError::Sqlx(_)),
          "duplicate forward-O2O surfaces a WriteError: {err:?}");
  }
  ```
  Note: `Passport` needs registration. Extend `boot()`'s builder with `.model::<Passport>()`. `umbral::orm::write::WriteError` is the canonical path (re-exported from `crate::orm::write::WriteError`); the real variants are `UniqueViolation { .. }`, `ForeignKeyViolation { .. }`, `ForeignKeyNotFound { .. }`, `Multiple { errors }`, `Validator { field, message }`, `Sqlx(_)`.
- [ ] Run, expect FAIL (existence isn't checked yet — a nonexistent id validates and inserts a dangling-FK row, or the count assertion fails):
  ```bash
  cd crates && cargo test -p umbral-core --test form_fk fk_field_rejects_nonexistent_parent_and_inserts_no_row
  ```
  Expect: `before != after` assertion failure (a row got inserted) OR the FK-error key is missing.
- [ ] Implement the async existence probe in `crates/umbral-core/src/orm/forms_runtime.rs`:
  ```rust
  /// Verify a row with PK == `id` exists in `target_table`, through
  /// the ORM (never raw SQL). On a miss, push a field-keyed error.
  /// Empty id on a nullable field is a no-op (the caller checks
  /// requiredness). Registry / pool failures are swallowed as a miss
  /// — a form can't validate against a DB that isn't up.
  pub async fn validate_fk_exists(
      field: &str,
      id: &str,
      target_table: &str,
      nullable: bool,
      errs: &mut ValidationErrors,
  ) {
      if id.is_empty() {
          if !nullable {
              errs.add(field, format!("{field} is required"));
          }
          return;
      }
      let Some(meta) = crate::migrate::registered_models()
          .into_iter()
          .find(|m| m.table == target_table)
      else {
          // Target not registered — can't verify; leave it to the DB FK.
          return;
      };
      let Some(pk_col) = meta.pk_column().map(|c| c.name.clone()) else {
          return;
      };
      let exists = crate::orm::dynamic::DynQuerySet::for_meta(&meta)
          .filter_eq_string(&pk_col, id)
          .count()
          .await
          .map(|n| n > 0)
          .unwrap_or(false);
      if !exists {
          errs.add(field, format!("{field}: no matching record"));
      }
  }
  ```
  Confirm `DynQuerySet` and `ModelMeta::pk_column()` are reachable: `DynQuerySet` is `crate::orm::dynamic::DynQuerySet`; `registered_models()` is `crate::migrate::registered_models`; `pk_column()` is used in `pk_meta_for_table` already, so it exists on `ModelMeta`. Add `pub use self::dynamic::DynQuerySet;`-style visibility only if needed (it's `pub` within `orm::dynamic`).
- [ ] Update the macro FK arm to call the existence check. In the FK `validate_body.push(...)` block (Task 4), replace the presence-only check with:
  ```rust
              validate_body.push(quote! {
                  let #raw_var: ::std::string::String =
                      data.get(#field_name).cloned().unwrap_or_default();
                  ::umbral::orm::forms_runtime::validate_fk_exists(
                      #field_name,
                      &#raw_var,
                      <#target_ty as ::umbral::orm::Model>::TABLE,
                      #nullable_lit,
                      &mut errs,
                  ).await;
              });
  ```
  The `.await` is now legal because `validate` is async (Task 1). The parse step is unchanged — it still runs so the `ForeignKey` is constructed, but `errs.into_result()?` short-circuits before the `Self { .. }` literal when existence failed.
- [ ] Run, expect PASS:
  ```bash
  cd crates && cargo test -p umbral-core --test form_fk
  ```
- [ ] Gate + commit:
  ```bash
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  git add -A && git commit -m "$(cat <<'EOF'
feat(forms): FK fields verify existence through the ORM before insert

validate() runs a DynQuerySet existence probe (count() > 0, never raw
SQL) against the target table's PK. A miss is a field-keyed error and
no row is inserted. Forward-O2O reuses the same path; its DB UNIQUE
constraint surfaces a WriteError on a duplicate target.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
  ```

---

## Task 6 — Async render fetches `ModelChoice` options (spec Part 2, render path)

`render_html` must emit a populated `<select>` for `ModelChoice` / (Task 7) `ModelMultiChoice` fields by fetching `(id, label)` rows from the target table through the ORM.

**Files:**
- Modify: `crates/umbral-core/src/orm/forms_runtime.rs` (async option fetch)
- Modify: `crates/umbral-core/src/forms.rs` (`Field::render_html_async` override for `ModelChoice`)
- Test: `crates/umbral-core/tests/form_fk.rs`

Steps:

- [ ] Write the failing test. Append to `crates/umbral-core/tests/form_fk.rs`:
  ```rust
  #[tokio::test]
  async fn fk_field_renders_select_with_seeded_options() {
      boot().await;
      let html = Book::render_html(&data(&[])).await;
      // The author <select> carries the seeded parent as an option.
      assert!(html.contains("<select name=\"author\""), "renders a select: {html}");
      assert!(html.contains("value=\"1\""), "seeded author id is an option: {html}");
      assert!(html.contains("Ada"), "label is the parent's text column: {html}");
  }
  ```
- [ ] Run, expect FAIL (the default `render_html_async` shim from Task 1 just calls sync `render_html`, which renders a `ModelChoice` as a bare `<input type="text">` with no options):
  ```bash
  cd crates && cargo test -p umbral-core --test form_fk fk_field_renders_select_with_seeded_options
  ```
  Expect: `renders a select` assertion fails (no `<select name="author"`).
- [ ] Implement the async option fetch in `crates/umbral-core/src/orm/forms_runtime.rs`:
  ```rust
  /// Fetch `(id, label)` option rows for a ModelChoice/ModelMultiChoice
  /// `<select>` through the ORM. `label_field` overrides the label
  /// column; default is the first non-PK text column (matches the
  /// admin's fk_picker convention). Returns at most 1000 rows — a form
  /// `<select>` with more candidates needs a search widget, not a flat
  /// list. Errors → empty options (an unrenderable select beats a 500).
  pub async fn fetch_model_options(
      target_table: &str,
      label_field: Option<&str>,
  ) -> Vec<(String, String)> {
      let Some(meta) = crate::migrate::registered_models()
          .into_iter()
          .find(|m| m.table == target_table)
      else {
          return Vec::new();
      };
      let Some(pk_col) = meta.pk_column().map(|c| c.name.clone()) else {
          return Vec::new();
      };
      let label_col = label_field
          .map(|s| s.to_string())
          .or_else(|| {
              meta.fields
                  .iter()
                  .find(|c| c.ty == crate::orm::SqlType::Text && c.name != pk_col)
                  .map(|c| c.name.clone())
          })
          .unwrap_or_else(|| pk_col.clone());
      // fetch_as_json returns Vec<serde_json::Map<String, Value>> — each
      // row is already a Map, no .as_object() needed.
      let rows = crate::orm::dynamic::DynQuerySet::for_meta(&meta)
          .select_cols(&[pk_col.clone(), label_col.clone()])
          .limit(1000)
          .fetch_as_json()
          .await
          .unwrap_or_default();
      rows.into_iter()
          .filter_map(|obj| {
              let id = json_scalar_to_string(obj.get(&pk_col)?);
              let label = obj.get(&label_col).map(json_scalar_to_string).unwrap_or_else(|| id.clone());
              Some((id, label))
          })
          .collect()
  }

  /// Stringify a JSON scalar for option values/labels.
  fn json_scalar_to_string(v: &serde_json::Value) -> String {
      match v {
          serde_json::Value::String(s) => s.clone(),
          serde_json::Value::Number(n) => n.to_string(),
          serde_json::Value::Bool(b) => b.to_string(),
          serde_json::Value::Null => String::new(),
          other => other.to_string(),
      }
  }
  ```
  `DynQuerySet::fetch_as_json` returns `Result<Vec<serde_json::Map<String, serde_json::Value>>, DynError>` (verified at `dynamic.rs:898`); `.unwrap_or_default()` yields `Vec<Map<..>>`, which the `filter_map` iterates directly.
- [ ] Implement the `Field::render_html_async` override in `crates/umbral-core/src/forms.rs`. Replace the Task-1 shim with a real dispatch:
  ```rust
  impl Field {
      /// Async render entry point. ModelChoice / ModelMultiChoice
      /// fetch their options first, then render the `<select>`; every
      /// other kind defers to the sync `render_html`.
      pub async fn render_html_async(&self, value: &str) -> String {
          match self.kind {
              InputKind::ModelChoice { target_table, label_field, .. } => {
                  let options = crate::orm::forms_runtime::fetch_model_options(target_table, label_field).await;
                  self.render_select(&options, value, false)
              }
              InputKind::ModelMultiChoice { target_table, label_field, .. } => {
                  let options = crate::orm::forms_runtime::fetch_model_options(target_table, label_field).await;
                  self.render_select(&options, value, true)
              }
              _ => self.render_html(value),
          }
      }

      /// Shared `<select>` writer for Select / ModelChoice /
      /// ModelMultiChoice. `multiple` adds the `multiple` attribute and
      /// the `[]` name suffix convention. `selected` matches against a
      /// space/comma-joined value string (the multi-value case).
      fn render_select(&self, options: &[(String, String)], value: &str, multiple: bool) -> String {
          let multiple_attr = if multiple { " multiple" } else { "" };
          let required = if self.required { " required" } else { "" };
          let mut s = format!(
              "<select name=\"{name}\"{multiple_attr}{required}>",
              name = self.name,
          );
          if !multiple && !self.required {
              s.push_str("<option value=\"\"></option>");
          }
          for (val, label) in options {
              let selected = if val == value { " selected" } else { "" };
              s.push_str(&format!(
                  "<option value=\"{v}\"{selected}>{l}</option>",
                  v = html_escape(val),
                  l = html_escape(label),
              ));
          }
          s.push_str("</select>");
          s
      }
  }
  ```
- [ ] Run, expect PASS:
  ```bash
  cd crates && cargo test -p umbral-core --test form_fk fk_field_renders_select_with_seeded_options
  ```
- [ ] Gate + commit:
  ```bash
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  git add -A && git commit -m "$(cat <<'EOF'
feat(forms): render_html fetches ModelChoice <select> options via ORM

The async render path queries the target table through DynQuerySet for
(id, label) pairs — id from the PK, label from #[form(label_field)] or
the first non-PK text column — and emits a populated <select>. Errors
degrade to an empty select rather than a 500.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
  ```

---

## Task 7 — `M2M<T>` → `ModelMultiChoice` + post-insert junction write (spec Part 1d)

`M2M<T>` has no parent column. The Form derive emits a `ModelMultiChoice` whose `validate()` parses the submitted id *list* (multi-value), verifies each id exists, and stuffs the validated ids onto the `M2M` field's pending slot; the typed `create()` writes them as junction rows after the parent insert, atomically, reusing `set_junction_dynamic`.

**Files:**
- Modify: `crates/umbral-core/src/orm/m2m.rs` (pending-ids slot + accessors)
- Modify: `crates/umbral-core/src/orm/model.rs` (`HydrateRelated::write_pending_m2m` hook)
- Modify: `crates/umbral-core/src/orm/queryset/mod.rs` (`create()` calls the hook after insert)
- Modify: `crates/umbral-core/src/forms.rs` (`InputKind::ModelMultiChoice`, `Field::model_multi_choice`)
- Modify: `crates/umbral-core/src/orm/forms_runtime.rs` (multi-id parse + per-id existence)
- Modify: `crates/umbral-macros/src/lib.rs` (`expand_form` M2M arm + `write_pending_m2m` arms on the Model derive's HydrateRelated impl)
- Test: `crates/umbral-core/tests/form_m2m.rs` (Create)

Steps:

- [ ] Write the failing test. Create `crates/umbral-core/tests/form_m2m.rs` (junction-row round-trip + atomicity on bad id):
  ```rust
  #![allow(dead_code)]
  use std::collections::HashMap;
  use tokio::sync::OnceCell;
  use umbral::orm::Model;
  use umbral::forms::FormValidate;
  use umbral_core::db;

  #[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
  #[umbral(table = "fm_tag")]
  struct Tag { pub id: i64, pub name: String }

  #[derive(Debug, Clone, Default, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model, umbral::forms::Form)]
  #[umbral(table = "fm_article")]
  struct Article {
      #[umbral(primary_key)]
      pub id: i64,
      #[form(required, length(min = 1, max = 200))]
      pub title: String,
      #[sqlx(skip)]
      #[serde(skip)]
      pub tags: umbral::orm::M2M<Tag>,
  }

  fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
      pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
  }
  // Multi-value submission: the form layer collapses repeated keys into
  // a comma-joined string under the m2m_<field> convention. Here we feed
  // the joined form directly.
  fn data_multi(title: &str, tag_ids: &[&str]) -> HashMap<String, String> {
      let mut m = HashMap::new();
      m.insert("title".to_string(), title.to_string());
      m.insert("tags".to_string(), tag_ids.join(","));
      m
  }

  static BOOT: OnceCell<()> = OnceCell::const_new();
  async fn boot() {
      BOOT.get_or_init(|| async {
          let settings = umbral::Settings::from_env().expect("figment defaults");
          let pool = db::connect_sqlite("sqlite::memory:").await.expect("sqlite");
          umbral::App::builder().settings(settings).database("default", pool.clone())
              .model::<Tag>().model::<Article>().build().expect("App::build");
          sqlx::query("CREATE TABLE fm_tag (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)")
              .execute(&pool).await.expect("create tag");
          sqlx::query("CREATE TABLE fm_article (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)")
              .execute(&pool).await.expect("create article");
          // Junction follows the <parent_table>_<field> convention.
          sqlx::query("CREATE TABLE fm_article_tags (parent_id INTEGER NOT NULL, child_id INTEGER NOT NULL, PRIMARY KEY (parent_id, child_id))")
              .execute(&pool).await.expect("create junction");
          for name in ["a", "b", "c"] {
              sqlx::query("INSERT INTO fm_tag (name) VALUES (?)").bind(name)
                  .execute(&pool).await.expect("seed tag");
          }
      }).await;
  }

  async fn junction_child_ids(parent_id: i64) -> Vec<i64> {
      let pool = db::pool();
      let rows: Vec<(i64,)> = sqlx::query_as("SELECT child_id FROM fm_article_tags WHERE parent_id = ? ORDER BY child_id")
          .bind(parent_id).fetch_all(&pool).await.expect("read junction");
      rows.into_iter().map(|(c,)| c).collect()
  }

  #[tokio::test]
  async fn m2m_form_writes_exactly_the_selected_junction_rows() {
      boot().await;
      let article = Article::validate(&data_multi("Intro", &["1", "2"]))
          .await.expect("valid m2m");
      let created = Article::objects().create(article).await.expect("create article");
      let ids = junction_child_ids(created.id).await;
      assert_eq!(ids, vec![1, 2], "exactly the two selected tags are junction rows (3rd absent)");
  }

  #[tokio::test]
  async fn m2m_form_bad_id_writes_zero_junction_rows() {
      boot().await;
      let before_articles = Article::objects().count().await.expect("count");
      let err = Article::validate(&data_multi("Broken", &["1", "9999"]))
          .await.expect_err("bad id rejected");
      assert!(err.fields.contains_key("tags"), "error keyed to the m2m field");
      // Validation failed before any insert — no article, no junction rows.
      let after_articles = Article::objects().count().await.expect("count");
      assert_eq!(before_articles, after_articles, "no parent row inserted");
  }
  ```
- [ ] Run, expect FAIL (the `tags: M2M<Tag>` field is rejected by the Form derive):
  ```bash
  cd crates && cargo test -p umbral-core --test form_m2m m2m_form_writes_exactly_the_selected_junction_rows
  ```
  Expect: `error: umbral::Form derive: unsupported field type ...` at the `tags` field.
- [ ] Implement the pending slot on `M2M<T,P>` in `crates/umbral-core/src/orm/m2m.rs`. Add to the struct (~82):
  ```rust
      /// Child PKs submitted through a form, awaiting the post-insert
      /// junction write. Drained by `take_pending_ids` in the typed
      /// create() path. Empty for hydrated/loaded rows.
      pending: Vec<sea_query::Value>,
  ```
  Add `pending: Vec::new()` to `M2M::empty()` (~112). Add accessors after `set_junction_table` (~148):
  ```rust
      /// Stage child PKs to be written as junction rows after the
      /// parent insert. Called by the Form derive's validate().
      pub fn set_pending_ids(&mut self, ids: Vec<sea_query::Value>) {
          self.pending = ids;
      }

      /// Drain the staged child PKs (post-insert junction write).
      pub fn take_pending_ids(&mut self) -> Vec<sea_query::Value> {
          std::mem::take(&mut self.pending)
      }
  ```
  Confirm `Default` for `M2M` (~103) still works — it calls `empty()` which now sets `pending: Vec::new()`.
- [ ] Add the `write_pending_m2m` hook to `HydrateRelated` in `crates/umbral-core/src/orm/model.rs` (~83, after `set_m2m_parent_ids`):
  ```rust
      /// Flush form-staged M2M selections to their junction tables
      /// after the parent row was inserted. The macro emits a body that
      /// walks this model's M2M fields, reads `parent_id` +
      /// `junction_table` (seeded by `set_m2m_parent_ids`) and the
      /// pending child ids, and calls `set_junction_dynamic`. Default:
      /// no-op for models with no M2M fields.
      ///
      /// Async + boxed because junction writes hit the DB; kept off the
      /// hot decode path (only `create()` calls it).
      fn write_pending_m2m<'a>(
          &'a mut self,
      ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), crate::orm::write::WriteError>> + Send + 'a>>
      {
          Box::pin(async { Ok(()) })
      }
  ```
  (A hand-rolled boxed-future default keeps `HydrateRelated` object-safe-compatible with its existing non-async methods without pulling `#[async_trait]` onto the whole trait.)
- [ ] Call the hook in typed `create()` in `crates/umbral-core/src/orm/queryset/mod.rs`. After each `row.set_m2m_parent_ids();` (the SQLite arm ~3064 and the Postgres arm ~3093), add the flush before `Ok(row)`:
  ```rust
                  row.set_m2m_parent_ids();
                  row.write_pending_m2m().await?;
                  Ok(row)
  ```
  Apply to both arms.
- [ ] Add `InputKind::ModelMultiChoice` + `Field::model_multi_choice` in `crates/umbral-core/src/forms.rs`. Add to `InputKind`:
  ```rust
      /// M2M relation. Submits a list of child ids; written as junction
      /// rows after the parent insert.
      ModelMultiChoice {
          target_table: &'static str,
          label_field: Option<&'static str>,
          pk_kind: PkKind,
      },
  ```
  Add an `InputKind::ModelMultiChoice { .. } => "text"` arm to `html_type`. Add the ctor after `Field::model_choice`:
  ```rust
      /// New multi-select M2M field.
      pub fn model_multi_choice(
          name: impl Into<String>,
          target_table: &'static str,
          label_field: Option<&'static str>,
          pk_kind: PkKind,
      ) -> Self {
          Self {
              name: name.into(),
              kind: InputKind::ModelMultiChoice { target_table, label_field, pk_kind },
              required: false,
              validators: Vec::new(),
              options: Vec::new(),
          }
      }
  ```
  The `render_html_async` `ModelMultiChoice` arm is already wired in Task 6.
- [ ] Add the multi-id parse + per-id existence helper to `crates/umbral-core/src/orm/forms_runtime.rs`:
  ```rust
  /// Split a submitted M2M value into ids. The form layer joins
  /// repeated keys with `,`; we also accept whitespace. Empty pieces
  /// are dropped.
  pub fn parse_multi_ids(raw: &str) -> Vec<String> {
      raw.split([',', ' ', '\n'])
          .map(|s| s.trim())
          .filter(|s| !s.is_empty())
          .map(|s| s.to_string())
          .collect()
  }

  /// Verify every id in `ids` exists in `target_table`; on any miss,
  /// push a single field-keyed error. Returns the parsed sea_query
  /// PK values for the ids that exist (used to stage the pending
  /// junction write). When any id is missing the caller treats the
  /// whole submission as invalid (atomicity) — errs is non-empty so
  /// the create never runs.
  pub async fn validate_multi_fk_exists(
      field: &str,
      ids: &[String],
      target_table: &str,
      errs: &mut ValidationErrors,
  ) -> Vec<sea_query::Value> {
      // Empty / optional M2M submitted nothing → no DB hit at all.
      if ids.is_empty() {
          return Vec::new();
      }
      let Some(meta) = crate::migrate::registered_models()
          .into_iter()
          .find(|m| m.table == target_table)
      else {
          return Vec::new();
      };
      let Some(pk_col) = meta.pk_column().map(|c| c.name.clone()) else {
          return Vec::new();
      };
      // ONE batched query — `SELECT <pk> FROM <target> WHERE <pk> IN (...)`.
      // NOT one count() per id: a list of M selected ids costs a single
      // round-trip, never M (no N+1). The set-difference below finds the
      // missing ids. Mirrors the batched M2M hydration the ORM already
      // uses (dynamic.rs `fetch_as_json` / `filter_m2m_contains_any`).
      let rows = crate::orm::dynamic::DynQuerySet::for_meta(&meta)
          .select_cols(&[pk_col.clone()])
          .filter_in_strings(&pk_col, ids)
          .fetch_as_json()
          .await
          .unwrap_or_default();
      let found: std::collections::HashSet<String> = rows
          .into_iter()
          .filter_map(|r| r.get(&pk_col).map(json_scalar_to_string))
          .collect();
      let mut out = Vec::with_capacity(ids.len());
      for id in ids {
          if found.contains(id) {
              if let Ok(n) = id.parse::<i64>() {
                  out.push(sea_query::Value::BigInt(Some(n)));
              }
          } else {
              errs.add(field, format!("{field}: id {id} has no matching record"));
          }
      }
      out
  }
  ```
  **Why batched:** `filter_in_strings` (dynamic.rs:412) coerces each value to the column `SqlType` and emits a single `IN (...)` predicate; `json_scalar_to_string` is the helper defined in Task 6. This keeps M2M validation at one query regardless of how many children are selected — the no-N+1 guarantee the audit requires.
  Confirm `sea_query::Value::BigInt(Some(i64))` is the correct shape (it matches `set_junction_dynamic`'s child-id values; the v1 M2M i64 constraint applies — non-i64 isn't supported, consistent with the spec's "follows the existing M2M constraint").
- [ ] Implement the macro M2M arm in `expand_form`. Detection reuses `m2m_inner` (and `Option<M2M<T>>` like the Model derive). Add, below the FK arm and above `classify_form_field_type`:
  ```rust
          // M2M<T> → ModelMultiChoice. No parent column; ids parsed and
          // staged on the M2M field's pending slot for the post-insert
          // junction write.
          let m2m_target: Option<syn::Type> =
              m2m_inner(&field.ty).cloned()
                  .or_else(|| option_inner_type(&field.ty).and_then(|i| m2m_inner(i).cloned()));
          if let Some(target_ty) = m2m_target {
              let label_field_tokens = match &attrs.label_field {
                  Some(lf) => quote!(::core::option::Option::Some(#lf)),
                  None => quote!(::core::option::Option::None),
              };
              let field_var = format_ident!("_{}_field", field_ident);
              field_builders.push(quote! {
                  let #field_var: ::umbral::forms::Field = ::umbral::forms::Field::model_multi_choice(
                      #field_name,
                      <#target_ty as ::umbral::orm::Model>::TABLE,
                      #label_field_tokens,
                      ::umbral::orm::forms_runtime::pk_kind_for_table(
                          <#target_ty as ::umbral::orm::Model>::TABLE,
                      ),
                  );
              });
              let raw_var = format_ident!("_{}_raw", field_ident);
              let ids_var = format_ident!("_{}_ids", field_ident);
              let pending_var = format_ident!("_{}_pending", field_ident);
              let parsed_var = format_ident!("_{}_parsed", field_ident);
              validate_body.push(quote! {
                  let #raw_var: ::std::string::String =
                      data.get(#field_name).cloned().unwrap_or_default();
                  let #ids_var = ::umbral::orm::forms_runtime::parse_multi_ids(&#raw_var);
                  let #pending_var = ::umbral::orm::forms_runtime::validate_multi_fk_exists(
                      #field_name,
                      &#ids_var,
                      <#target_ty as ::umbral::orm::Model>::TABLE,
                      &mut errs,
                  ).await;
                  // Build the M2M field with its pending ids staged.
                  let mut #parsed_var: #target_field_ty = ::core::default::Default::default();
                  #parsed_var.set_pending_ids(#pending_var);
              });
              struct_inits.push(quote! { #field_ident: #parsed_var });
              continue;
          }
  ```
  where `#target_field_ty` is the field's own declared type (`M2M<Tag>` or `Option<M2M<Tag>>`). Bind it just before the push:
  ```rust
          let target_field_ty = &field.ty;
  ```
  (Place the `let target_field_ty = &field.ty;` at the top of the per-field loop body so it's in scope.) For an `Option<M2M<T>>` field, `Default::default()` yields `None`, and `set_pending_ids` won't exist on `Option` — so restrict the M2M *form* arm to the non-Option shape: gate `if let Some(target_ty) = m2m_inner(&field.ty).cloned()` only (drop the `Option<M2M>` branch in the form arm). A form-submittable M2M is always the bare `M2M<T>` shape; `Option<M2M<T>>` on a Model is a Model-side ergonomic, not a form field. Document this in a `// ` comment on the arm.
- [ ] Emit the `write_pending_m2m` body on the Model derive's `HydrateRelated` impl. In `crates/umbral-macros/src/lib.rs`, locate where `set_m2m_parent_ids` arms are collected (`m2m_parent_arms`, ~895) and the `HydrateRelated` impl emission (~1781-1810). Collect a parallel `write_pending_m2m_arms` for each M2M field (the macro already has `(field_name_str, inner_ty)` pairs for M2M at ~1327). For each M2M field push:
  ```rust
  write_pending_m2m_arms.push(quote! {
      {
          let pending = self.#field_ident.take_pending_ids();
          if !pending.is_empty() {
              if let (Some(parent_id), Some(junction)) =
                  (self.#field_ident.parent_id().copied(), self.#field_ident.junction_table())
              {
                  ::umbral::orm::m2m::set_junction_dynamic(
                      junction,
                      ::sea_query::Value::BigInt(::core::option::Option::Some(parent_id)),
                      pending,
                  )
                  .await
                  .map_err(::umbral::orm::write::WriteError::Sqlx)?;
              }
          }
      }
  });
  ```
  Then emit the override inside the `impl HydrateRelated` block (only when `write_pending_m2m_arms` is non-empty):
  ```rust
      fn write_pending_m2m<'a>(
          &'a mut self,
      ) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = ::core::result::Result<(), ::umbral::orm::write::WriteError>> + ::core::marker::Send + 'a>>
      {
          ::std::boxed::Box::pin(async move {
              #(#write_pending_m2m_arms)*
              ::core::result::Result::Ok(())
          })
      }
  ```
  Confirm `parent_id()` returns `Option<&P>` where `P = i64`; `.copied()` yields `Option<i64>`. The `set_junction_dynamic` and `M2M` accessors are `pub` (verified). Re-export `set_junction_dynamic` path as `umbral::orm::m2m::set_junction_dynamic` (it's `pub` in `orm::m2m`).
- [ ] Run, expect PASS:
  ```bash
  cd crates && cargo test -p umbral-core --test form_m2m
  ```
- [ ] Gate + commit:
  ```bash
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  git add -A && git commit -m "$(cat <<'EOF'
feat(forms): M2M fields become a ModelMultiChoice + post-insert junction write

The Form derive parses the submitted id list, verifies each id exists,
and stages the validated ids on the M2M field's pending slot. The typed
create() path, after seeding parent_id/junction_table, flushes them to
the <parent>_<field> junction table via the existing
set_junction_dynamic machinery. A single bad id fails validation before
any insert — zero parent and zero junction rows (atomicity).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
  ```

---

## Task 8 — End-to-end: derive `Form` on `PluginComment`, delete hand-rolled `Default` (spec acceptance case)

The primary acceptance case: `PluginComment` (FK + `Option<FK>` + choices + reverse-skip, no M2M) compiles with `#[derive(umbral::forms::Form)]`, drops its hand-rolled `Default`, and a behavioral submit test confirms a comment saves with the FK bound correctly.

**Files:**
- Modify: `umbral_website/plugins/plugin_directory/src/models.rs` (`PluginComment` ~347-452)
- Test: a behavioral submit test colocated with the website plugin crate, or `crates/umbral-core/tests/form_fk.rs` if the website crate has no test harness. (Prefer the website crate; check `umbral_website/plugins/plugin_directory/` for an existing `tests/` dir or `#[cfg(test)]` module.)

Note: `umbral_website` is a standalone Cargo project outside the framework workspace. Its build/test runs from `umbral_website/`, not `crates/`. The framework-side behavior is already proven by Tasks 3-5 (FK + choices through a real DB); this task proves the real consumer compiles and submits.

Steps:

- [ ] Write the failing test. In `umbral_website/plugins/plugin_directory/src/models.rs`, add a `#[cfg(test)] mod tests` (or a `tests/` integration file) that boots a SQLite pool, seeds a `Plugin`, submits a `PluginComment` form, and reads the FK back:
  ```rust
  #[cfg(test)]
  mod form_tests {
      use super::*;
      use umbral::forms::FormValidate;
      use umbral::orm::Model;
      use std::collections::HashMap;

      fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
          pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
      }

      #[tokio::test]
      async fn plugin_comment_form_submits_with_fk_and_choices() {
          let pool = umbral_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
          umbral::App::builder()
              .settings(umbral::Settings::from_env().unwrap())
              .database("default", pool.clone())
              .model::<Plugin>()
              .model::<PluginComment>()
              .build()
              .unwrap();
          // Minimal tables for the two models (test-only DDL).
          sqlx::query("CREATE TABLE plugin (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, slug TEXT, crate_name TEXT, author TEXT, short_description TEXT, full_content TEXT, installation_commands TEXT)")
              .execute(&pool).await.unwrap();
          sqlx::query("CREATE TABLE plugin_comment (id INTEGER PRIMARY KEY AUTOINCREMENT, plugin INTEGER NOT NULL REFERENCES plugin(id), author INTEGER, body TEXT NOT NULL, kind TEXT NOT NULL, moderation TEXT NOT NULL, pinned BOOLEAN NOT NULL DEFAULT 0, author_label TEXT, parent INTEGER, plugin_version TEXT, umbral_version TEXT, database_backend TEXT, operating_system TEXT, created_at TEXT, updated_at TEXT, deleted_at TEXT)")
              .execute(&pool).await.unwrap();
          sqlx::query("INSERT INTO plugin (id, name) VALUES (1, 'demo')").execute(&pool).await.unwrap();

          let comment = PluginComment::validate(&data(&[
              ("plugin", "1"),
              ("body", "Great plugin, works on sqlite."),
              ("kind", "usage_note"),
          ])).await.expect("comment validates with FK + choices");
          assert_eq!(comment.plugin.id(), 1);
          assert_eq!(comment.kind, CommentKind::UsageNote);
      }
  }
  ```
  (Adjust the test DDL columns to whatever the real migration produces; the point is FK `plugin` is bound as INTEGER and `kind` decodes to the enum. Add `tokio` + `sqlx` to the plugin crate's `[dev-dependencies]` if absent — check `umbral_website/plugins/plugin_directory/Cargo.toml`.)
- [ ] Run, expect FAIL (the derive is commented out; `PluginComment::validate` doesn't exist):
  ```bash
  cd umbral_website && cargo test -p plugin_directory plugin_comment_form_submits_with_fk_and_choices
  ```
  Expect: `error[E0599]: no function or associated item named 'validate' found for struct 'PluginComment'`.
- [ ] Implement. In `umbral_website/plugins/plugin_directory/src/models.rs`:
  - Replace the commented derive line and the active one (~347-348) with the single enabled derive:
    ```rust
    #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model, umbral::forms::Form)]
    ```
  - Restore the `#[form(...)]` attrs on the user-submittable fields (uncomment): `body` (`#[form(required, length(min = 5, max = 5_000))]`), `plugin_version` / `umbral_version` / `database_backend` / `operating_system` (`#[form(optional, length(max = 40))]`).
  - Leave `#[umbral(on_delete = "cascade")] pub plugin: ForeignKey<Plugin>` WITHOUT `#[umbral(noform)]` — it's now a `ModelChoice` form field (the FK the test submits). Keep `author` as `#[umbral(noform, on_delete = "set_null")]` (handler fills it from auth context), and `parent` as `#[umbral(on_delete = "set_null")]` — it's `Option<ForeignKey<PluginComment>>`, a nullable `ModelChoice`; if the public form shouldn't expose it, add `#[umbral(noform)]` to `parent`.
  - For `kind` and `moderation`: `kind` becomes a public `Select` (uncomment to `#[umbral(choices, default = "general")]`). `moderation` should stay server-managed → `#[umbral(noform, choices, default = "pending")]` (the public form must not let a submitter pick their own moderation status).
  - Delete the hand-rolled `impl Default for PluginComment { ... }` block (~431-452) and its preceding explanatory comment (~422-430). The Form macro's `..Default::default()` tail now needs `Default` — but `ForeignKey` has no `Default`. Since `plugin` is now a form field (not skipped), and `author`/`parent`/timestamps/`deleted_at` ARE skipped, the struct still needs `Default` for the tail. Resolve this: the skipped `author: Option<ForeignKey<AuthUser>>` and `parent: Option<ForeignKey<PluginComment>>` are `Option`, which IS `Default` (→ `None`); the only non-Default skipped fields are the timestamps and `deleted_at` (all `Default`). With `plugin` no longer skipped, no bare `ForeignKey` field remains in the `..Default::default()` set — so `#[derive(Default)]` becomes derivable IF every remaining field is `Default`. Add `Default` to the derive list and delete the manual impl:
    ```rust
    #[derive(Debug, Clone, Default, sqlx::FromRow, Serialize, Deserialize, Model, umbral::forms::Form)]
    ```
    Verify by build: if any skipped field still isn't `Default`, the derive errors and names it — fix by making that field skipped-and-Option or marking it a form field.
  - Delete the `TODO: ENABLE FORM HERE ...` comment (~346).
- [ ] Run, expect PASS:
  ```bash
  cd umbral_website && cargo test -p plugin_directory plugin_comment_form_submits_with_fk_and_choices
  ```
- [ ] Build the whole website to confirm no downstream breakage (handlers that constructed `PluginComment::default()` or referenced the old form-commented fields):
  ```bash
  cd umbral_website && cargo build && cargo test
  ```
  Fix any consumer that relied on the deleted manual `Default` or the old field attrs.
- [ ] Framework gate (the macro + core changes), then commit:
  ```bash
  cd crates && cargo fmt && cargo clippy --all-targets && cargo build && cargo test
  cd ../umbral_website && cargo fmt && cargo build
  git add -A && git commit -m "$(cat <<'EOF'
feat(website): derive Form on PluginComment, delete hand-rolled Default

The Form derive now handles FK (ModelChoice), choices (Select), and
auto-skips reverse relations, so PluginComment's public submission form
works directly off the Model struct. plugin is a ModelChoice; kind a
Select; author/moderation stay server-managed. The hand-rolled Default
boilerplate is gone — every remaining field is Default-derivable.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
  ```

---

## Docs (ship a feature, ship its doc page)

- [ ] Add `documentation/docs/v0.0.1/orm/forms-relations.mdx`: FK / forward-O2O (`ModelChoice`), M2M multi-select (`ModelMultiChoice`), choices (`Select`) in `#[derive(Form)]`; the async render path; reverse relations auto-skipped. One minimal example per kind (the `Book`/`Author` FK form, the `Article`/`Tag` M2M form, the `ChoiceForm`). Link to this plan + `docs/superpowers/specs/2026-06-11-orm-relations-forms-and-joins-design.md` Part 1/2. Frontmatter: `title`, `description`, `sidebar_position`, `tab_group: orm`. Commit `docs(orm): forms learn relations page`.

---

## Spec coverage check (Part 1 + Part 2)

| Spec sub-point | Task |
|---|---|
| 1a — reverse relations auto-skip (`ReverseSet` + reverse `OneToOne`) | Task 2 |
| 1b — choices enum → `Select` (compile-time options, nullable empty option) | Task 3 |
| 1c — `ForeignKey<T>` / forward `OneToOne<T>` → `ModelChoice` (label field, pk_kind) | Tasks 4 (parse) + 5 (existence) + 6 (render options) |
| 1d — `M2M<T>` → `ModelMultiChoice` + post-insert junction write (reuse `set_junction_dynamic`) | Task 7 |
| Part 2 — `FormValidate` async (`#[async_trait]`, ambient pool, FK existence via ORM, choices pure, `Form<T>` extractor awaits) | Task 1 (async-ify) + Task 5 (existence) + Task 6 (render) |
| Acceptance — `PluginComment` derives Form, deletes hand-rolled `Default`, behavioral submit | Task 8 |
