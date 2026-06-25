//! Signals bridge: a model's post_save / post_delete fans out to real-time
//! clients with zero polling. We emit the ORM signal directly (the exact
//! call the write path makes) and assert the bridge ran the handler.

use std::collections::HashSet;

use umbral_realtime::{DEFAULT_BUFFER, Realtime, RealtimePlugin};

async fn boot() {
    // Fresh signal registry so a stray subscription can't leak in.
    umbral::signals::clear_for_tests();

    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();

    // Bridge: any create/update/delete on `note` pushes a `note_changed`
    // event (the whole ModelEvent) to the `note-watchers` group.
    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(RealtimePlugin::default().on_table("note", |ev| async move {
            Realtime::to_group("note-watchers")
                .send("note_changed", &ev)
                .await;
        }))
        .build()
        .expect("App::build");
}

#[tokio::test]
async fn on_table_fans_out_post_save_and_post_delete() {
    boot().await;

    // A watcher connection joined directly (register bypasses the policy,
    // which the transports apply — here we just need a sink in the group).
    let registry = Realtime::registry();
    let mut groups = HashSet::new();
    groups.insert("note-watchers".to_string());
    let (_id, mut rx) = registry
        .register(None, groups, DEFAULT_BUFFER)
        .await
        .expect("registration admitted (no connection cap)");

    // 1. A post_save (created) — emitted exactly as Manager::create would.
    umbral::signals::emit(
        "post_save:note",
        serde_json::json!({ "instance": { "id": 7, "body": "hello" }, "created": true }),
    )
    .await;

    let got = rx.try_recv().expect("post_save fanned out to the watcher");
    assert_eq!(got.event, "note_changed");
    let data = got.data.to_string();
    assert!(data.contains("created"), "action is created; got {data}");
    assert!(data.contains("hello"), "the row is carried; got {data}");

    // 2. A post_delete.
    umbral::signals::emit(
        "post_delete:note",
        serde_json::json!({ "instance": { "id": 7, "body": "hello" } }),
    )
    .await;

    let got = rx
        .try_recv()
        .expect("post_delete fanned out to the watcher");
    let data = got.data.to_string();
    assert!(data.contains("deleted"), "action is deleted; got {data}");

    // 3. A signal for a DIFFERENT table is ignored by this bridge.
    umbral::signals::emit(
        "post_save:other",
        serde_json::json!({ "instance": { "id": 1 }, "created": true }),
    )
    .await;
    assert!(
        rx.try_recv().is_err(),
        "a different table's signal does not fan out here"
    );
}
