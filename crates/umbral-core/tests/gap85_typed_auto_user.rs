//! gap85: the TYPED write path must freeze `#[umbral(auto_user_add)]` and
//! refresh `#[umbral(auto_user)]` on UPDATE, not just on the dynamic
//! (REST/admin JSON+form) path and the typed INSERT path. Sibling of gap68
//! (`auto_now_add` / `auto_now`), same contract, but the value comes from the
//! ambient caller identity (`umbral::db::route_context_scope` /
//! `RouteContext::with_user`) rather than from `now()`:
//!
//! - `auto_user_add` = stamp the caller on INSERT, frozen forever after (an
//!   UPDATE never rewrites it, even though the struct still carries a value —
//!   possibly a bogus one, since a client should never be able to forge
//!   authorship by round-tripping someone else's id back through an update).
//! - `auto_user` = stamp the CURRENT ambient caller on every save/update,
//!   ignoring whatever the struct carried.
//!
//! Covers both typed UPDATE entry points touched by the fix: `save()` and
//! `update_values()`.

#![allow(dead_code)]

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::{Mutex, OnceCell};
use umbral::db::RouteContext;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "gap85_note")]
pub struct Note {
    pub id: i64,
    /// Stamped once, on create.
    #[umbral(auto_user_add)]
    pub created_by: Option<i64>,
    /// Re-stamped on every write.
    #[umbral(auto_user)]
    pub updated_by: Option<i64>,
    pub label: String,
}

static SERIALISE: Mutex<()> = Mutex::const_new(());
static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("gap85.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        let _app = umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<Note>()
            .build()
            .expect("App::build");

        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
    })
    .await;
}

async fn clear() {
    let pool = umbral::db::pool();
    sqlx::query("DELETE FROM gap85_note")
        .execute(&pool)
        .await
        .expect("clear");
}

fn ctx_for(user_id: i64) -> RouteContext {
    RouteContext::new().with_user(user_id.to_string())
}

/// A typed `create` stamps BOTH columns from the ambient caller — the who-did-
/// it twin of gap68's `now()` stamping.
#[tokio::test]
async fn typed_create_stamps_the_ambient_caller() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let row = umbral::db::route_context_scope(ctx_for(7), async {
        Note::objects()
            .create(Note {
                id: 0,
                created_by: None,
                updated_by: None,
                label: "first".to_string(),
            })
            .await
            .expect("create")
    })
    .await;

    assert_eq!(row.created_by, Some(7), "created_by stamped from caller");
    assert_eq!(row.updated_by, Some(7), "updated_by stamped from caller");
}

/// A typed `save` (UPDATE) refreshes `auto_user` to whoever is CURRENTLY the
/// ambient caller, while `auto_user_add` stays frozen at the original author —
/// even though the in-memory struct carries a bogus, forged value for both.
#[tokio::test]
async fn typed_save_freezes_auto_user_add_and_refreshes_auto_user() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let created = umbral::db::route_context_scope(ctx_for(7), async {
        Note::objects()
            .create(Note {
                id: 0,
                created_by: None,
                updated_by: None,
                label: "v1".to_string(),
            })
            .await
            .expect("create")
    })
    .await;
    assert_eq!(created.created_by, Some(7));
    assert_eq!(created.updated_by, Some(7));

    // A DIFFERENT user (8) edits the row. The in-memory struct also forges
    // both stamp columns to a THIRD id (99) — the framework must ignore the
    // struct entirely for these two columns.
    let mut edited = created.clone();
    edited.label = "v2".to_string();
    edited.created_by = Some(99);
    edited.updated_by = Some(99);

    let saved = umbral::db::route_context_scope(ctx_for(8), async {
        Note::objects().save(edited).await.expect("save/update")
    })
    .await;

    assert_eq!(saved.label, "v2");
    assert_eq!(
        saved.created_by,
        Some(7),
        "auto_user_add moved on update (should be frozen at the original author)"
    );
    assert_eq!(
        saved.updated_by,
        Some(8),
        "auto_user did not refresh to the current ambient caller"
    );

    // Read the row back independently to prove it's the stored state, not
    // just the RETURNING image.
    let fetched = Note::objects()
        .filter(note::ID.eq(saved.id))
        .first()
        .await
        .expect("refetch")
        .expect("row exists");
    assert_eq!(fetched.created_by, Some(7));
    assert_eq!(fetched.updated_by, Some(8));
}

/// Same contract via `update_values()` (the `build_update_for` path shared with
/// `update_or_create`'s `defaults` struct): a caller-supplied map naming
/// `created_by` must not move it, and `updated_by` refreshes to the ambient
/// caller even when the map doesn't mention it at all.
#[tokio::test]
async fn typed_update_values_freezes_add_and_refreshes_user() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let created = umbral::db::route_context_scope(ctx_for(7), async {
        Note::objects()
            .create(Note {
                id: 0,
                created_by: None,
                updated_by: None,
                label: "v1".to_string(),
            })
            .await
            .expect("create")
    })
    .await;

    let mut values = serde_json::Map::new();
    values.insert("label".to_string(), serde_json::json!("v2"));
    // Forge an attempt to move the original author via update_values directly.
    values.insert("created_by".to_string(), serde_json::json!(99));
    values.insert("updated_by".to_string(), serde_json::json!(99));

    umbral::db::route_context_scope(ctx_for(8), async {
        Note::objects()
            .filter(note::ID.eq(created.id))
            .update_values(values)
            .await
            .expect("update_values")
    })
    .await;

    let fetched = Note::objects()
        .filter(note::ID.eq(created.id))
        .first()
        .await
        .expect("refetch")
        .expect("row exists");
    assert_eq!(fetched.label, "v2");
    assert_eq!(
        fetched.created_by,
        Some(7),
        "auto_user_add moved via update_values (should be frozen)"
    );
    assert_eq!(
        fetched.updated_by,
        Some(8),
        "auto_user did not refresh to the ambient caller via update_values"
    );
}

/// No ambient caller (a background job, the CLI) → NULL on both stamps at
/// create, and `auto_user` refreshes to NULL (not left at the previous value)
/// on an update performed with no caller in scope — mirrors the dynamic
/// path's "no guess, no invented author" contract.
#[tokio::test]
async fn typed_update_with_no_ambient_caller_stamps_null() {
    boot().await;
    let _g = SERIALISE.lock().await;
    clear().await;

    let created = umbral::db::route_context_scope(ctx_for(7), async {
        Note::objects()
            .create(Note {
                id: 0,
                created_by: None,
                updated_by: None,
                label: "v1".to_string(),
            })
            .await
            .expect("create")
    })
    .await;
    assert_eq!(created.updated_by, Some(7));

    // No route_context_scope here: runs with the default (no-user) context.
    let mut edited = created.clone();
    edited.label = "v2".to_string();
    let saved = Note::objects().save(edited).await.expect("save/update");

    assert_eq!(saved.created_by, Some(7), "auto_user_add still frozen");
    assert_eq!(
        saved.updated_by, None,
        "no ambient caller must stamp NULL, not keep the stale value"
    );
}
