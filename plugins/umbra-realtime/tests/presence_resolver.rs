//! Identity-projection proof, custom-resolver half: with a resolver returning
//! `{id, name}`, the presence payload carries **exactly** those keys — the dev's
//! explicit choice of what's safe to broadcast. (The id-only default lives in
//! `presence.rs`; one app per binary, since the ambient handle is a `OnceLock`.)

#![allow(dead_code)]

use std::collections::HashSet;

use umbra_realtime::{DEFAULT_BUFFER, Event, PresenceSpec, Realtime, RealtimePlugin, dispatch_presence};

async fn boot(plugin: RealtimePlugin) {
    umbra::signals::clear_for_tests();
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();
    umbra::App::builder()
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

#[tokio::test]
async fn custom_resolver_projects_exactly_what_it_returns() {
    // The dev's resolver maps a user id to {id, name} — and ONLY that. A real
    // app would look the name up; here we synthesize it to prove the wire
    // carries exactly the resolver's output, no more.
    boot(RealtimePlugin::new().with_presence(
        PresenceSpec::prefixes(["room:"]).resolver(|uid| {
            serde_json::json!({ "id": uid, "name": format!("user-{uid}") })
        }),
    ))
    .await;
    let registry = Realtime::registry();

    // A subscriber already present, then user 5 joins.
    let (_sub_id, mut sub, _t) = registry
        .register_with_presence(None, group_set("room:7"), DEFAULT_BUFFER)
        .await
        .expect("subscriber admitted");
    let (_id, _rx, transitions) = registry
        .register_with_presence(Some(5), group_set("room:7"), DEFAULT_BUFFER)
        .await
        .expect("conn admitted");
    dispatch_presence(transitions).await;

    let mut events: Vec<Event> = Vec::new();
    while let Ok(ev) = sub.try_recv() {
        events.push(ev);
    }
    let join = events
        .iter()
        .find(|ev| ev.event == "presence:join")
        .expect("a presence:join reached the subscriber");
    let obj = join.data.as_object().expect("member is an object");
    assert_eq!(obj.len(), 2, "resolver output carries exactly {{id, name}}");
    assert_eq!(obj.get("id").and_then(|v| v.as_i64()), Some(5));
    assert_eq!(obj.get("name").and_then(|v| v.as_str()), Some("user-5"));
    // No key the resolver didn't return.
    assert!(!obj.contains_key("email"), "nothing the resolver didn't return leaks");
}
