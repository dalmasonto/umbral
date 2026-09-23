//! gaps6 #7 — DynQuerySet::filter_pk_eq: filter by a JSON pk value, coercing to
//! the pk column's type. Exercised for both an i64 pk and a String pk (the
//! materialized write-back must not assume i64).

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use umbral::orm::DynQuerySet;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fpk_int")]
pub struct FpkInt {
    pub id: i64,
    pub label: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "fpk_str")]
pub struct FpkStr {
    #[umbral(primary_key)]
    pub slug: String,
    pub label: String,
}

async fn boot() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().unwrap();
        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<FpkInt>()
            .model::<FpkStr>()
            .build()
            .unwrap();
        umbral_core::migrate::create_tables_for_tests()
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn filter_pk_eq_matches_an_i64_pk() {
    boot().await;
    let meta = umbral::migrate::ModelMeta::for_::<FpkInt>();
    DynQuerySet::for_meta(&meta)
        .insert_json(json!({"label":"a"}).as_object().unwrap())
        .await
        .unwrap();
    let row = DynQuerySet::for_meta(&meta)
        .insert_json(json!({"label":"b"}).as_object().unwrap())
        .await
        .unwrap();
    let id = row["id"].clone();

    let n = DynQuerySet::for_meta(&meta)
        .filter_pk_eq(&id)
        .update_json(json!({"label":"B"}).as_object().unwrap())
        .await
        .unwrap();
    assert_eq!(n, 1, "exactly the one pk-matched row updates");

    let got = DynQuerySet::for_meta(&meta)
        .filter_pk_eq(&id)
        .fetch_as_json()
        .await
        .unwrap();
    assert_eq!(got[0]["label"], "B");
}

#[tokio::test]
async fn filter_pk_eq_matches_a_string_pk() {
    boot().await;
    let meta = umbral::migrate::ModelMeta::for_::<FpkStr>();
    DynQuerySet::for_meta(&meta)
        .insert_json(json!({"slug":"x","label":"a"}).as_object().unwrap())
        .await
        .unwrap();

    let n = DynQuerySet::for_meta(&meta)
        .filter_pk_eq(&json!("x"))
        .update_json(json!({"label":"A"}).as_object().unwrap())
        .await
        .unwrap();
    assert_eq!(n, 1, "a String pk coerces and matches");
}
