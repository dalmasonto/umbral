//! Feature 55 follow-up — `DynQuerySet::filter_in_strings`.
//!
//! The admin's multi-select filter dialog sends one URL param per
//! column with comma-joined values (`?filter_brand=1,2,3`). The
//! handler splits those, then calls `filter_in_strings(col, &parts)`
//! to emit a single `WHERE col IN (?, ?, ?)` clause. The values are
//! coerced against the column's SqlType so an integer FK column
//! gets bound as i64, not a TEXT cast.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::DynQuerySet;
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fis_product")]
pub struct Product {
    pub id: i64,
    pub name: String,
    pub stock: i32,
    pub is_featured: bool,
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
            .model::<Product>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        for (name, stock, feat) in &[
            ("alpha", 10, true),
            ("beta", 20, false),
            ("gamma", 30, true),
            ("delta", 40, false),
        ] {
            sqlx::query("INSERT INTO fis_product (name, stock, is_featured) VALUES (?, ?, ?)")
                .bind(*name)
                .bind(*stock)
                .bind(*feat)
                .execute(&pool)
                .await
                .expect("seed");
        }
    })
    .await;
}

fn meta_for(table: &str) -> umbral::migrate::ModelMeta {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == table)
        .expect("registered")
}

#[tokio::test]
async fn filter_in_strings_integer_col_emits_in_clause() {
    boot().await;
    let meta = meta_for("fis_product");
    let n = DynQuerySet::for_meta(&meta)
        .filter_in_strings("stock", &["10".to_string(), "30".to_string()])
        .count()
        .await
        .expect("count");
    assert_eq!(n, 2, "stock IN (10, 30) should match alpha + gamma");
}

#[tokio::test]
async fn filter_in_strings_text_col_emits_in_clause() {
    boot().await;
    let meta = meta_for("fis_product");
    let n = DynQuerySet::for_meta(&meta)
        .filter_in_strings(
            "name",
            &[
                "alpha".to_string(),
                "delta".to_string(),
                "missing".to_string(),
            ],
        )
        .count()
        .await
        .expect("count");
    assert_eq!(
        n, 2,
        "name IN ('alpha', 'delta', 'missing') matches alpha + delta only"
    );
}

#[tokio::test]
async fn filter_in_strings_bool_col_coerces_per_type() {
    boot().await;
    let meta = meta_for("fis_product");
    let n = DynQuerySet::for_meta(&meta)
        .filter_in_strings("is_featured", &["true".to_string()])
        .count()
        .await
        .expect("count");
    assert_eq!(n, 2, "is_featured IN (true) matches the 2 featured rows");
}

#[tokio::test]
async fn filter_in_strings_skips_unparseable_int_values() {
    boot().await;
    let meta = meta_for("fis_product");
    // "10" parses, "garbage" is dropped; result is `stock IN (10)`.
    let n = DynQuerySet::for_meta(&meta)
        .filter_in_strings("stock", &["10".to_string(), "garbage".to_string()])
        .count()
        .await
        .expect("count");
    assert_eq!(n, 1, "only alpha matches the surviving parsed value 10");
}

#[tokio::test]
async fn filter_in_strings_empty_input_is_noop() {
    boot().await;
    let meta = meta_for("fis_product");
    let n = DynQuerySet::for_meta(&meta)
        .filter_in_strings("stock", &[])
        .count()
        .await
        .expect("count");
    assert_eq!(n, 4, "empty filter must not constrain the result set");
}
