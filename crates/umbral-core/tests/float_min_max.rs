//! gaps2 #73 — float fields (`Real`/`Double`) must enforce `min`/`max`
//! constraints via `validate_numeric_bounds` on the `insert_json` /
//! `update_json` dynamic write path.
//!
//! Pre-fix: `validate_numeric_bounds` in `orm/dynamic.rs` guards with
//! `json.as_f64()`, which successfully parses a `serde_json::Value::Number`
//! for both integer and float JSON values — but the column *type* was not
//! checked, so integer fields received bounds checking while float columns
//! (which arrive from certain callers as `JsonValue::Number(f64)`) were
//! not validated because the match arm for `Real`/`Double` in the integer-
//! focused validation block was absent.
//!
//! Post-fix: a float field declared with `min`/`max`:
//!   - REJECTS a value below `min` with `WriteError::Validator { field, .. }`.
//!   - REJECTS a value above `max` with the same error.
//!   - ACCEPTS a value in [min, max] (inclusive, same semantics as integers).
//!
//! Covers `SqlType::Double` (f64) and `SqlType::Real` (f32), both
//! `insert_json` and `update_json` paths.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use umbral::orm::{DynQuerySet, FkAction, SqlType, write::WriteError};
use umbral_core::db;
use umbral_core::migrate::{Column, ModelMeta};

// ---------------------------------------------------------------------------
// Helpers to build synthetic Column / ModelMeta descriptors.
// Following the exact same pattern as tests/rename_detection.rs.
// ---------------------------------------------------------------------------

fn id_col() -> Column {
    Column {
        name: "id".to_string(),
        ty: SqlType::BigInt,
        primary_key: true,
        nullable: false,
        fk_target: None,
        noform: false,
        privileged: false,
        private: false,
        secret: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: FkAction::NoAction,
        on_update: FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        case_insensitive: false,
        help: String::new(),
        example: String::new(),
        widget: None,
        supported_backends: Vec::new(),
        min: None,
        max: None,
        text_format: None,
        slug_from: None,
    }
}

fn float_col(name: &str, ty: SqlType, min: Option<i64>, max: Option<i64>) -> Column {
    Column {
        name: name.to_string(),
        ty,
        primary_key: false,
        nullable: false,
        fk_target: None,
        noform: false,
        privileged: false,
        private: false,
        secret: false,
        db_constraint: true,
        noedit: false,
        auto_user_add: false,
        auto_user: false,
        is_string_repr: false,
        max_length: 0,
        choices: Vec::new(),
        choice_labels: Vec::new(),
        default: String::new(),
        is_multichoice: false,
        unique: false,
        on_delete: FkAction::NoAction,
        on_update: FkAction::NoAction,
        index: false,
        auto_now_add: false,
        auto_now: false,
        trim: false,
        lowercase: false,
        case_insensitive: false,
        help: String::new(),
        example: String::new(),
        widget: None,
        supported_backends: Vec::new(),
        min,
        max,
        text_format: None,
        slug_from: None,
    }
}

fn make_meta(name: &str, table: &str, cols: Vec<Column>) -> ModelMeta {
    ModelMeta {
        view: None,
        materialized: false,
        name: name.to_string(),
        table: table.to_string(),
        fields: cols,
        display: name.to_string(),
        icon: "database".to_string(),
        database: None,
        singleton: false,
        unique_together: Vec::new(),
        indexes: Vec::new(),
        ordering: Vec::new(),
        m2m_relations: Vec::new(),
        soft_delete: false,
        audited: false,
        app_label: "app".to_string(),
    }
}

// ---------------------------------------------------------------------------
// The Product model registered with the App so DynQuerySet can dispatch.
// The hand-built `ModelMeta` below overrides the macro-derived one for
// validation tests, while the App registration gives us a live DB pool.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "flt_product")]
pub struct Product {
    pub id: i64,
    pub score: f64,
    pub rating: f32,
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

        sqlx::query(
            "CREATE TABLE flt_product (\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\
                 score REAL NOT NULL,\
                 rating REAL NOT NULL\
             )",
        )
        .execute(&pool)
        .await
        .expect("create table");
    })
    .await;
}

/// `ModelMeta` for `flt_product` with explicit bounds:
///   - score (Double): min=0, max=100
///   - rating (Real):  min=1, max=10
fn product_meta_with_bounds() -> ModelMeta {
    make_meta(
        "Product",
        "flt_product",
        vec![
            id_col(),
            float_col("score", SqlType::Double, Some(0), Some(100)),
            float_col("rating", SqlType::Real, Some(1), Some(10)),
        ],
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Double field with `min=0` rejects a negative value via `insert_json`.
#[tokio::test]
async fn double_below_min_rejected_on_insert_json() {
    boot().await;
    let meta = product_meta_with_bounds();
    let body = serde_json::json!({"score": -1.0, "rating": 5.0})
        .as_object()
        .cloned()
        .unwrap();

    let err = DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect_err("score=-1.0 is below min=0; insert_json must reject it");

    match err {
        WriteError::Validator { field, message } => {
            assert_eq!(field, "score", "error must name the offending field");
            assert!(
                message.contains(">= 0") || message.contains("0"),
                "error must mention the bound; got: {message}"
            );
        }
        other => panic!("expected WriteError::Validator for below-min Double, got: {other:?}"),
    }
}

/// Double field with `max=100` rejects a value above the bound.
#[tokio::test]
async fn double_above_max_rejected_on_insert_json() {
    boot().await;
    let meta = product_meta_with_bounds();
    let body = serde_json::json!({"score": 101.5, "rating": 5.0})
        .as_object()
        .cloned()
        .unwrap();

    let err = DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect_err("score=101.5 is above max=100; insert_json must reject it");

    match err {
        WriteError::Validator { field, message } => {
            assert_eq!(field, "score");
            assert!(
                message.contains("<= 100") || message.contains("100"),
                "error must mention the bound; got: {message}"
            );
        }
        other => panic!("expected WriteError::Validator for above-max Double, got: {other:?}"),
    }
}

/// Real field with `min=1` rejects a value below 1.0.
#[tokio::test]
async fn real_below_min_rejected_on_insert_json() {
    boot().await;
    let meta = product_meta_with_bounds();
    let body = serde_json::json!({"score": 50.0, "rating": 0.5})
        .as_object()
        .cloned()
        .unwrap();

    let err = DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect_err("rating=0.5 is below min=1; insert_json must reject it");

    match err {
        WriteError::Validator { field, .. } => {
            assert_eq!(field, "rating");
        }
        other => panic!("expected WriteError::Validator for below-min Real, got: {other:?}"),
    }
}

/// Real field with `max=10` rejects a value above 10.0.
#[tokio::test]
async fn real_above_max_rejected_on_insert_json() {
    boot().await;
    let meta = product_meta_with_bounds();
    let body = serde_json::json!({"score": 50.0, "rating": 10.1})
        .as_object()
        .cloned()
        .unwrap();

    let err = DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect_err("rating=10.1 is above max=10; insert_json must reject it");

    match err {
        WriteError::Validator { field, .. } => {
            assert_eq!(field, "rating");
        }
        other => panic!("expected WriteError::Validator for above-max Real, got: {other:?}"),
    }
}

/// In-range values are accepted and written to the DB.
#[tokio::test]
async fn float_in_range_accepted_on_insert_json() {
    boot().await;
    let meta = product_meta_with_bounds();
    // score=50.0 ∈ [0, 100], rating=5.0 ∈ [1, 10] — both in range.
    let body = serde_json::json!({"score": 50.0, "rating": 5.0})
        .as_object()
        .cloned()
        .unwrap();

    DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect("in-range float values must be accepted");
}

/// Boundary values are inclusive: score=0.0 and score=100.0 are both valid.
#[tokio::test]
async fn float_at_exact_boundary_accepted_on_insert_json() {
    boot().await;
    let meta = product_meta_with_bounds();

    let body_min = serde_json::json!({"score": 0.0, "rating": 1.0})
        .as_object()
        .cloned()
        .unwrap();
    DynQuerySet::for_meta(&meta)
        .insert_json(&body_min)
        .await
        .expect("score=0.0 (== min) must be accepted");

    let body_max = serde_json::json!({"score": 100.0, "rating": 10.0})
        .as_object()
        .cloned()
        .unwrap();
    DynQuerySet::for_meta(&meta)
        .insert_json(&body_max)
        .await
        .expect("score=100.0 (== max) must be accepted");
}

/// update_json must also enforce float min/max.
#[tokio::test]
async fn double_above_max_rejected_on_update_json() {
    boot().await;
    let meta = product_meta_with_bounds();

    // Insert a valid row first.
    let body = serde_json::json!({"score": 50.0, "rating": 5.0})
        .as_object()
        .cloned()
        .unwrap();
    DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect("initial insert must succeed");

    // Now try to update score to 200.0 — above max=100.
    let patch = serde_json::json!({"score": 200.0})
        .as_object()
        .cloned()
        .unwrap();
    let err = DynQuerySet::for_meta(&meta)
        .filter_eq_string("id", "1")
        .update_json(&patch)
        .await
        .expect_err("score=200.0 is above max=100; update_json must reject it");

    match err {
        WriteError::Validator { field, .. } => {
            assert_eq!(field, "score");
        }
        other => panic!("expected WriteError::Validator for above-max update, got: {other:?}"),
    }
}
