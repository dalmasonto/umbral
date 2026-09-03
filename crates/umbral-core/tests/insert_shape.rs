//! gaps4 #88 — `#[derive(New)]` generates a partial insert shape `<Model>New`
//! that omits ORM-auto-managed fields, and `create`/`get_or_create`/
//! `update_or_create` accept it via `impl Into<T>`.
//!
//! Behavioral: real rows through the real public path. The fact that
//! `WidgetNew { name: … }` even COMPILES — with no `id`, `created_at`, or
//! `updated_at` — is itself the type-level proof those fields are omitted; the
//! round-trips then prove the write path stamps them.

#![allow(dead_code)]

use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as TokioMutex, OnceCell};

use umbral_core::db;

fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

#[derive(
    Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model, umbral::orm::New,
)]
#[umbral(table = "is_widget")]
pub struct Widget {
    pub id: i64,
    #[umbral(string, unique)]
    pub name: String,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let dir = std::env::temp_dir();
        let path = dir.join(format!("umbral_insert_shape_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = db::connect_sqlite(&url).await.expect("file-backed sqlite");
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

/// `create(<Model>New { … })` inserts a row naming ONLY the user data; the PK is
/// assigned and both auto timestamps are stamped by the write path (not the
/// epoch a `Default` would leave).
#[tokio::test]
async fn create_from_new_shape_assigns_pk_and_stamps_auto_fields() {
    let _g = test_lock().lock().await;
    boot().await;

    // No id / created_at / updated_at named here — that this compiles is the
    // type-level omission proof.
    let created = Widget::objects()
        .create(WidgetNew {
            name: "gizmo".into(),
        })
        .await
        .expect("create from insert shape");

    assert!(created.id > 0, "autoincrement PK assigned");
    assert_eq!(created.name, "gizmo");
    assert!(
        created.created_at.timestamp() > 0,
        "auto_now_add stamped a real time, not the Default epoch"
    );
    assert!(created.updated_at.timestamp() > 0, "auto_now stamped");

    // Round-trips through a real read.
    let got = Widget::objects()
        .filter(widget::ID.eq(created.id))
        .first()
        .await
        .unwrap()
        .expect("read back");
    assert_eq!(got.name, "gizmo");
    assert_eq!(got.id, created.id);
}

/// A full `T` still works through the same `impl Into<T>` argument — `T: Into<T>`
/// is identity, so this is a pure backward-compatibility guard.
#[tokio::test]
async fn create_still_accepts_a_full_model() {
    let _g = test_lock().lock().await;
    boot().await;

    let created = Widget::objects()
        .create(Widget {
            id: 0,
            name: "full-model".into(),
            created_at: Default::default(),
            updated_at: Default::default(),
        })
        .await
        .expect("create from full model");
    assert!(created.id > 0);
    assert_eq!(created.name, "full-model");
}

/// `get_or_create` accepts the insert shape as its `defaults`: first call
/// inserts, second converges on the same row.
#[tokio::test]
async fn get_or_create_accepts_the_insert_shape() {
    let _g = test_lock().lock().await;
    boot().await;

    let (first, created) = Widget::objects()
        .get_or_create(
            widget::NAME.eq("goc-name"),
            WidgetNew {
                name: "goc-name".into(),
            },
        )
        .await
        .expect("get_or_create insert");
    assert!(created, "first call inserts");

    let (again, created2) = Widget::objects()
        .get_or_create(
            widget::NAME.eq("goc-name"),
            WidgetNew {
                name: "goc-name".into(),
            },
        )
        .await
        .expect("get_or_create hit");
    assert!(!created2, "second call finds the existing row");
    assert_eq!(first.id, again.id, "same row, not a duplicate");
}

/// `update_or_create` accepts the insert shape too.
#[tokio::test]
async fn update_or_create_accepts_the_insert_shape() {
    let _g = test_lock().lock().await;
    boot().await;

    let (row, created) = Widget::objects()
        .update_or_create(
            widget::NAME.eq("uoc-name"),
            WidgetNew {
                name: "uoc-name".into(),
            },
        )
        .await
        .expect("update_or_create insert");
    assert!(created);
    assert!(row.id > 0);
    assert_eq!(row.name, "uoc-name");
}
