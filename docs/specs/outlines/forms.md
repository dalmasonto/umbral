# Outline — Forms

| | |
|---|---|
| **Status** | Outline. Promotes at M9–M12 entry when admin or non-API web flows need it. |
| **Maps to milestone** | M9–M12 |
| **Companions** | `02-plugin-contract.md`, `04-orm-model-and-fields.md`, outlines `web-layer.md`, `templates.md`, `security-defaults.md`, `rest.md`, `static-and-media.md`, `arch.md §6.3` |

## Purpose

Forms own the **HTML form pipeline** end-to-end: render an HTML `<form>`, accept a `POST` body that arrived as `application/x-www-form-urlencoded` or `multipart/form-data`, parse it into a typed struct, validate it (field-level and form-level), display errors back as HTML next to the offending field, and — for the `ModelForm` case — save the result through the ORM. This is the substrate the admin renders its change pages on, and the substrate any non-API web view uses for user input. The spec-set design audit flagged the **single most important point about this outline**: forms are *not* REST serializers. Both go struct ↔ payload, both validate, both share a validator catalog — and the temptation to collapse them into one abstraction is precisely what this outline rejects. Serializers handle JSON over the wire and emit structured machine-readable error envelopes; forms handle HTML form encoding and emit field-bound errors meant to be rendered next to an `<input>`. Their content types, their parsers, their renderers, and their error shapes are all different. Keeping them as parallel-but-distinct subsystems (`umbral-rest` versus the forms surface owned here) is the structural proof that "serializers are a plugin" extends to "forms are their own thing too" — and means a REST-free app pays nothing for serializers while an HTML-only app pays nothing for the REST plugin.

## Key concepts

**Declarative `Form` derive.** A form is a struct annotated with `#[derive(Form)]`. The derive expands to (a) a parser from URL-encoded / multipart bodies into the struct, (b) per-field rendering through widgets, (c) field-level and form-level validation, and (d) an error type whose errors are attached to the field that raised them (or to the form as a whole for cross-field rules). The hand-written shape lands first; the derive mechanises it — the M2 → M3 progression `04-orm-model-and-fields.md` uses for `Model`.

```rust
#[derive(Form)]
pub struct ContactForm {
    #[umbral(max_length = 200)] pub subject: String,
    #[umbral(widget = Textarea)] pub body: String,
    pub email: EmailField,
}
```

**`ModelForm`.** A form bound to a `Model`. It auto-derives its field set from the model's `FIELDS` (with `include` / `exclude` knobs), populates initial values from a `Post` instance for an edit page, and persists via `Model::create` (new) or `Model::save` (existing) on `is_valid() == true`. The bridge is structural, not magical: the `ModelForm` derive walks `T::FIELDS` and emits the matching form fields with the matching widgets.

```rust
async fn edit_post(Path(id): Path<i64>, form: Form<PostForm>) -> Result<Response> {
    if form.is_valid() { form.save().await?; return Ok(redirect("/posts")); }
    render("posts/edit.html", &form)   // errors are bound to fields
}
```

**Field types — distinct from ORM field types.** A form's `TextField`, `EmailField`, `FileField`, `ChoiceField`, `DateField` look superficially like ORM fields but exist for a different reason: they carry a *widget*, a parser for the wire form encoding, and HTML-shaped validation messages. `EmailField` in the ORM is a column-level annotation; `EmailField` in a form is "render an `<input type=email>`, parse a string, validate as email." The two share the underlying validator from the catalog (the `validator` crate), not the field type.

**Widgets.** Widgets are the render-side decision: `TextInput`, `Textarea`, `Select`, `CheckboxInput`, `RadioSelect`, `FileInput`, `DateInput`, `Hidden`. Each widget knows how to render itself as HTML (delegated to the templating substrate from `templates.md`) and how to read the form-encoded value back. A field has a default widget; overriding it on the struct field swaps the render without changing the field type. HTML-only widgets — a CAPTCHA, a honeypot — are fields whose parsed value never reaches the saved model; they exist only to participate in validation.

**Validation.** Per-field validation runs first (length, regex, format, choices), then a form-level `clean(&mut self) -> Result<(), FormError>` hook runs for cross-field rules ("password matches confirmation"). Validators draw from the **shared catalog** with `#[umbral(validators(...))]` in `04-orm-model-and-fields.md` — the same `validator` crate functions, called by name. Validation failures don't throw: they accumulate into the form's error map keyed by field, plus a `non_field_errors` bucket for form-level failures.

**Error display.** Errors live as a map attached to the form: `form.errors().on("email")` returns `Vec<String>` for that field, `form.errors().non_field()` returns the form-level ones. Templates iterate this to render `<span class="error">` next to inputs. This is deliberately different from `rest.md`'s JSON envelope — HTML consumers want the errors threaded through the markup, not serialised separately.

**Formsets and inline formsets.** A `Formset<F>` is `N` instances of a form submitted together (e.g. "edit 5 line items at once"), with management metadata (`TOTAL_FORMS`, `INITIAL_FORMS`) carried as hidden inputs. An `InlineFormset<Parent, Child>` is the FK-aware variant: edit a `Post` and its `Comment` rows in one submission, save them as one transaction, route validation errors back to the right child form. The admin's inline editing is the canonical user.

## Promote-to-deep trigger

Promote at M9–M12 entry — the milestone where `umbral-admin` lands or the first non-API web flow (a login page, a contact form, a generic-view-backed CRUD page) needs to render and process an HTML form. The deep spec pins the derive shape, the widget render contract against `templates.md`, the `ModelForm` save semantics, and the formset management-form encoding.

## Open questions

- **`#[derive(Form)]` attribute shape.** Per-field options (`widget = Textarea`, `label = "..."`, `help_text = "..."`, `initial = "..."`) need a parser story that mirrors `04-orm-model-and-fields.md`'s `#[umbral(...)]` group without colliding with ORM attributes when both apply to a `ModelForm` struct.
- **Widget render mechanism.** Widgets render HTML; HTML is produced by the templating substrate. Open: whether widgets ship as individual template files the engine loads (the template-per-widget path), or as Rust functions returning marked-safe strings, or both. Cross-link to `templates.md` for the render seam; cross-link to `security-defaults.md` for autoescape behavior on widget output.
- **Shared validator catalog.** The validators named in `#[umbral(validators(...))]` on a model and the validators named on a form field must be the same set so a model-level rule and a form-level rule never disagree. Open: how to surface the catalog so both `04-orm-model-and-fields.md` and this outline consume one named set, and how custom user-defined validators register into it.
- **CSRF token handling.** Forms render a CSRF hidden input on every `POST` form by default and validate it on submit. The hook lives in the `Form<T>` extractor (parse-time) and the rendering helper (render-time). Open question carried from `web-layer.md` open #5 and owned long-term by `security-defaults.md`.
- **Multipart and file uploads.** A form with a `FileField` parses `multipart/form-data` via the `Multipart` extractor from `web-layer.md` and stores the file via the storage backend in `static-and-media.md`. Open: where the per-part size cap is enforced (form layer vs storage layer) and how a failed upload surfaces as a field-bound error rather than a 400.

## Cross-links

- Deep specs that constrain this: `04-orm-model-and-fields.md` (`Model`, `FIELDS`, the shared validator surface a `ModelForm` reads), `02-plugin-contract.md` (forms ship from the same plugin contract every built-in uses — the admin plugin is the primary consumer).
- Sibling outlines: `templates.md` (widget HTML rendering, autoescape), `security-defaults.md` (CSRF token, signed inputs), `rest.md` (the **explicitly-separate** REST serializer story — same validator catalog, different content type, different error envelope), `static-and-media.md` (file upload storage for `FileField` widgets), `web-layer.md` (`Form<T>` extractor, multipart parsing, the request shape forms submit against).
- `arch.md §6.3` — the `umbral-admin` description, where the admin's auto CRUD UI is built on top of `ModelForm`.
