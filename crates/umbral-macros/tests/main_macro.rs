//! Rejection tests for the `#[umbral::main]` attribute macro (gaps4 #60).
//!
//! The success path (macro expands, generated fn actually runs on a real
//! tokio runtime, `Ok`/`Err` propagate) is covered in
//! `crates/umbral/tests/umbral_main_macro.rs` — that's the compile+run
//! test; `umbral-macros` itself has no async runtime to run one against,
//! so this file sticks to what `trybuild` is for: pinning the
//! `compile_error!` message a misuse produces.
//!
//!   1. Non-`async fn` is rejected.
//!   2. Attribute arguments (`flavor = "..."`, not supported yet) are
//!      rejected with a message pointing at `#[tokio::main(...)]` as the
//!      escape hatch.

#[test]
fn rejects_non_async_fn() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/main_macro_fixtures/non_async_fn.rs");
}

#[test]
fn rejects_attribute_arguments() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/main_macro_fixtures/args_not_supported.rs");
}
