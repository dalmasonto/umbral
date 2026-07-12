//! `AppBuilder::auto_models()` — models register themselves (gaps3 #46).
//!
//! Naming every model with `.model::<T>()` is pure ceremony: the information is
//! already in the `#[derive(Model)]`. The derive now self-registers into a
//! link-time slice, and `auto_models()` collects it.
//!
//! It stays **opt-in** on purpose. Discovery is link-time: a model in your binary
//! crate is always linked and always found, but a model in a *library* crate that
//! nothing references can be dropped by the linker — and would then be missing
//! from the registry, i.e. missing from `makemigrations`, i.e. a table that
//! silently never gets created. That failure is invisible until production, so it
//! is not something to make the default.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePoolOptions;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "am_post")]
pub struct AmPost {
    pub id: i64,
    pub title: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "am_tag")]
pub struct AmTag {
    pub id: i64,
    pub name: String,
}

/// The derive submits every model to the link-time slice, with no app help.
#[test]
fn the_derive_self_registers_every_model() {
    let tables: Vec<String> = umbral::migrate::link_registered_models()
        .into_iter()
        .map(|m| m.table)
        .collect();
    for t in ["am_post", "am_tag"] {
        assert!(
            tables.contains(&t.to_string()),
            "`{t}` should self-register via #[derive(Model)]; found {tables:?}",
        );
    }
}

/// `auto_models()` registers them without a `.model::<T>()` per model — and
/// composes with explicit registration rather than replacing it, so adding it to
/// an existing app can't double-register anything.
#[tokio::test]
async fn auto_models_registers_them_and_dedupes_against_explicit() {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let pool = SqlitePoolOptions::new()
        .connect("sqlite::memory:")
        .await
        .expect("pool");

    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        // AmPost named explicitly, AmTag not named at all: both must land, once.
        .model::<AmPost>()
        .auto_models()
        .build()
        .expect("App::build");

    let tables: Vec<String> = umbral::migrate::registered_models()
        .into_iter()
        .map(|m| m.table)
        .collect();

    assert!(
        tables.contains(&"am_tag".to_string()),
        "auto_models() must register a model the app never named; got {tables:?}",
    );
    assert_eq!(
        tables.iter().filter(|t| *t == "am_post").count(),
        1,
        "a model registered BOTH explicitly and by auto_models() must appear once, \
         not twice — the two compose; got {tables:?}",
    );
}
