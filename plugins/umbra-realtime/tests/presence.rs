//! Security proof for `RealtimePlugin::with_presence` — gated, identity-projected
//! "who's online in a group".
//!
//! These tests are the deliverable: presence exposes *user identity*, so they
//! prove it never leaks more than the dev allowed. We drive the exact registry
//! seam the SSE/WS transports use (`register_with_presence` /
//! `deregister_with_presence` → `dispatch_presence`) and assert what reaches a
//! subscriber: that a group without presence enabled is silent (default-off),
//! that the default projection is `{id}`-only (no name/email leak), that
//! membership dedups by user (first-join / last-leave), that anonymous
//! connections never appear, and that `GroupPolicy::can_join` still governs who
//! can see presence. The custom-resolver projection is proved in
//! `presence_resolver.rs` (a second app, since the ambient handle is a
//! process-global `OnceLock` — one boot per binary).

#![allow(dead_code)]

use std::collections::HashSet;

use umbra_realtime::{
    DEFAULT_BUFFER, Event, PresenceSpec, PublicGroupsOnly, Realtime, RealtimePlugin, Registry,
    dispatch_presence,
};

/// Boot one app with the given plugin. One per test binary (the ambient
/// `Realtime` is a `OnceLock`).
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

/// A passive anonymous subscriber sink in `group` (anonymous → never itself
/// produces presence), to read what dispatch delivered.
async fn watch(group: &str) -> tokio::sync::mpsc::Receiver<Event> {
    let (_id, rx, _t) = Realtime::registry()
        .register_with_presence(None, group_set(group), DEFAULT_BUFFER)
        .await
        .expect("subscriber admitted");
    rx
}

/// Register a conn as `user_id` in `group` and dispatch its presence
/// transitions — exactly the SSE/WS connect path. Returns the conn id.
async fn connect(registry: &Registry, user_id: Option<i64>, group: &str) -> u64 {
    let (id, _rx, transitions) = registry
        .register_with_presence(user_id, group_set(group), DEFAULT_BUFFER)
        .await
        .expect("conn admitted");
    dispatch_presence(transitions).await;
    id
}

/// Deregister a conn and dispatch its last-leave transitions (the `ConnGuard` path).
async fn disconnect(registry: &Registry, id: u64) {
    let transitions = registry.deregister_with_presence(id).await;
    dispatch_presence(transitions).await;
}

/// Drain every queued event off `rx` as `(event_name, data)`.
fn drain(rx: &mut tokio::sync::mpsc::Receiver<Event>) -> Vec<(String, serde_json::Value)> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push((ev.event, ev.data));
    }
    out
}

fn names(events: &[(String, serde_json::Value)]) -> Vec<&str> {
    events.iter().map(|(n, _)| n.as_str()).collect()
}

/// The single shared spec for this binary: presence enabled for `room:*` only,
/// default (id-only) projection.
fn plugin() -> RealtimePlugin {
    RealtimePlugin::new().with_presence(PresenceSpec::prefixes(["room:"]))
}

// =========================================================================
// 1-4 in one booting test. `App::build()` publishes ambient settings/pool via a
// process-global `OnceLock`, so it runs at most ONCE per test binary — hence a
// single boot drives every state-dependent assertion (on distinct groups).
// The policy gate (test 5) needs no app and is its own `#[test]` below.
// =========================================================================

#[tokio::test]
async fn presence_gating_default_off_projection_dedup_anonymous() {
    boot(plugin()).await;
    let registry = Realtime::registry();

    // --- 1. Default OFF: `public:lobby` is not a `room:*` group → silent. ---
    {
        let mut sub = watch("public:lobby").await;
        let id = connect(&registry, Some(7), "public:lobby").await;
        disconnect(&registry, id).await;
        let events = drain(&mut sub);
        assert!(
            events.is_empty(),
            "a non-presence group emits no presence:* events; got {events:?}"
        );
    }

    // --- 2. Default projection is {id}-only (no name/email leak). ---
    {
        let mut sub = watch("room:42").await;
        // user 7's "row" has name+email, but the projection only sees the id.
        let _id = connect(&registry, Some(7), "room:42").await;
        let events = drain(&mut sub);
        let join = events
            .iter()
            .find(|(n, _)| n == "presence:join")
            .expect("a presence:join reached the subscriber");
        let obj = join.1.as_object().expect("member payload is an object");
        assert_eq!(obj.len(), 1, "default projection carries exactly one key");
        assert_eq!(obj.get("id").and_then(|v| v.as_i64()), Some(7));
        let raw = join.1.to_string();
        assert!(!raw.contains("name"), "no name field on the wire: {raw}");
        assert!(!raw.contains("email"), "no email field on the wire: {raw}");
    }

    // --- 3. Dedup by user: first-join / last-leave across two tabs. ---
    {
        let mut sub = watch("room:dedup").await;

        let c1 = connect(&registry, Some(9), "room:dedup").await;
        let after_first = drain(&mut sub);
        let joins = after_first.iter().filter(|(n, _)| n == "presence:join").count();
        assert_eq!(joins, 1, "first connection of a user fires exactly one join");

        // 2nd tab, SAME user → NO second join.
        let c2 = connect(&registry, Some(9), "room:dedup").await;
        let after_second = drain(&mut sub);
        assert!(
            !names(&after_second).contains(&"presence:join"),
            "a 2nd connection of an already-present user fires NO join; got {after_second:?}"
        );

        // Close one of two → NOT a leave (still has a live conn).
        disconnect(&registry, c1).await;
        let after_one_close = drain(&mut sub);
        assert!(
            !names(&after_one_close).contains(&"presence:leave"),
            "closing one of two connections fires NO leave; got {after_one_close:?}"
        );

        // Close the last → fully left → exactly one leave naming the user.
        disconnect(&registry, c2).await;
        let after_last_close = drain(&mut sub);
        let leaves = after_last_close.iter().filter(|(n, _)| n == "presence:leave").count();
        assert_eq!(leaves, 1, "closing the LAST connection fires exactly one leave");
        assert_eq!(
            after_last_close
                .iter()
                .find(|(n, _)| n == "presence:leave")
                .and_then(|(_, d)| d.get("id"))
                .and_then(|v| v.as_i64()),
            Some(9),
            "the leave names the user who left"
        );
    }

    // --- 4. Anonymous excluded: a None-user conn produces no presence. ---
    {
        let mut sub = watch("room:anon").await;
        let id = connect(&registry, None, "room:anon").await;
        let on_join = drain(&mut sub);
        disconnect(&registry, id).await;
        let on_leave = drain(&mut sub);
        assert!(
            on_join.is_empty() && on_leave.is_empty(),
            "an anonymous conn never appears in presence; join={on_join:?} leave={on_leave:?}"
        );
    }
}

// =========================================================================
// 5. Policy-gated — who may JOIN a presence group is governed by can_join, so
//    a client the default policy won't admit to a private group never sees its
//    presence. (Same gate the SSE/WS handshake applies before registering.)
// =========================================================================

#[test]
fn t5_presence_visibility_is_policy_gated() {
    use umbra_realtime::GroupPolicy;
    let policy = PublicGroupsOnly;
    // A private `room:*` group is presence-enabled by the spec, but the default
    // policy refuses the join — so the client never registers, never receives
    // its presence:* events. Enabling presence never widens who can subscribe.
    assert!(
        !policy.can_join(Some(1), "room:42"),
        "the default policy denies a non-public group, presence-enabled or not"
    );
    assert!(
        !policy.can_join(None, "room:42"),
        "an anonymous client is denied the private presence group too"
    );
    // Only a public: presence group is visible by default.
    assert!(
        policy.can_join(None, "public:lobby"),
        "a public: group is joinable, so its presence (if enabled) is visible"
    );
}
