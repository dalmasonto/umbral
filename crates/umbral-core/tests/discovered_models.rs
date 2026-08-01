//! gaps4 #40: `umbral::discovered_models!()` — a plugin's `models()` body in
//! one line. The derive records `module_path!()` in its link-time
//! registration, and the macro filters the slice down to the CALLING crate,
//! so this binary sees its own models and none of the framework's.
//!
//! No App boot, no DB — this is a pure registry read, which is exactly the
//! point: `models()` runs before the app exists.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "dm_widget")]
pub struct Widget {
    pub id: i64,
    pub name: String,
}

// In a submodule, to prove crate-level (not module-level) matching.
mod nested {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
    #[umbral(table = "dm_gadget")]
    pub struct Gadget {
        pub id: i64,
        pub label: String,
    }
}

#[test]
fn discovered_models_returns_exactly_this_crates_models() {
    let metas = umbral::discovered_models!();
    let mut tables: Vec<&str> = metas.iter().map(|m| m.table.as_str()).collect();
    tables.sort_unstable();

    assert_eq!(
        tables,
        vec!["dm_gadget", "dm_widget"],
        "exactly this binary's two models — crate-wide (the submodule model \
         counts), and nothing from linked framework crates"
    );
}

#[test]
fn discovered_models_carry_full_meta() {
    let metas = umbral::discovered_models!();
    let widget = metas
        .iter()
        .find(|m| m.table == "dm_widget")
        .expect("widget discovered");
    assert_eq!(widget.name, "Widget");
    assert!(
        widget.fields.iter().any(|f| f.name == "name"),
        "the registration builds the same ModelMeta as ModelMeta::for_::<T>()"
    );
}
