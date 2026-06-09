//! Form parsing, validation, and HTML rendering.
//!
//! ## Two names, two layers (gaps2 #19)
//!
//! - **`FormValidate` trait** (the primitive, was `Form`): a struct
//!   implements this to provide a `validate(&HashMap)` method. The
//!   `#[derive(Form)]` macro emits it.
//! - **`Form<T>` extractor** (the axum entry point): wraps the
//!   parsed-and-validated `T` in a `Result<T, FormErrors>`. Use in
//!   handler signatures: `Form<ContactForm>`.
//!
//! The trait used to be called `Form` too, but that collided with
//! the extractor type in the same module. The name with generics
//! went to the extractor (matches `axum::extract::Form<T>` /
//! `axum::Json<T>` shape) and the trait got the more descriptive
//! `FormValidate`.
//!
//! The piece that fills out the request-handling story between axum's
//! `Form<T>` extractor (raw key/value access) and a typed Rust struct
//! (the application's view of validated input). Django's
//! `forms.Form` and `forms.ModelForm` are the closest reference;
//! umbra's first cut is the primitive layer those abstractions sit
//! on top of.
//!
//! ## v1 shape
//!
//! - [`Field`] types per HTML input shape (`TextField`,
//!   `IntegerField`, `EmailField`, `PasswordField`, `BooleanField`,
//!   `DateField`, `TimeField`).
//! - Field-level validators ([`Required`], [`MinLength`],
//!   [`MaxLength`], [`Pattern`]) plus the convenience built-in checks
//!   each field type does for its own shape (e.g. `EmailField` runs
//!   `Pattern` against an email regex by default).
//! - [`ValidationErrors`] is a map of field-name -> error messages.
//!   Forms accumulate every per-field error before returning, so the
//!   user sees the whole form's problems at once, the same way Django
//!   does.
//! - HTML rendering: every field type has [`Field::render_html`]
//!   that emits a single `<input>` (or `<textarea>`) with the right
//!   `type`, `name`, `value`, and a `required` attribute when the
//!   field is required.
//!
//! ## v1 caps
//!
//! - No `#[derive(Form)]` macro. Users compose forms by hand:
//!   `LoginForm::validate(&form_data)` is a function that reads each
//!   field, accumulates errors, returns either the typed struct or
//!   `Err(ValidationErrors)`. The derive lands as a future round.
//! - No file uploads (multipart); HTML-only.
//! - No localized error messages.

use std::collections::HashMap;

// =========================================================================
// Form trait. The `#[derive(Form)]` macro emits an impl of this. User
// code can also impl it by hand for the rare "I want different
// semantics than the macro" case.
// =========================================================================

/// The contract a typed form satisfies. `validate` reads form data
/// (a `HashMap<String, String>`, the natural shape after
/// `serde_urlencoded` or axum's `Form` extractor) and produces either
/// the typed struct or a `ValidationErrors` map describing every
/// problem at once.
///
/// `render_html` writes the form's HTML inputs, prefilled from a
/// HashMap on the re-render path (after a validation failure or on
/// edit views). The default impl walks `fields()` and concatenates
/// each field's `render_html` — most macro-derived forms inherit
/// this and only override when they need custom layout.
pub trait FormValidate: Sized {
    /// Parse and validate the form's input. Returns the typed struct
    /// on success; returns `ValidationErrors` with every field's
    /// problems accumulated on failure.
    fn validate(data: &HashMap<String, String>) -> Result<Self, ValidationErrors>;

    /// The field declarations this form carries. Used by the default
    /// `render_html` to walk them in declaration order. The macro
    /// emits one entry per struct field.
    fn fields() -> Vec<Field>;

    /// Render every field as an HTML `<label>` + `<input>` pair,
    /// prefilled from `data`. Wraps each in a `<div class="field">`
    /// for styling. Override if you want a non-default layout.
    fn render_html(data: &HashMap<String, String>) -> String {
        let mut out = String::new();
        for field in Self::fields() {
            let value = data.get(&field.name).map(String::as_str).unwrap_or("");
            out.push_str("<div class=\"field\">");
            out.push_str(&format!(
                "<label for=\"{name}\">{name}</label>",
                name = field.name
            ));
            out.push_str(&field.render_html(value));
            out.push_str("</div>");
        }
        out
    }
}

// =========================================================================
// Errors. One per-field message list, plus a "non-field" bucket for
// cross-field issues (passwords don't match, etc.).
// =========================================================================

/// A collection of per-field validation errors. Forms accumulate
/// these and return the whole map at once.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ValidationErrors {
    /// Per-field error messages. Each field's vec may carry multiple
    /// messages (e.g. both Required and Pattern fire).
    pub fields: HashMap<String, Vec<String>>,
    /// Cross-field errors that don't belong to one field. Use for
    /// "password and confirm don't match", etc.
    pub non_field: Vec<String>,
}

impl ValidationErrors {
    /// Construct an empty error set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an error to one field. Multiple calls accumulate.
    pub fn add(&mut self, field: &str, message: impl Into<String>) {
        self.fields
            .entry(field.to_string())
            .or_default()
            .push(message.into());
    }

    /// Add a cross-field error.
    pub fn add_non_field(&mut self, message: impl Into<String>) {
        self.non_field.push(message.into());
    }

    /// Has any error been recorded?
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.non_field.is_empty()
    }

    /// Convert to `Result<(), ValidationErrors>`, returning `Ok(())`
    /// when no errors have accumulated. Use as the last step in a
    /// form's `validate` method.
    pub fn into_result(self) -> Result<(), Self> {
        if self.is_empty() { Ok(()) } else { Err(self) }
    }
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for msg in &self.non_field {
            writeln!(f, "form: {msg}")?;
        }
        for (field, msgs) in &self.fields {
            for msg in msgs {
                writeln!(f, "{field}: {msg}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

// =========================================================================
// Validators. Reusable functions that take a value and either succeed
// or push an error onto the per-field list. Each is a small struct
// implementing `Validator` so users can build a Vec<Box<dyn ...>>.
// =========================================================================

/// One validator's verdict.
pub trait Validator: Send + Sync {
    /// Check the value. `field_name` is included for the error
    /// message. Return `Ok(())` to accept, `Err(message)` to reject.
    fn check(&self, field_name: &str, value: &str) -> Result<(), String>;
}

/// The field must not be empty.
pub struct Required;
impl Validator for Required {
    fn check(&self, field_name: &str, value: &str) -> Result<(), String> {
        if value.trim().is_empty() {
            Err(format!("{field_name} is required"))
        } else {
            Ok(())
        }
    }
}

/// The field's length (in characters) must be at least `n`.
pub struct MinLength(pub usize);
impl Validator for MinLength {
    fn check(&self, field_name: &str, value: &str) -> Result<(), String> {
        if value.chars().count() < self.0 {
            Err(format!(
                "{field_name} must be at least {} characters",
                self.0
            ))
        } else {
            Ok(())
        }
    }
}

/// The field's length (in characters) must be at most `n`.
pub struct MaxLength(pub usize);
impl Validator for MaxLength {
    fn check(&self, field_name: &str, value: &str) -> Result<(), String> {
        if value.chars().count() > self.0 {
            Err(format!(
                "{field_name} must be at most {} characters",
                self.0
            ))
        } else {
            Ok(())
        }
    }
}

/// A simple "must look like an email" check. Not RFC 5322 strict —
/// covers the 99% case (one `@`, non-empty local part, dot in the
/// domain). Users with stricter needs swap in a real regex.
pub struct EmailFormat;
impl Validator for EmailFormat {
    fn check(&self, field_name: &str, value: &str) -> Result<(), String> {
        let Some((local, domain)) = value.split_once('@') else {
            return Err(format!("{field_name} must contain `@`"));
        };
        if local.is_empty() {
            return Err(format!("{field_name} is missing a local part before `@`"));
        }
        if !domain.contains('.') {
            return Err(format!(
                "{field_name}'s domain must contain at least one `.`"
            ));
        }
        if domain.starts_with('.') || domain.ends_with('.') {
            return Err(format!("{field_name}'s domain is malformed"));
        }
        Ok(())
    }
}

/// Regex-pattern validator — the catch-all shape for "value must
/// match this format". Reject the field with a user-supplied message
/// when the pattern doesn't match.
///
/// Used by `#[form(regex = "...")]` on derived form structs AND by
/// the `Field::regex` / `Field::phone` / `Field::url` convenience
/// constructors. The pattern is parsed once at construction time
/// (panics if invalid — a hardcoded pattern can't go wrong in
/// production; user-supplied patterns are validated at `cargo build`
/// time through the macro's `Regex::new(...)` compile-time call).
pub struct RegexFormat {
    pattern: regex::Regex,
    message: String,
}

impl RegexFormat {
    /// Build a regex validator from a pattern + a human message. The
    /// pattern is compiled eagerly — use `regex::Regex::new` shape
    /// (no leading slash, no flags suffix). Panics on an invalid
    /// pattern; the derive macro catches this at build time by
    /// emitting the literal into the generated code.
    pub fn new(pattern: &str, message: impl Into<String>) -> Self {
        Self {
            pattern: regex::Regex::new(pattern)
                .unwrap_or_else(|e| panic!("RegexFormat: invalid pattern `{pattern}`: {e}")),
            message: message.into(),
        }
    }
}

impl Validator for RegexFormat {
    fn check(&self, field_name: &str, value: &str) -> Result<(), String> {
        if self.pattern.is_match(value) {
            Ok(())
        } else {
            // `{field}` placeholder in the message gets substituted
            // with the actual field name — lets one message template
            // be reused across forms ("{field} must start with `+`").
            // Most callers won't use the placeholder; substitution is
            // a no-op when it's absent.
            Err(self.message.replace("{field}", field_name))
        }
    }
}

/// E.164 international phone-number format — the standard the
/// telecoms industry uses. `+<country code><subscriber number>`
/// where the country code is 1-3 digits and the subscriber number
/// is up to 14 digits, no spaces or punctuation.
///
/// Catches the most common typo'd-phone cases ("07065" with no
/// country code, "+0..." starting with zero, letters mixed in,
/// dashes / spaces / parens that proper E.164 doesn't allow).
/// Users who need a softer "accept anything that looks vaguely
/// phone-ish" check can write their own regex via
/// `#[form(regex = "...", message = "...")]`.
pub const PHONE_E164_PATTERN: &str = r"^\+[1-9]\d{1,14}$";

/// URL validator — http(s) only, requires a host, accepts an
/// optional path/query/fragment. Conservative on purpose:
/// `ftp://`, `mailto:`, etc. get rejected so a form that promises
/// "URL" doesn't end up persisting a non-web scheme.
pub const URL_PATTERN: &str = r"^https?://[A-Za-z0-9._~:%/?#\[\]@!$&'()*+,;=-]+$";

// =========================================================================
// Field types. Each owns its name, value (after parsing), and a list
// of validators that fire in order. `render_html` emits the matching
// HTML input.
// =========================================================================

/// What HTML `<input type>` a field renders as. The form module
/// uses this for `render_html`; it's the same set the admin's
/// `input_kind` produces.
#[derive(Debug, Clone, Copy)]
pub enum InputKind {
    Text,
    Number,
    Email,
    /// gaps2 #19 follow-up — `<input type="tel">` so mobile
    /// browsers pop the number keypad. Phone fields don't get
    /// browser-side validation (there's no canonical phone format
    /// the browser knows about), so the server-side regex is what
    /// catches typo'd input.
    Tel,
    /// `<input type="url">`. Browser does shallow validation
    /// (requires a scheme + host) but the server-side regex is
    /// stricter about which schemes are allowed.
    Url,
    Password,
    Checkbox,
    Date,
    Time,
    DatetimeLocal,
    Textarea,
}

impl InputKind {
    fn html_type(self) -> &'static str {
        match self {
            InputKind::Text | InputKind::Textarea => "text",
            InputKind::Number => "number",
            InputKind::Email => "email",
            InputKind::Tel => "tel",
            InputKind::Url => "url",
            InputKind::Password => "password",
            InputKind::Checkbox => "checkbox",
            InputKind::Date => "date",
            InputKind::Time => "time",
            InputKind::DatetimeLocal => "datetime-local",
        }
    }
}

/// A single form field: name + kind + validators. The field doesn't
/// own its parsed value; `validate` reads from the form-data map and
/// pushes errors onto the accumulator.
pub struct Field {
    pub name: String,
    pub kind: InputKind,
    pub required: bool,
    pub validators: Vec<Box<dyn Validator>>,
}

impl Field {
    /// New text field. Caller adds validators via builder methods.
    pub fn text(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: InputKind::Text,
            required: true,
            validators: vec![Box::new(Required)],
        }
    }

    /// New email field. Carries `EmailFormat` by default.
    pub fn email(name: impl Into<String>) -> Self {
        let mut f = Self::text(name);
        f.kind = InputKind::Email;
        f.validators.push(Box::new(EmailFormat));
        f
    }

    /// Attach a regex-pattern validator to an existing field.
    /// Composes with `Required`, `MinLength`, `MaxLength`, etc. —
    /// the regex check fires after the others, so empty / missing
    /// values surface the right "is required" error rather than
    /// a confusing "doesn't match pattern" error.
    ///
    /// The pattern is compiled eagerly — an invalid regex panics at
    /// construction time. The derive macro short-circuits this by
    /// emitting the literal pattern, so a malformed
    /// `#[form(regex = "...")]` surfaces as a panic in tests rather
    /// than silently passing every input.
    ///
    /// Use `{field}` in the message to interpolate the field name.
    ///
    /// ```ignore
    /// let f = Field::text("invoice_id")
    ///     .regex(r"^INV-\d{6}$", "{field} must look like `INV-123456`");
    /// ```
    pub fn regex(mut self, pattern: &str, message: impl Into<String>) -> Self {
        self.validators
            .push(Box::new(RegexFormat::new(pattern, message)));
        self
    }

    /// New phone field — E.164 international format
    /// (`+<country><subscriber>`, e.g. `+14155551234`). Catches the
    /// common typo'd-phone cases ("07065", "+0…", letters mixed in,
    /// dashes / spaces / parens that proper E.164 doesn't allow).
    /// Renders as `<input type="tel">` so mobile browsers pop the
    /// number keypad.
    ///
    /// Soft-validation case ("accept anything phone-ish, even
    /// without country code"): use `Field::text` + your own
    /// `.regex(...)`. The strict E.164 pattern here is the right
    /// default because every form that asks for a phone number
    /// SHOULD be storing them in E.164 (the only shape that
    /// round-trips across providers / SMS gateways / address books).
    pub fn phone(name: impl Into<String>) -> Self {
        let mut f = Self::text(name);
        f.kind = InputKind::Tel;
        f.validators.push(Box::new(RegexFormat::new(
            PHONE_E164_PATTERN,
            "{field} must be E.164 format — `+` then country code then number, no spaces",
        )));
        f
    }

    /// New URL field — http(s) only, requires a host. Conservative:
    /// `ftp://` / `mailto:` / etc. get rejected so a form that
    /// promises "URL" doesn't persist a non-web scheme.
    pub fn url(name: impl Into<String>) -> Self {
        let mut f = Self::text(name);
        f.kind = InputKind::Url;
        f.validators.push(Box::new(RegexFormat::new(
            URL_PATTERN,
            "{field} must be an http(s):// URL",
        )));
        f
    }

    /// New password field. Identical validation rules to text; the
    /// difference is the rendered `<input type="password">` so the
    /// browser masks input.
    pub fn password(name: impl Into<String>) -> Self {
        let mut f = Self::text(name);
        f.kind = InputKind::Password;
        f
    }

    /// New integer field. Validates that the value parses as `i64`.
    pub fn integer(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: InputKind::Number,
            required: true,
            validators: vec![Box::new(Required), Box::new(IntegerFormat)],
        }
    }

    /// New floating-point field. Renders as `<input type="number">`
    /// (no `step` set; HTML's default accepts decimals). Validates
    /// only `Required`; the macro's parse step is what catches
    /// non-numeric input — the field-level validator would reject
    /// integer literals which is the wrong shape for an f64 field.
    pub fn float(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: InputKind::Number,
            required: true,
            validators: vec![Box::new(Required), Box::new(FloatFormat)],
        }
    }

    /// New boolean field. Required-by-default would be wrong here
    /// (HTML emits the field key only when the box is checked), so
    /// boolean fields skip `Required`.
    pub fn boolean(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: InputKind::Checkbox,
            required: false,
            validators: Vec::new(),
        }
    }

    /// Mark the field as optional. The `validate` method
    /// short-circuits when `required` is false and the value is
    /// empty, so the `Required` validator (if any) doesn't fire on
    /// an empty optional field. Validators that wrap non-empty
    /// values (`MinLength`, `Pattern`, ...) still run when there's
    /// something to check.
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    /// Add a `MinLength(n)` validator. Builder method, returns self.
    pub fn min_length(mut self, n: usize) -> Self {
        self.validators.push(Box::new(MinLength(n)));
        self
    }

    /// Add a `MaxLength(n)` validator. Builder method, returns self.
    pub fn max_length(mut self, n: usize) -> Self {
        self.validators.push(Box::new(MaxLength(n)));
        self
    }

    /// Add a custom validator. Named `with_validator` rather than
    /// `add` so it doesn't shadow `std::ops::Add::add` for clippy.
    pub fn with_validator(mut self, v: impl Validator + 'static) -> Self {
        self.validators.push(Box::new(v));
        self
    }

    /// Run every validator over `value`. Errors push onto `errors`.
    /// An empty value on a non-required field skips validation
    /// entirely (an optional empty input is valid).
    pub fn validate(&self, value: &str, errors: &mut ValidationErrors) {
        if !self.required && value.is_empty() {
            return;
        }
        for v in &self.validators {
            if let Err(msg) = v.check(&self.name, value) {
                errors.add(&self.name, msg);
            }
        }
    }

    /// Render the field as a single HTML `<input>` element. The
    /// `value` is the form's prefill (empty for a fresh form, the
    /// raw user input on a re-render after validation failed).
    pub fn render_html(&self, value: &str) -> String {
        let safe_value = html_escape(value);
        let required = if self.required { " required" } else { "" };
        match self.kind {
            InputKind::Textarea => format!(
                "<textarea name=\"{name}\"{required}>{safe_value}</textarea>",
                name = self.name,
            ),
            InputKind::Checkbox => {
                let checked = if value == "true" || value == "on" || value == "1" {
                    " checked"
                } else {
                    ""
                };
                format!(
                    "<input type=\"checkbox\" name=\"{name}\" value=\"true\"{checked}>",
                    name = self.name,
                )
            }
            other => format!(
                "<input type=\"{ty}\" name=\"{name}\" value=\"{safe_value}\"{required}>",
                ty = other.html_type(),
                name = self.name,
            ),
        }
    }
}

/// `IntegerFormat` is a private validator used by `Field::integer`.
/// Not exported as a builder method because every numeric field
/// already gets it.
struct IntegerFormat;
impl Validator for IntegerFormat {
    fn check(&self, field_name: &str, value: &str) -> Result<(), String> {
        value
            .parse::<i64>()
            .map(|_| ())
            .map_err(|_| format!("{field_name} must be a whole number"))
    }
}

/// `FloatFormat` is the float-field counterpart. Accepts anything
/// that parses as `f64`, which includes integer literals like `"42"`
/// — JS's `parseFloat` does the same.
struct FloatFormat;
impl Validator for FloatFormat {
    fn check(&self, field_name: &str, value: &str) -> Result<(), String> {
        value
            .parse::<f64>()
            .map(|_| ())
            .map_err(|_| format!("{field_name} must be a number"))
    }
}

// =========================================================================
// HTML escaping. Inline so the module doesn't pull in an extra crate.
// Covers the five chars the OWASP cheat sheet flags.
// =========================================================================

fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            other => out.push(other),
        }
    }
    out
}

// =========================================================================
// gaps2 #19 — `Form<T>` axum extractor + `FormErrors` lifter
//
// The architectural rule (per gaps2 #19's spec): validation errors
// originate at the ORM's `WriteError`. Every surface MAPS them, none
// REDEFINES them. `ValidationErrors` is the form-specific producer;
// `WriteError::Multiple` is the unified consumer. `FormErrors` is a
// thin wrapper around `WriteError` that adds the
// template-friendly flat view (`errors.name` → first error string).
// =========================================================================

use crate::orm::write::WriteError;

/// Form-validation error envelope. Wraps the ORM's `WriteError` so
/// every surface (REST 400 bodies, admin form spans, HTML form
/// renders) sees the same structured shape. The template helper
/// `as_template_ctx` produces the flat single-string-per-field view
/// that most form templates ask for.
///
/// Not `Clone` because `WriteError` carries a `sqlx::Error` variant
/// that's also not Clone. If you need a cheap copyable bundle of
/// rendered messages, use [`Self::as_template_ctx`] which returns
/// an owned `serde_json::Map`.
#[derive(Debug)]
pub struct FormErrors {
    inner: WriteError,
    /// The raw form pairs the user submitted, captured before
    /// validation ran. Lets the handler re-render the form template
    /// pre-filled with what the user typed — see
    /// [`Self::raw_values`] and [`Self::raw_as_json`]. The
    /// extractor (`Form::from_request`) carries this through
    /// automatically; the `From<ValidationErrors>` path leaves it
    /// empty (no raw input was ever in scope), which is the right
    /// default for handlers that build a `FormErrors` from scratch
    /// for ad-hoc errors.
    raw: HashMap<String, String>,
}

impl FormErrors {
    /// Wrap any [`WriteError`]. Use [`From`] for free conversion in
    /// `?` chains. The raw values default to empty — call
    /// [`Self::with_raw`] when you have the submitted pairs in
    /// scope (typically only inside an axum extractor).
    pub fn new(err: WriteError) -> Self {
        Self {
            inner: err,
            raw: HashMap::new(),
        }
    }

    /// Construct a `FormErrors` carrying both the validation
    /// failure AND the raw form pairs the user submitted. The raw
    /// pairs let the handler re-render the form pre-filled with
    /// what the user typed instead of falling back to
    /// `T::default()` (which loses every keystroke).
    pub fn with_raw(err: WriteError, raw: HashMap<String, String>) -> Self {
        Self { inner: err, raw }
    }

    /// Borrow the raw form pairs the user submitted, if the
    /// extractor captured them. Empty when the [`FormErrors`] was
    /// constructed via [`Self::new`] or any `From` impl that
    /// doesn't see the request body.
    pub fn raw_values(&self) -> &HashMap<String, String> {
        &self.raw
    }

    /// JSON-shaped view of the raw values, ready to drop straight
    /// into a template context as the `form` key so existing
    /// `{{ form.<field> }}` references repopulate the user's
    /// input. The map is `String → String` so every value
    /// serialises to a JSON string — templates that need typed
    /// access should call [`Self::raw_values`] and convert per
    /// field.
    pub fn raw_as_json(&self) -> serde_json::Value {
        serde_json::Value::Object(
            self.raw
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
        )
    }

    /// Borrow the underlying [`WriteError`] — keeps every accessor
    /// available (`field_errors()`, `non_field_errors()`,
    /// `error_code()`).
    pub fn as_write_error(&self) -> &WriteError {
        &self.inner
    }

    /// Move out the underlying [`WriteError`] (e.g. to feed a
    /// REST-style DRF body builder).
    pub fn into_write_error(self) -> WriteError {
        self.inner
    }

    /// Per-field error map — see [`WriteError::field_errors`].
    pub fn field_errors(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        self.inner.field_errors()
    }

    /// Cross-field / non-field error list — see
    /// [`WriteError::non_field_errors`].
    pub fn non_field_errors(&self) -> Vec<String> {
        self.inner.non_field_errors()
    }

    /// Template-friendly flat view: each field maps to its FIRST
    /// error message (string), plus the FIRST non-field error under
    /// the `form` key. Renders directly under the `errors` context
    /// key — templates write `{{ errors.name }}` or
    /// `{% if errors.form %}`.
    ///
    /// For templates that need to render EVERY error per field
    /// (rare), call [`field_errors`] / [`non_field_errors`]
    /// directly and pass the maps as-is.
    pub fn as_template_ctx(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut out = serde_json::Map::new();
        for (key, msgs) in self.field_errors() {
            if let Some(first) = msgs.into_iter().next() {
                out.insert(key, serde_json::Value::String(first));
            }
        }
        if let Some(first) = self.non_field_errors().into_iter().next() {
            out.insert("form".to_string(), serde_json::Value::String(first));
        }
        out
    }
}

impl std::fmt::Display for FormErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.inner)
    }
}

impl std::error::Error for FormErrors {}

impl From<WriteError> for FormErrors {
    fn from(e: WriteError) -> Self {
        Self::new(e)
    }
}

/// Lift the form-primitive [`ValidationErrors`] into the canonical
/// [`WriteError`]. Each per-field message becomes a
/// `WriteError::Validator { field, message }`; non-field messages
/// become an `Anonymous` validator carrying the bare message.
/// Wrapped under `WriteError::Multiple` when there's more than one.
impl From<ValidationErrors> for WriteError {
    fn from(e: ValidationErrors) -> Self {
        let mut out: Vec<WriteError> = Vec::new();
        for (field, msgs) in e.fields {
            for message in msgs {
                out.push(WriteError::Validator {
                    field: field.clone(),
                    message,
                });
            }
        }
        for message in e.non_field {
            out.push(WriteError::Validator {
                field: String::new(),
                message,
            });
        }
        if out.len() == 1 {
            out.into_iter().next().expect("len == 1")
        } else {
            WriteError::Multiple { errors: out }
        }
    }
}

impl From<ValidationErrors> for FormErrors {
    fn from(e: ValidationErrors) -> Self {
        Self::new(e.into())
    }
}

/// Axum extractor that validates a form body before the handler
/// runs. On extraction success the wrapped result is
/// `Ok(T)` (the validated struct); on validation failure the
/// wrapped result is `Err(FormErrors)`. The HTTP layer never
/// rejects — handlers ALWAYS see a `Form<T>` and decide what to
/// render via [`Self::into_result`].
///
/// ```ignore
/// use umbra::forms::Form;
///
/// pub async fn submit(form: Form<ContactForm>) -> impl IntoResponse {
///     match form.into_result() {
///         Ok(valid)  => persist_and_redirect(valid).await,
///         Err(errs)  => render_form_with_errors(errs),
///     }
/// }
/// ```
///
/// The "always wrap, handler unwraps" shape (vs. axum's rejection-
/// type pattern) lets the handler render the form template with
/// the user's original input AND the per-field errors in one place
/// — no double-render dance, no rejection-type IntoResponse impl
/// to write per form.
pub struct Form<T> {
    inner: Result<T, FormErrors>,
}

impl<T> Form<T> {
    /// Construct a `Form<T>` carrying a validated value.
    pub fn valid(value: T) -> Self {
        Self { inner: Ok(value) }
    }

    /// Construct a `Form<T>` carrying validation errors.
    pub fn invalid(errors: FormErrors) -> Self {
        Self { inner: Err(errors) }
    }

    /// Move the wrapped `Result` out. Handlers branch on this.
    pub fn into_result(self) -> Result<T, FormErrors> {
        self.inner
    }

    /// Borrow the wrapped `Result` for inspection without consuming.
    pub fn as_result(&self) -> Result<&T, &FormErrors> {
        self.inner.as_ref()
    }
}

impl<T, S> axum::extract::FromRequest<S> for Form<T>
where
    T: FormValidate + serde::de::DeserializeOwned + Send + 'static,
    S: Send + Sync,
{
    type Rejection = axum::response::Response;

    async fn from_request(
        req: axum::extract::Request,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        use axum::body::to_bytes;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        // Read the body up to a sane limit (2 MiB matches axum's
        // default Form extractor). Anything larger almost certainly
        // means a misuse (file upload through the form extractor)
        // and we'd rather 413 than buffer megabytes silently.
        const MAX_BODY: usize = 2 * 1024 * 1024;
        let bytes = match to_bytes(req.into_body(), MAX_BODY).await {
            Ok(b) => b,
            Err(_) => {
                return Err(
                    (StatusCode::PAYLOAD_TOO_LARGE, "form body exceeds 2 MiB").into_response()
                );
            }
        };

        // Parse x-www-form-urlencoded into a String->String map.
        // Empty bodies parse to an empty map — `FormValidate::validate`
        // then sees every field as missing and surfaces the right
        // per-field "required" errors.
        let pairs: std::collections::HashMap<String, String> =
            serde_urlencoded::from_bytes(&bytes).unwrap_or_default();

        // Run validation. On success, we've already proven the data
        // fits T's shape — return Ok(T). On failure, lift the
        // ValidationErrors to a FormErrors AND attach the raw
        // pairs so the handler can render the template pre-filled
        // with what the user typed. Without this the user loses
        // every keystroke on validation failure — see gaps2 #19
        // follow-up commit for the bug screenshot that prompted
        // this change.
        match T::validate(&pairs) {
            Ok(value) => Ok(Self::valid(value)),
            Err(errs) => {
                let write_err: WriteError = errs.into();
                Ok(Self::invalid(FormErrors::with_raw(write_err, pairs)))
            }
        }
    }
}

// =========================================================================
// Tests live inline because the surface is pure (no DB, no async).
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn required_field_rejects_empty_value() {
        let f = Field::text("username");
        let mut errs = ValidationErrors::new();
        let form = data(&[("username", "")]);
        f.validate(form.get("username").unwrap(), &mut errs);
        assert!(errs.fields.contains_key("username"));
        assert!(errs.fields["username"][0].contains("required"));
    }

    #[test]
    fn optional_field_with_empty_value_passes() {
        let f = Field::text("bio").optional();
        let mut errs = ValidationErrors::new();
        f.validate("", &mut errs);
        assert!(errs.is_empty());
    }

    #[test]
    fn min_max_length_combine_on_one_field() {
        let f = Field::text("title").min_length(3).max_length(5);
        let mut errs = ValidationErrors::new();
        f.validate("ab", &mut errs);
        assert!(errs.fields["title"][0].contains("at least 3"));

        let mut errs = ValidationErrors::new();
        f.validate("toolong", &mut errs);
        assert!(errs.fields["title"][0].contains("at most 5"));

        let mut errs = ValidationErrors::new();
        f.validate("abcd", &mut errs);
        assert!(errs.is_empty());
    }

    #[test]
    fn integer_field_rejects_non_numeric_input() {
        let f = Field::integer("age");
        let mut errs = ValidationErrors::new();
        f.validate("twelve", &mut errs);
        assert!(errs.fields["age"][0].contains("whole number"));

        let mut errs = ValidationErrors::new();
        f.validate("42", &mut errs);
        assert!(errs.is_empty());
    }

    #[test]
    fn email_field_runs_the_built_in_format_check() {
        let f = Field::email("email");

        let mut errs = ValidationErrors::new();
        f.validate("not-an-email", &mut errs);
        assert!(!errs.is_empty());

        let mut errs = ValidationErrors::new();
        f.validate("alice@example.com", &mut errs);
        assert!(errs.is_empty());

        // Local part missing
        let mut errs = ValidationErrors::new();
        f.validate("@example.com", &mut errs);
        assert!(!errs.is_empty());

        // Domain missing a dot
        let mut errs = ValidationErrors::new();
        f.validate("alice@example", &mut errs);
        assert!(!errs.is_empty());
    }

    #[test]
    fn non_field_errors_propagate_through_into_result() {
        let mut errs = ValidationErrors::new();
        errs.add_non_field("passwords do not match");
        let result = errs.into_result();
        match result {
            Err(e) => {
                assert_eq!(e.non_field.len(), 1);
                assert!(e.non_field[0].contains("passwords"));
            }
            Ok(_) => panic!("non-field error should fail into_result"),
        }
    }

    #[test]
    fn render_html_escapes_user_input_against_xss() {
        let f = Field::text("title");
        let rendered = f.render_html("<script>alert(1)</script>");
        assert!(rendered.contains("&lt;script&gt;"));
        assert!(!rendered.contains("<script>alert"));
        assert!(rendered.contains("name=\"title\""));
        assert!(rendered.contains("required"));
    }

    #[test]
    fn render_html_emits_the_right_input_type_per_field_kind() {
        assert!(Field::text("a").render_html("").contains("type=\"text\""));
        assert!(Field::email("a").render_html("").contains("type=\"email\""));
        assert!(
            Field::password("a")
                .render_html("")
                .contains("type=\"password\"")
        );
        assert!(
            Field::integer("a")
                .render_html("")
                .contains("type=\"number\"")
        );
        assert!(
            Field::boolean("a")
                .render_html("")
                .contains("type=\"checkbox\"")
        );
    }

    #[test]
    fn boolean_field_renders_checked_when_value_is_truthy() {
        let f = Field::boolean("is_admin");
        assert!(f.render_html("true").contains(" checked"));
        assert!(f.render_html("on").contains(" checked"));
        assert!(f.render_html("1").contains(" checked"));
        assert!(!f.render_html("").contains(" checked"));
        assert!(!f.render_html("false").contains(" checked"));
    }

    /// Demo composition: a tiny LoginForm built from primitive
    /// fields. Stands in for what a `#[derive(Form)]` would produce.
    /// Validates a HashMap, returns a typed struct, accumulates
    /// every field's errors.
    #[derive(Debug, PartialEq, Eq)]
    struct LoginForm {
        username: String,
        password: String,
    }

    impl LoginForm {
        fn validate(form: &HashMap<String, String>) -> Result<Self, ValidationErrors> {
            let username_field = Field::text("username").min_length(3).max_length(150);
            let password_field = Field::password("password").min_length(8);
            let mut errs = ValidationErrors::new();
            let username = form.get("username").cloned().unwrap_or_default();
            let password = form.get("password").cloned().unwrap_or_default();
            username_field.validate(&username, &mut errs);
            password_field.validate(&password, &mut errs);
            errs.into_result()?;
            Ok(Self { username, password })
        }
    }

    #[test]
    fn login_form_demo_validates_happy_path() {
        let input = data(&[("username", "alice"), ("password", "hunter2-stronger")]);
        let form = LoginForm::validate(&input).expect("happy path");
        assert_eq!(form.username, "alice");
        assert_eq!(form.password, "hunter2-stronger");
    }

    #[test]
    fn login_form_demo_collects_every_field_error_at_once() {
        let input = data(&[("username", "ab"), ("password", "short")]);
        let err = LoginForm::validate(&input).expect_err("both fields fail");
        assert!(err.fields.contains_key("username"));
        assert!(err.fields.contains_key("password"));
        assert!(err.fields["username"][0].contains("at least 3"));
        assert!(err.fields["password"][0].contains("at least 8"));
    }

    // =====================================================================
    // gaps2 #19 follow-up — FormErrors carries the raw form pairs so the
    // handler can re-render the template pre-filled with what the user
    // typed instead of falling back to `T::default()` (which loses every
    // keystroke). Screenshot 2026-06-10 01-03-09 reported the data-loss
    // bug pre-fix.
    // =====================================================================

    // =====================================================================
    // gaps2 #19 follow-up — regex / phone / url validators
    // =====================================================================

    #[test]
    fn phone_field_accepts_e164_format() {
        let f = Field::phone("phone");
        let mut errs = ValidationErrors::new();
        f.validate("+14155551234", &mut errs);
        assert!(errs.is_empty(), "valid E.164 should pass: {:?}", errs);
    }

    #[test]
    fn phone_field_rejects_local_only_format() {
        // The bug report case: "07065" got accepted because the
        // field had only `optional + max_length`. With `Field::phone`
        // (== `#[form(phone)]`) the E.164 regex rejects it.
        let f = Field::phone("phone");
        let mut errs = ValidationErrors::new();
        f.validate("07065", &mut errs);
        assert!(errs.fields.contains_key("phone"));
        assert!(
            errs.fields["phone"][0].contains("E.164"),
            "error message names the format: {:?}",
            errs.fields["phone"][0]
        );
    }

    #[test]
    fn phone_field_rejects_letters_and_punctuation() {
        let f = Field::phone("phone");
        for bad in &["+1-415-555-1234", "+1 (415) 555 1234", "+1abc", "+0123"] {
            let mut errs = ValidationErrors::new();
            f.validate(bad, &mut errs);
            assert!(
                errs.fields.contains_key("phone"),
                "should reject `{bad}`: {:?}",
                errs.fields
            );
        }
    }

    #[test]
    fn url_field_accepts_http_and_https_only() {
        let f = Field::url("homepage");
        for good in &["https://example.com", "http://example.com/path?q=1"] {
            let mut errs = ValidationErrors::new();
            f.validate(good, &mut errs);
            assert!(errs.is_empty(), "should accept `{good}`: {:?}", errs);
        }
        for bad in &["ftp://example.com", "mailto:a@b.c", "example.com"] {
            let mut errs = ValidationErrors::new();
            f.validate(bad, &mut errs);
            assert!(
                errs.fields.contains_key("homepage"),
                "should reject `{bad}`: {:?}",
                errs.fields
            );
        }
    }

    #[test]
    fn regex_validator_substitutes_field_in_message() {
        // {field} placeholder gets the actual field name — useful
        // for reusable messages across multiple forms.
        let f = Field::text("invoice_id")
            .regex(r"^INV-\d{6}$", "{field} must match the invoice pattern");
        let mut errs = ValidationErrors::new();
        f.validate("not-an-invoice", &mut errs);
        assert_eq!(
            errs.fields["invoice_id"][0],
            "invoice_id must match the invoice pattern"
        );
    }

    #[test]
    fn regex_validator_composes_with_required_and_max_length() {
        // Order: Required runs FIRST (empty → "is required"),
        // then max_length, then regex. An empty value should
        // surface the "required" error, not "doesn't match pattern".
        let f = Field::text("code")
            .max_length(8)
            .regex(r"^[A-Z]{3}$", "{field} must be 3 uppercase letters");

        let mut errs = ValidationErrors::new();
        f.validate("", &mut errs);
        assert!(
            errs.fields["code"][0].contains("required"),
            "empty surfaces required error first: {:?}",
            errs.fields["code"][0]
        );

        let mut errs = ValidationErrors::new();
        f.validate("HELLO", &mut errs);
        assert!(
            errs.fields["code"][0].contains("3 uppercase"),
            "regex error fires when value is present but malformed: {:?}",
            errs.fields["code"][0]
        );
    }

    #[test]
    fn form_errors_with_raw_round_trips_the_submitted_pairs() {
        let mut raw = HashMap::new();
        raw.insert("name".to_string(), "Bella Verifier".to_string());
        raw.insert("email".to_string(), "bella@invalid".to_string());
        raw.insert("phone".to_string(), "none".to_string());

        let errs = FormErrors::with_raw(
            WriteError::Validator {
                field: "email".to_string(),
                message: "email's domain must contain at least one `.`".to_string(),
            },
            raw.clone(),
        );

        // Raw values survive untouched.
        assert_eq!(
            errs.raw_values().get("name").map(|s| s.as_str()),
            Some("Bella Verifier"),
        );
        assert_eq!(
            errs.raw_values().get("phone").map(|s| s.as_str()),
            Some("none"),
        );

        // JSON shape is a flat `{ field: "literal user input" }` map,
        // ready to drop straight into a template ctx as `form` so
        // `{{ form.name }}` repopulates.
        let json = errs.raw_as_json();
        let obj = json.as_object().expect("raw_as_json is an object");
        assert_eq!(
            obj.get("name").and_then(|v| v.as_str()),
            Some("Bella Verifier")
        );
        assert_eq!(obj.get("phone").and_then(|v| v.as_str()), Some("none"));
    }

    #[test]
    fn form_errors_new_defaults_raw_to_empty_for_ad_hoc_construction() {
        // FormErrors::new doesn't see the request body — common shape
        // for handlers that construct an ad-hoc error after the
        // extractor ran. Raw map MUST default to empty, not panic.
        let errs = FormErrors::new(WriteError::Validator {
            field: "form".to_string(),
            message: "rate limited".to_string(),
        });
        assert!(errs.raw_values().is_empty());
        // JSON shape stays a valid empty object — template ctx
        // doesn't crash when nothing was submitted.
        let json = errs.raw_as_json();
        assert!(json.as_object().expect("object").is_empty());
    }
}
