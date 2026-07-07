//! audit_2 realtime #5 — the `presence:sync` roster snapshot is delivered ONLY
//! to the joining user's connection, not re-broadcast to the whole group on
//! every join. Existing members track the roster from the `presence:join` /
//! `presence:leave` deltas they already receive, so a join storm is O(N), not
//! O(N²). This proves the recipient scoping (one app per binary — the ambient
//! handle is a `OnceLock`).

#![allow(dead_code)]

use std::collections::HashSet;

use umbral_realtime::{
    DEFAULT_BUFFER, Event, PresenceSpec, Realtime, RealtimePlugin, dispatch_presence,
};

async fn boot(plugin: RealtimePlugin) {
    umbral::signals::clear_for_tests();
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();
    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(plugin)
        .build()
        .expect("App::build");
}

fn group_set(g: &str) -> HashSet<String> {
    let mut s = HashSet::new();
    s.insert(g.to_string());
    s
}

fn drain(rx: &mut tokio::sync::mpsc::Receiver<Event>) -> Vec<String> {
    let mut names = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        names.push(ev.event);
    }
    names
}

#[tokio::test]
async fn sync_goes_to_the_joiner_not_the_whole_group() {
    boot(RealtimePlugin::new().with_presence(PresenceSpec::prefixes(["room:"]))).await;
    let registry = Realtime::registry();

    // An existing member of the room (user "1"), holding its own receiver.
    let (_existing_id, mut existing_rx, first_transitions) = registry
        .register_with_presence(Some("1".to_string()), group_set("room:9"), DEFAULT_BUFFER)
        .await
        .expect("existing member admitted");
    // Dispatch (and drain) user 1's own join — it received its own sync as the
    // first member. We only care about what happens on the NEXT user's join.
    dispatch_presence(first_transitions).await;
    let _ = drain(&mut existing_rx);

    // User "2" joins the same room, holding its own receiver.
    let (_joiner_id, mut joiner_rx, join_transitions) = registry
        .register_with_presence(Some("2".to_string()), group_set("room:9"), DEFAULT_BUFFER)
        .await
        .expect("joiner admitted");
    dispatch_presence(join_transitions).await;

    // The existing member learns about the newcomer via the `presence:join`
    // delta — but is NOT re-sent the full roster. That absence is the fix: no
    // O(N²) whole-group re-sync on every join.
    let existing_saw = drain(&mut existing_rx);
    assert!(
        existing_saw.contains(&"presence:join".to_string()),
        "existing member must receive the join delta; got {existing_saw:?}"
    );
    assert!(
        !existing_saw.contains(&"presence:sync".to_string()),
        "existing member must NOT be re-sent the full roster on someone else's join; \
         got {existing_saw:?}"
    );

    // The joining connection DOES receive its initial roster snapshot.
    let joiner_saw = drain(&mut joiner_rx);
    assert!(
        joiner_saw.contains(&"presence:sync".to_string()),
        "the joining connection must receive its initial presence:sync roster; \
         got {joiner_saw:?}"
    );
}
