//! gaps2 #12 — `DynError` is a real enum carrying `WriteError` or
//! `sqlx::Error`, not a bare alias.
//!
//! Pins the routing contract: form-coercion failures surface as
//! `DynError::Write(WriteError::Validator{..})` with the offending
//! column name intact, so the admin / Form<T> / REST consumers can
//! render per-field instead of flattening to a string. DB-driver
//! failures stay on the `Sqlx` arm.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::write::WriteError;
use umbral::orm::{DynError, DynQuerySet};
use umbral_core::db;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "dyn_err_widget")]
pub struct Widget {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
    pub stock: i64,
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
            .model::<Widget>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

fn meta() -> umbral::migrate::ModelMeta {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "dyn_err_widget")
        .expect("registered")
}

#[tokio::test]
async fn form_coercion_failure_surfaces_as_dyn_error_write_with_field_name() {
    boot().await;
    // "not a number" can't coerce into the `stock` BIGINT column,
    // so the form coercion path should land in the Validator arm
    // of WriteError, with `field = "stock"` so the admin can render
    // the message beneath the right input.
    let mut form = std::collections::HashMap::new();
    form.insert("name".to_string(), "widget-a".to_string());
    form.insert("stock".to_string(), "not a number".to_string());

    let err = DynQuerySet::for_meta(&meta())
        .insert_form(&form, &[])
        .await
        .expect_err("non-numeric stock should fail coercion");

    match err {
        DynError::Write(WriteError::Validator { field, message }) => {
            assert_eq!(field, "stock", "field must point at the offending column");
            assert!(
                !message.is_empty(),
                "validator message must carry a hint of the parse failure"
            );
        }
        DynError::Write(other) => {
            panic!("expected WriteError::Validator on form coercion failure, got {other:?}")
        }
        DynError::Sqlx(e) => panic!(
            "form coercion failure must NOT flatten to sqlx::Error \
             (gaps2 #12 regression). got: {e:?}"
        ),
    }
}

#[tokio::test]
async fn update_form_coercion_failure_also_surfaces_as_dyn_error_write() {
    boot().await;
    // The form-coercion check fires BEFORE the UPDATE executes,
    // so we don't need a real row to target — a synthetic
    // `WHERE id = 999` is enough to drive the path. (Sharing
    // seeded rows across `#[tokio::test]`s on an in-memory
    // SQLite pool is unreliable because each fresh pool
    // connection points at its own memory DB.)
    let mut bad = std::collections::HashMap::new();
    bad.insert("stock".to_string(), "still not a number".to_string());
    let err = DynQuerySet::for_meta(&meta())
        .filter_eq_string("id", "999")
        .update_form(&bad, &[])
        .await
        .expect_err("non-numeric stock should fail coercion on update");

    match err {
        DynError::Write(WriteError::Validator { field, .. }) => {
            assert_eq!(field, "stock");
        }
        other => panic!("expected DynError::Write(Validator), got: {other:?}"),
    }
}

#[tokio::test]
async fn dyn_error_lifts_via_from_for_both_arms() {
    // gaps2 #12 contract: `?` ergonomics across both arms. Pin the
    // `From` impls so we'd notice if either disappeared.
    let sqlx_err: sqlx::Error = sqlx::Error::Protocol("synthetic".to_string());
    let lifted: DynError = sqlx_err.into();
    assert!(matches!(lifted, DynError::Sqlx(_)));

    let write_err = WriteError::Validator {
        field: "foo".to_string(),
        message: "synthetic".to_string(),
    };
    let lifted: DynError = write_err.into();
    assert!(matches!(lifted, DynError::Write(_)));
}
