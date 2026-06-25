//! Feature #67 — custom template tags / filters. Two surfaces:
//!   * the built-in example tags shipped in every engine (`now`,
//!     `currency`),
//!   * the `Plugin::template_registrars` hook, exercised here directly
//!     through `templates::init_with` (the seam `App::build` calls) by
//!     installing a custom `shout` filter and an `exclaim` function.
//!
//! One `init_with` per test binary (it publishes the process-wide engine
//! OnceLock), so a single boot registers every template and all tests
//! render against it.

use std::fs;
use std::sync::OnceLock;

use tempfile::TempDir;
use umbral_core::templates::{self, TemplateRegistrar};

static DIR: OnceLock<TempDir> = OnceLock::new();

fn boot() {
    DIR.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("currency.html"), "{{ amount | currency }}").unwrap();
        fs::write(
            dir.path().join("currency_code.html"),
            "{{ amount | currency(code) }}",
        )
        .unwrap();
        fs::write(dir.path().join("now_fmt.html"), "{{ now(fmt) }}").unwrap();
        fs::write(dir.path().join("now_default.html"), "{{ now() }}").unwrap();
        // Uses the plugin-contributed filter + function.
        fs::write(
            dir.path().join("custom.html"),
            "{{ word | shout }}{{ exclaim() }}",
        )
        .unwrap();

        // A plugin's `template_registrars()` would return exactly this:
        // owned, 'static closures that mutate the Environment.
        let registrars: Vec<TemplateRegistrar> = vec![Box::new(|env| {
            env.add_filter("shout", |s: String| s.to_uppercase());
            env.add_function("exclaim", || "!".to_string());
        })];

        templates::init_with(&[dir.path().to_path_buf()], registrars)
            .expect("init_with should publish the engine");
        dir
    });
}

fn render(name: &str, ctx: serde_json::Value) -> String {
    templates::render(name, &ctx).unwrap()
}

#[test]
fn currency_filter_defaults_to_usd_with_grouping() {
    boot();
    assert_eq!(
        render("currency.html", serde_json::json!({ "amount": 1234.5 })),
        "$1,234.50"
    );
    assert_eq!(
        render("currency.html", serde_json::json!({ "amount": 7.0 })),
        "$7.00"
    );
    assert_eq!(
        render(
            "currency.html",
            serde_json::json!({ "amount": 1234567.891 })
        ),
        "$1,234,567.89"
    );
}

#[test]
fn currency_filter_negative_sign_precedes_symbol() {
    boot();
    assert_eq!(
        render("currency.html", serde_json::json!({ "amount": -12.4 })),
        "-$12.40"
    );
}

#[test]
fn currency_filter_honors_known_and_unknown_codes() {
    boot();
    assert_eq!(
        render(
            "currency_code.html",
            serde_json::json!({ "amount": 50.0, "code": "EUR" })
        ),
        "€50.00"
    );
    assert_eq!(
        render(
            "currency_code.html",
            serde_json::json!({ "amount": 99.0, "code": "GBP" })
        ),
        "£99.00"
    );
    // Unknown ISO code falls back to "<amount> <CODE>".
    assert_eq!(
        render(
            "currency_code.html",
            serde_json::json!({ "amount": 1000.0, "code": "XYZ" })
        ),
        "1,000.00 XYZ"
    );
}

#[test]
fn now_function_formats_with_strftime() {
    boot();
    // A `%Y` format yields the current 4-digit year — assert it parses to
    // a sane value rather than pinning a clock-dependent literal.
    let year = render("now_fmt.html", serde_json::json!({ "fmt": "%Y" }));
    let year: i32 = year.trim().parse().expect("year should be numeric");
    assert!(year >= 2026, "got implausible year {year}");
}

#[test]
fn now_function_defaults_to_rfc3339() {
    boot();
    let out = render("now_default.html", serde_json::json!({}));
    // RFC 3339 always carries the date/time separator.
    assert!(
        out.contains('T'),
        "expected an RFC3339 timestamp, got {out}"
    );
}

#[test]
fn plugin_registrar_filter_and_function_are_applied() {
    boot();
    let out = render("custom.html", serde_json::json!({ "word": "hi" }));
    assert_eq!(out, "HI!");
}
