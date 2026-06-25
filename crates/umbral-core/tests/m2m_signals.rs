//! Gap 38 — `m2m_changed:<junction>` signals from M2M mutations.
//!
//! `M2M::add` / `remove` / `set` / `clear` each emit one event with
//! the junction-table name, action, parent_id, and the lists of
//! added/removed child PKs. `set` reports the SQL-level reality —
//! prior children are removed (the DB DELETE wiped them) and supplied
//! children are added (the DB INSERTs re-built the set). Consumers
//! that want a "diff" view compute it in their handler from those two
//! lists.

#![allow(dead_code, private_interfaces)]

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

use umbral::orm::M2M;
use umbral_core::signals::{clear_for_tests, subscribe};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(plugin = "m2msig")]
pub struct SigGroup {
    pub id: i64,
    pub name: String,
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: M2M<SigTag>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(plugin = "m2msig")]
pub struct SigTag {
    pub id: i64,
    pub label: String,
}

/// Snake-case of the parent struct + field name + plugin prefix.
const JUNCTION_TABLE: &str = "m2msig_sig_group_tags";
const SIGNAL_NAME: &str = "m2m_changed:m2msig_sig_group_tags";

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("m2m_signals.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");

        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<SigGroup>()
            .model::<SigTag>()
            .build()
            .expect("App::build");

        let migration_tmp = tempfile::tempdir().expect("migration tempdir");
        let migration_path = migration_tmp.path().to_path_buf();
        std::mem::forget(migration_tmp);
        umbral::migrate::make_in(&migration_path)
            .await
            .expect("make_in");
        umbral::migrate::run_in(&migration_path)
            .await
            .expect("run_in");
    })
    .await;
}

fn capture() -> Arc<Mutex<Vec<Value>>> {
    let captured: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let c = captured.clone();
    subscribe(SIGNAL_NAME, move |p| c.lock().unwrap().push(p.clone()));
    captured
}

async fn fresh_group(name: &str) -> SigGroup {
    SigGroup::objects()
        .create(SigGroup {
            id: 0,
            name: name.into(),
            tags: M2M::empty(),
        })
        .await
        .expect("create group")
}

async fn fresh_tag(label: &str) -> SigTag {
    SigTag::objects()
        .create(SigTag {
            id: 0,
            label: label.into(),
        })
        .await
        .expect("create tag")
}

// Per-test serialisation — boot is shared, but the signal registry is
// process-wide so concurrent subscriptions race.
use tokio::sync::Mutex as TokioMutex;
static SERIALISE: TokioMutex<()> = TokioMutex::const_new(());

#[tokio::test]
async fn add_emits_with_added_child_pk() {
    let _g = SERIALISE.lock().await;
    boot().await;
    clear_for_tests();
    let captured = capture();

    let group = fresh_group("add-grp").await;
    let tag = fresh_tag("add-tag").await;
    group.tags.add(&tag).await.expect("add");

    let events = captured.lock().unwrap();
    assert_eq!(events.len(), 1);
    let p = &events[0];
    assert_eq!(p["action"], json!("add"));
    assert_eq!(p["parent_id"], json!(group.id));
    assert_eq!(p["added"], json!([tag.id]));
    assert_eq!(p["removed"], json!([]));
    assert!(p.get("actor").is_some());
}

#[tokio::test]
async fn remove_emits_with_removed_child_pk() {
    let _g = SERIALISE.lock().await;
    boot().await;
    clear_for_tests();

    let group = fresh_group("rm-grp").await;
    let tag = fresh_tag("rm-tag").await;
    group.tags.add(&tag).await.expect("add");

    // Subscribe AFTER the add so we only see the remove event.
    let captured = capture();
    group.tags.remove(&tag).await.expect("remove");

    let events = captured.lock().unwrap();
    assert_eq!(events.len(), 1);
    let p = &events[0];
    assert_eq!(p["action"], json!("remove"));
    assert_eq!(p["parent_id"], json!(group.id));
    assert_eq!(p["added"], json!([]));
    assert_eq!(p["removed"], json!([tag.id]));
}

#[tokio::test]
async fn set_emits_prior_in_removed_and_supplied_in_added() {
    let _g = SERIALISE.lock().await;
    boot().await;
    clear_for_tests();

    let group = fresh_group("set-grp").await;
    let prior = fresh_tag("set-prior").await;
    let new_a = fresh_tag("set-a").await;
    let new_b = fresh_tag("set-b").await;
    group.tags.add(&prior).await.expect("seed add");

    let captured = capture();
    group.tags.set(&[&new_a, &new_b]).await.expect("set");

    let events = captured.lock().unwrap();
    assert_eq!(events.len(), 1);
    let p = &events[0];
    assert_eq!(p["action"], json!("set"));
    assert_eq!(p["parent_id"], json!(group.id));
    let added: Vec<i64> = p["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    let mut want_added = vec![new_a.id, new_b.id];
    want_added.sort();
    let mut got_added = added;
    got_added.sort();
    assert_eq!(got_added, want_added);
    let removed: Vec<i64> = p["removed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    assert_eq!(removed, vec![prior.id]);
}

#[tokio::test]
async fn clear_emits_prior_in_removed() {
    let _g = SERIALISE.lock().await;
    boot().await;
    clear_for_tests();

    let group = fresh_group("clear-grp").await;
    let tag_a = fresh_tag("clear-a").await;
    let tag_b = fresh_tag("clear-b").await;
    group.tags.add(&tag_a).await.expect("a");
    group.tags.add(&tag_b).await.expect("b");

    let captured = capture();
    let removed_count = group.tags.clear().await.expect("clear");
    assert_eq!(removed_count, 2);

    let events = captured.lock().unwrap();
    assert_eq!(events.len(), 1);
    let p = &events[0];
    assert_eq!(p["action"], json!("clear"));
    assert_eq!(p["parent_id"], json!(group.id));
    assert_eq!(p["added"], json!([]));
    let removed: Vec<i64> = p["removed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    let mut got = removed;
    got.sort();
    let mut want = vec![tag_a.id, tag_b.id];
    want.sort();
    assert_eq!(got, want);
}

#[tokio::test]
async fn clear_on_empty_relation_does_not_emit() {
    let _g = SERIALISE.lock().await;
    boot().await;
    clear_for_tests();

    let group = fresh_group("clear-empty").await;
    let captured = capture();
    let n = group.tags.clear().await.expect("clear");
    assert_eq!(n, 0);
    assert!(captured.lock().unwrap().is_empty());
}
