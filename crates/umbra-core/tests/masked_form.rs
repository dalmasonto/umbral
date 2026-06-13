//! `Masked` fields are excluded from the `Form` derive by default.
//!
//! Before this, a `Masked` field on a `Form`-deriving struct was a hard
//! compile error ("unsupported field type … mark with `#[umbra(noform)]`").
//! Masked secrets are server-set, never user-submittable, so the derive
//! now skips them automatically — this struct compiles WITHOUT any
//! `#[umbra(noform)]` on the masked fields, and the form validates
//! without them.

#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;
use umbra::forms::FormValidate;
use umbra::orm::Masked;

#[derive(
    Debug,
    Clone,
    Default,
    sqlx::FromRow,
    Serialize,
    Deserialize,
    umbra::orm::Model,
    umbra::forms::Form,
)]
#[umbra(table = "masked_form_contact")]
pub struct Contact {
    pub id: i64,
    #[form(required, length(min = 2, max = 80))]
    pub name: String,
    // No `#[umbra(noform)]` here — the derive skips Masked on its own.
    pub api_key: Masked<String>,
    pub recovery_code: Option<Masked<String>>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();
async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbra::Settings::from_env().expect("figment defaults");
        let pool = SqlitePoolOptions::new()
            .connect_with(SqliteConnectOptions::new().in_memory(true))
            .await
            .expect("pool");
        umbra::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Contact>()
            .build()
            .expect("App::build");
    })
    .await;
}

fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[tokio::test]
async fn masked_fields_are_skipped_from_the_form() {
    boot().await;
    // The submission carries only the non-masked field; the masked
    // fields are not part of the form surface at all.
    let contact = Contact::validate(&data(&[("name", "Ada")]))
        .await
        .expect("validates without supplying the masked fields");
    assert_eq!(contact.name, "Ada");
    // They took their defaults: an empty in-memory plaintext, and None.
    assert_eq!(contact.api_key.reveal().unwrap(), "");
    assert!(contact.recovery_code.is_none());
}
