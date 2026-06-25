//! Regression coverage for the `models![T, U, V]` macro on the
//! umbral facade. The macro resolves each type to its
//! `Model::TABLE` string at compile time, producing a plain
//! array literal callers borrow as `&[&str]` and feed to any
//! table-name-accepting API (`AdminPlugin::dashboard_models_only`,
//! REST resource allowlists, future similar surfaces).
//!
//! What the tests pin:
//!   - Single-arity expansion works (`models![T]`).
//!   - Multi-arity expansion preserves declaration order.
//!   - Trailing comma is accepted.
//!   - The expansion compiles in both `&[&str]` and array contexts.
//!   - Renaming a model's `#[umbral(table = "...")]` propagates
//!     through the macro without any caller change.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use umbral::orm::Model;
use umbral::prelude::*;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Product {
    id: i64,
    name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "orders")] // <-- renamed; macro must follow
struct Order {
    id: i64,
    note: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Customer {
    id: i64,
    email: String,
}

#[test]
fn single_type_resolves_to_its_table() {
    let tables = models![Product];
    assert_eq!(tables, ["product"]);
}

#[test]
fn multiple_types_preserve_declaration_order() {
    let tables = models![Customer, Product, Order];
    // Order matters — the dashboard renders cards left-to-right
    // in this order, so the macro must be stable.
    assert_eq!(tables, ["customer", "product", "orders"]);
}

#[test]
fn trailing_comma_accepted() {
    let tables = models![Product, Order,];
    assert_eq!(tables, ["product", "orders"]);
}

#[test]
fn renamed_table_is_picked_up_automatically() {
    // `Order` carries `#[umbral(table = "orders")]` — if the macro
    // reached through anything other than `Model::TABLE`, this
    // would still say "order" (the struct-name fallback). The
    // assertion proves the macro routes through TABLE, so a
    // table rename never requires touching downstream call sites.
    assert_eq!(models![Order], ["orders"]);
    assert_eq!(<Order as Model>::TABLE, "orders");
}

#[test]
fn passes_to_a_slice_taking_api() {
    // The real call site is `dashboard_models_only(&[S])` where
    // `S: Into<String> + Clone`. Borrow the array literal as a
    // slice; `&'static str` satisfies the bound. This compiles
    // → the API integration works.
    fn takes_table_slice<S: Into<String> + Clone>(_tables: &[S]) {}
    takes_table_slice(&models![Product, Order, Customer]);
}
