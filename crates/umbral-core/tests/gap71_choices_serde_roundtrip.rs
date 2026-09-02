//! Gap #71 — a `#[derive(Choices)]` field round-trips the TYPED write path
//! with ONLY `#[choices(rename_all = …)]` on the enum (no duplicated
//! `#[serde(rename_all = …)]`, no `#[derive(Serialize, Deserialize)]`).
//!
//! The typed write path serializes the whole model with serde
//! (`serde_json::to_value(instance)`) and validates the result against the
//! Choices vocabulary (the CHECK-constrained DB values). Before the fix, serde
//! emitted the PascalCase variant name (`"Other"` / `"WebFramework"`) while the
//! validator only accepted the renamed value (`"other"` / `"web_framework"`),
//! so writing an explicit non-default variant was silently REJECTED. The
//! Choices derive now emits its own `Serialize`/`Deserialize` that speak the
//! DB-value vocabulary, so serde is single-sourced with `as_str` / the CHECK.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral_core::db;

// NOTE: ONLY `#[choices(rename_all = "snake_case")]`. No `#[serde(...)]`, no
// `#[derive(Serialize, Deserialize)]` on this enum — that is the whole point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, umbral::orm::Choices)]
#[choices(rename_all = "snake_case")]
pub enum SoftwareCategory {
    Language,
    WebFramework,
    Other,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "g71_software")]
pub struct Software {
    pub id: i64,
    pub name: String,
    #[umbral(choices)]
    pub category: SoftwareCategory,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Software>()
            .build()
            .expect("App::build");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

/// serde now serializes a Choices variant as its DB string, respecting
/// `#[choices(rename_all)]` — the exact value the validator + CHECK expect.
#[test]
fn serde_serializes_the_db_value_not_the_variant_name() {
    assert_eq!(
        serde_json::to_string(&SoftwareCategory::Other).unwrap(),
        "\"other\""
    );
    assert_eq!(
        serde_json::to_string(&SoftwareCategory::WebFramework).unwrap(),
        "\"web_framework\""
    );
    // ...and Deserialize reads that same DB string back to the variant.
    let back: SoftwareCategory = serde_json::from_str("\"web_framework\"").unwrap();
    assert_eq!(back, SoftwareCategory::WebFramework);
}

/// The real bug: a typed `create(...)` of a NON-default `rename_all`'d variant
/// must NOT be rejected by the validator, and must store the renamed value.
#[tokio::test]
async fn typed_create_of_nondefault_choices_variant_round_trips() {
    boot().await;

    // This is the write that used to fail: serde emitted "Other", the CHECK set
    // is {language, web_framework, other}, insert rejected. Now it succeeds.
    let created = Software::objects()
        .create(Software {
            id: 0,
            name: "rustc".to_string(),
            category: SoftwareCategory::Other,
        })
        .await
        .expect("create with SoftwareCategory::Other must not be rejected");
    assert_eq!(created.category, SoftwareCategory::Other);

    // A second row with a two-word snake_case variant.
    Software::objects()
        .create(Software {
            id: 0,
            name: "axum".to_string(),
            category: SoftwareCategory::WebFramework,
        })
        .await
        .expect("create with WebFramework must not be rejected");

    // Read the row back through the typed path — the enum decodes correctly.
    let fetched = Software::objects()
        .filter(software::NAME.eq("rustc"))
        .first()
        .await
        .expect("fetch")
        .expect("row exists");
    assert_eq!(fetched.category, SoftwareCategory::Other);

    // Prove the STORED string is the renamed value ("other"), not "Other":
    // filtering by the raw DB string selects the row.
    let n = Software::objects()
        .filter(software::CATEGORY.eq("other"))
        .count()
        .await
        .expect("count by stored db value");
    assert_eq!(n, 1, "the row stored the renamed value `other`");

    let web = Software::objects()
        .filter(software::CATEGORY.eq("web_framework"))
        .count()
        .await
        .expect("count web_framework");
    assert_eq!(web, 1, "two-word variant stored as `web_framework`");
}
