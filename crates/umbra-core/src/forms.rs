//! Form parsing, validation, and HTML rendering.
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
}
