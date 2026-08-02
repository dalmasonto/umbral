//! `#[umbral(auto_uuid)]` — the framework generates a fresh random v4 UUID for
//! a column on create when the caller omits it (or leaves the nil UUID), on
//! BOTH the typed `Manager::create` path and the dynamic (REST/admin) path. A
//! public, opaque, non-sequential id that works identically on SQLite and
//! Postgres (unlike a Postgres-only `gen_random_uuid()` DDL default).

use sqlx::SqlitePool;
use tokio::sync::OnceCell;
use umbral::migrate::ModelMeta;
use umbral::orm::DynQuerySet;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "auto_uuid_doc")]
pub struct Doc {
    id: i64,
    #[umbral(auto_uuid)]
    public_id: Uuid,
    title: String,
}

// `App::build*()` publishes process-global `OnceLock`s and panics on a second
// call, so the whole binary shares one boot.
static BOOT: OnceCell<SqlitePool> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let pool = umbral::db::connect_sqlite("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let mut settings = umbral::Settings::from_env().expect("settings");
        settings.database_url = "sqlite::memory:".to_string();
        umbral::App::builder()
            .settings(settings)
            .database("default", pool.clone())
            .model::<Doc>()
            .build_deferred()
            .expect("App::build_deferred");
        umbral_core::migrate::create_tables_for_tests()
            .await
            .expect("create the test schema");
        pool
    })
    .await;
}

/// Typed path: a struct that leaves `public_id` at the nil UUID (what
/// `Default`/an un-set field carries) gets a real random UUID on create.
#[tokio::test]
async fn typed_create_generates_a_uuid_when_omitted() {
    boot().await;
    let created = Doc::objects()
        .create(Doc {
            id: 0,
            public_id: Uuid::nil(),
            title: "typed-omitted".into(),
        })
        .await
        .expect("create");
    assert_ne!(
        created.public_id,
        Uuid::nil(),
        "auto_uuid must generate a real UUID on the typed path when the field is nil",
    );
}

/// Typed path: an explicitly-chosen non-nil UUID is kept, never overwritten.
#[tokio::test]
async fn typed_create_keeps_an_explicit_uuid() {
    boot().await;
    let chosen = Uuid::new_v4();
    let created = Doc::objects()
        .create(Doc {
            id: 0,
            public_id: chosen,
            title: "typed-explicit".into(),
        })
        .await
        .expect("create");
    assert_eq!(
        created.public_id, chosen,
        "an explicitly-supplied UUID must be kept, not regenerated",
    );
}

/// Dynamic path (what the admin / REST create runs on): the body omits
/// `public_id` entirely, and the framework fills it — the whole point, since a
/// Uuid column has no sensible form field to make the operator type by hand.
#[tokio::test]
async fn dynamic_create_generates_a_uuid_when_absent() {
    boot().await;
    let meta = ModelMeta::for_::<Doc>();
    let mut body = serde_json::Map::new();
    body.insert("title".into(), serde_json::json!("dynamic-absent"));

    let row = DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect("dynamic insert with public_id absent must succeed, not violate NOT NULL");

    let pid = row
        .get("public_id")
        .and_then(|v| v.as_str())
        .expect("public_id present in the returned row");
    assert_ne!(
        pid, "00000000-0000-0000-0000-000000000000",
        "auto_uuid must generate a real UUID on the dynamic path; got {pid}",
    );
    Uuid::parse_str(pid).expect("the generated public_id is a valid UUID");
}
