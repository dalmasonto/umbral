//! Rejection tests for the `#[umbral::model(base = ...)]` attribute macro
//! (gaps4 #62/#64/#67).
//!
//! The success path (splice produces flat native fields, real round-trip,
//! auto-generated typed consts) is covered in
//! `crates/umbral-core/tests/model_attr_base.rs`. This file pins the
//! `compile_error!` messages a misuse produces:
//!
//!   1. No `#[derive(..., Model)]` on the struct below the attribute — the
//!      ordering footgun (attribute placed below the derive line, or the
//!      derive omitted) — is caught with an ordering hint.
//!   2. An unrecognized argument (anything but `base = <Base>`) is rejected.

#[test]
fn rejects_missing_model_derive() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/model_attr_fixtures/no_model_derive.rs");
}

#[test]
fn rejects_bad_argument() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/model_attr_fixtures/bad_argument.rs");
}
