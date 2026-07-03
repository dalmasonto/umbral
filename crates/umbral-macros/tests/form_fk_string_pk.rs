//! Regression test for audit finding `core-macros-cli` #8: the `Form`
//! derive used to parse FK values with a hardcoded `parse::<i64>()` +
//! `ForeignKey::new(0)`, so deriving `Form` on a model whose FK target
//! has a `String` (or `Uuid`) primary key failed to compile / parsed
//! the wrong type. The derive now parses into
//! `<Target as Model>::PrimaryKey`, so a String-PK FK target works.
//!
//! This exercises the real path: derive `Form` on a model with a
//! `ForeignKey<StringKeyedParent>`, submit a string id, validate, and
//! read the linked parent back through the ORM.

#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use umbral::forms::FormValidate;
use umbral::orm::ForeignKey;

// Parent with a String primary key (a slug-style key).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ffk_str_country")]
pub struct Country {
    #[umbral(primary_key)]
    pub code: String,
    pub name: String,
}

// Child whose FK targets the String-PK parent. If the derive still
// hardcoded i64 this struct would fail to compile.
#[derive(
    Debug,
    Clone,
    Default,
    sqlx::FromRow,
    Serialize,
    Deserialize,
    umbral::orm::Model,
    umbral::forms::Form,
)]
#[umbral(table = "ffk_str_city")]
pub struct City {
    #[umbral(primary_key)]
    pub id: i64,
    #[form(required, length(min = 1, max = 120))]
    pub name: String,
    pub country: ForeignKey<Country>,
}

fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

static BOOT: OnceCell<()> = OnceCell::const_new();
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("sqlite");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Country>()
            .model::<City>()
            .build()
            .expect("App::build");
        sqlx::query("CREATE TABLE ffk_str_country (code TEXT PRIMARY KEY, name TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("create country");
        sqlx::query("CREATE TABLE ffk_str_city (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, country TEXT NOT NULL REFERENCES ffk_str_country(code))")
            .execute(&pool)
            .await
            .expect("create city");
        sqlx::query("INSERT INTO ffk_str_country (code, name) VALUES ('ke', 'Kenya')")
            .execute(&pool)
            .await
            .expect("seed country");
    })
    .await;
}

#[tokio::test]
async fn string_pk_fk_parses_and_links_real_parent() {
    boot().await;
    // Submit the parent's String PK as the FK value.
    let city = City::validate(&data(&[("name", "Nairobi"), ("country", "ke")]))
        .await
        .expect("valid String-PK FK");
    // The parsed FK carries the submitted String id verbatim — proving
    // it was parsed as a String, not coerced through i64.
    assert_eq!(city.country.id(), "ke");

    // Persist + resolve the parent back through the ORM.
    let created = City::objects().create(city).await.expect("create city");
    let parent = created
        .country
        .resolve(&umbral::db::pool())
        .await
        .expect("resolve parent");
    assert_eq!(parent.name, "Kenya", "FK resolves to the seeded parent");
}

#[tokio::test]
async fn string_pk_fk_rejects_nonexistent_parent() {
    boot().await;
    let err = City::validate(&data(&[
        ("name", "Ghost-string-pk-city"),
        ("country", "zz"),
    ]))
    .await
    .expect_err("nonexistent String-PK FK rejected");
    assert!(
        err.fields.contains_key("country"),
        "error keyed to the FK field"
    );
    let count = City::objects()
        .filter(city::NAME.eq("Ghost-string-pk-city"))
        .count()
        .await
        .expect("count by name");
    assert_eq!(count, 0, "no row inserted on a bad FK");
}
