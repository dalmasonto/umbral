//! gaps4 #26 / gap #73 — `insert_form` returns the new row's PK in its TRUE
//! shape, not a lying `0` for a non-integer PK.
//!
//! Before the fix `insert_form` returned `i64`: an integer PK came back correct,
//! but a String / Uuid PK fell through to `Ok(0)`. A caller that redirected to
//! `…/edit/{pk}` or wired a child to the new parent used `0` and addressed the
//! wrong row (or none). Now it returns [`InsertedPk`], preserving the shape.

#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{DynQuerySet, InsertedPk};
use umbral_core::db;

/// A model whose PK is a String (`slug`), not the default `id: i64`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ifp_tag")]
pub struct Tag {
    #[umbral(primary_key, string, max_length = 50)]
    pub slug: String,
    #[umbral(string)]
    pub label: String,
}

/// A conventional integer-PK model, to prove the integer path is untouched.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "ifp_note")]
pub struct Note {
    pub id: i64,
    #[umbral(string)]
    pub body: String,
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
            .model::<Tag>()
            .model::<Note>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

fn tag_meta() -> umbral::migrate::ModelMeta {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "ifp_tag")
        .expect("Tag registered")
}

fn note_meta() -> umbral::migrate::ModelMeta {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "ifp_note")
        .expect("Note registered")
}

fn form(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The one that matters: a String-PK insert hands back the REAL slug, not `0`.
#[tokio::test]
async fn string_pk_insert_returns_the_real_key_not_zero() {
    boot().await;
    let pk = DynQuerySet::for_meta(&tag_meta())
        .insert_form(&form(&[("slug", "rust"), ("label", "Rust")]), &[])
        .await
        .expect("insert tag");

    assert_eq!(
        pk,
        InsertedPk::Text("rust".to_string()),
        "a String-PK insert must return its actual key in true shape, not a lying 0"
    );
    // And it renders as the bare identifier a redirect URL wants — no quotes,
    // no `0`.
    assert_eq!(pk.to_string(), "rust");
    assert_eq!(pk.as_i64(), None, "a String PK is not an integer");
}

/// The integer path is unchanged: a real auto-increment id, usable as i64.
#[tokio::test]
async fn integer_pk_insert_still_returns_the_rowid() {
    boot().await;
    let pk = DynQuerySet::for_meta(&note_meta())
        .insert_form(&form(&[("body", "hello")]), &[])
        .await
        .expect("insert note");

    let id = pk.as_i64().expect("integer PK comes back as Int");
    assert!(id > 0, "auto-increment id should be a real rowid");
    assert_eq!(pk.to_string(), id.to_string());
}

/// An empty insert (nothing survived the skip filter) reports `None`, not `0`.
#[tokio::test]
async fn empty_insert_reports_none() {
    boot().await;
    // Skip the only writable column; the auto-increment PK is omitted anyway, so
    // no column is left to insert.
    let pk = DynQuerySet::for_meta(&note_meta())
        .insert_form(&form(&[("body", "x")]), &["body".to_string()])
        .await
        .expect("insert note");
    assert!(
        pk.is_none(),
        "an insert with no columns must report None: {pk:?}"
    );
}
