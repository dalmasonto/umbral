//! Per-field clean / validate hooks (features #83).
//!
//! The declarative attributes cover the rules the framework can name. This is the
//! escape hatch for the ones only your app knows — masking a banned word,
//! rejecting a username that collides with a reserved route, coercing a phone
//! number into E.164.
//!
//! One hook shape does both jobs: `Ok(value)` rewrites, `Err(message)` rejects as
//! a `WriteError::Validator` — which REST already renders as a per-field 400, the
//! `Form<T>` extractor already surfaces, and the admin already shows inline. You
//! write the rule; you wire nothing.
//!
//! The property that decides whether this is worth anything: **a hook fires on
//! every write path.** One that only ran for REST would look enforced while a
//! background job or a seed script walked straight past it — worse than no hook,
//! because you'd trust it.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::cleaners::{clear_for_tests, register_cleaner};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "cl_post")]
pub struct ClPost {
    pub id: i64,
    pub title: String,
}

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &L
}

async fn boot() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("cl.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<ClPost>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

/// A transform hook rewrites the value — on the TYPED path.
#[tokio::test]
async fn a_transform_hook_rewrites_on_the_typed_path() {
    let _g = lock().lock().await;
    boot().await;
    clear_for_tests();
    register_cleaner::<ClPost>("title", |v| {
        Ok(json!(
            v.as_str().unwrap_or_default().replace("damn", "d***")
        ))
    });

    let row = ClPost::objects()
        .create(ClPost {
            id: 0,
            title: "a damn good post".into(),
        })
        .await
        .expect("create");
    assert_eq!(row.title, "a d*** good post");
    clear_for_tests();
}

/// **The property that matters.** The same hook fires on the DYNAMIC path — which
/// is what REST and the admin run on. A hook that only covered one path would look
/// enforced while another writer walked past it.
#[tokio::test]
async fn the_same_hook_fires_on_the_dynamic_path() {
    let _g = lock().lock().await;
    boot().await;
    clear_for_tests();
    register_cleaner::<ClPost>("title", |v| {
        Ok(json!(
            v.as_str().unwrap_or_default().replace("damn", "d***")
        ))
    });

    let meta = umbral::migrate::ModelMeta::for_::<ClPost>();
    let row = umbral::orm::DynQuerySet::for_meta(&meta)
        .insert_json(json!({"title": "another damn post"}).as_object().unwrap())
        .await
        .expect("dyn insert");

    assert_eq!(
        row["title"],
        json!("another d*** post"),
        "REST and the admin run on DynQuerySet — a hook that skipped them would be \
         a rule you *think* is enforced",
    );
    clear_for_tests();
}

/// A reject hook fails the write as a `Validator` error, keyed to the field — the
/// shape REST renders as a 400 field-error map and the admin shows inline.
#[tokio::test]
async fn a_reject_hook_fails_the_write_with_a_field_error() {
    let _g = lock().lock().await;
    boot().await;
    clear_for_tests();
    register_cleaner::<ClPost>("title", |v| {
        if v.as_str().unwrap_or_default().contains("<script") {
            return Err("HTML is not allowed in a title".into());
        }
        Ok(v.clone())
    });

    let err = ClPost::objects()
        .create(ClPost {
            id: 0,
            title: "<script>alert(1)</script>".into(),
        })
        .await
        .expect_err("the hook must reject this write");

    let fields = err.field_errors();
    assert_eq!(
        fields.get("title").map(|v| v.as_slice()),
        Some(["HTML is not allowed in a title".to_string()].as_slice()),
        "the rejection must arrive keyed to the field, so REST/admin render it \
         inline with no wiring; got {fields:?}",
    );

    let n: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM cl_post WHERE title LIKE '%script%'")
        .fetch_one(&umbral::db::pool())
        .await
        .unwrap();
    assert_eq!(n.0, 0, "the row must not have been written");
    clear_for_tests();
}

/// Hooks compose in registration order, each seeing the previous one's output —
/// so a normalise step and a reject step work together.
#[tokio::test]
async fn hooks_compose_in_registration_order() {
    let _g = lock().lock().await;
    boot().await;
    clear_for_tests();
    // 1. normalise
    register_cleaner::<ClPost>("title", |v| {
        Ok(json!(v.as_str().unwrap_or_default().trim().to_string()))
    });
    // 2. reject — sees the TRIMMED value, so "   " is caught as empty
    register_cleaner::<ClPost>("title", |v| {
        if v.as_str().unwrap_or_default().is_empty() {
            return Err("title cannot be blank".into());
        }
        Ok(v.clone())
    });

    let err = ClPost::objects()
        .create(ClPost {
            id: 0,
            title: "     ".into(),
        })
        .await
        .expect_err("whitespace-only must be rejected by the second hook");
    assert!(
        err.field_errors().contains_key("title"),
        "the second hook must see the first's output, or a whitespace-only title \
         sails through as 'non-empty'",
    );
    clear_for_tests();
}

/// A hook on a field that doesn't exist is a boot-time panic, not a silent no-op.
/// A moderation rule that *looks* installed and never runs is the worst outcome.
#[test]
#[should_panic(expected = "does not exist")]
fn registering_a_hook_on_an_unknown_field_panics() {
    register_cleaner::<ClPost>("titel", |v| Ok(v.clone()));
}
