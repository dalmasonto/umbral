//! Reconnect-resume + connection-cap tests (the two #82 realtime gaps).
//!
//! These drive the `Registry` directly — the same path the SSE/WS handlers
//! use — rather than full HTTP, mirroring the in-crate registry tests.
//! Covered:
//!   * monotonic event ids on every dispatch,
//!   * `replay_since` replays exactly the events after `Last-Event-ID`,
//!   * replay is target-filtered (a User(A) reconnect doesn't see User(B)),
//!   * a tiny replay buffer retains only the most recent N,
//!   * `max_connections` refuses registration at the cap (the same `None`
//!     the handlers turn into a 503), and a freed slot re-admits.

use std::collections::HashSet;

use umbra_realtime::{DEFAULT_BUFFER, Event, Registry, TargetKind};

fn groups(gs: &[&str]) -> HashSet<String> {
    gs.iter().map(|s| s.to_string()).collect()
}

fn evt(event: &str) -> Event {
    Event {
        event: event.into(),
        data: serde_json::Value::Null,
        channel: String::new(),
        seq: 0,
    }
}

/// Every delivered event carries a strictly-increasing `seq`, regardless of
/// target — the id a browser echoes back as `Last-Event-ID`.
#[tokio::test]
async fn delivered_events_carry_increasing_seq() {
    let reg = Registry::default();
    let (_id, mut rx) = reg
        .register(None, groups(&[]), DEFAULT_BUFFER)
        .await
        .unwrap();

    for _ in 0..3 {
        reg.dispatch(&TargetKind::Broadcast, evt("e")).await;
    }

    let a = rx.try_recv().unwrap();
    let b = rx.try_recv().unwrap();
    let c = rx.try_recv().unwrap();
    assert!(a.seq >= 1, "seq starts at 1, got {}", a.seq);
    assert!(b.seq > a.seq && c.seq > b.seq, "seq is monotonic");
    assert_eq!(b.seq, a.seq + 1);
    assert_eq!(c.seq, b.seq + 1);
}

/// Deliver events 1..=5 into the buffer, then a reconnect with
/// `Last-Event-ID = <seq of event 2>` replays exactly events 3,4,5 (by
/// seq), in order, before any live event. An absent id → no replay.
#[tokio::test]
async fn replay_resumes_after_last_event_id() {
    let reg = Registry::default();
    // A broadcast subscriber, so every event matches its target.
    let (_id, _rx) = reg
        .register(None, groups(&[]), DEFAULT_BUFFER)
        .await
        .unwrap();

    let mut seqs = Vec::new();
    for i in 1..=5 {
        reg.dispatch(&TargetKind::Broadcast, evt(&format!("e{i}")))
            .await;
        // seq is assigned during dispatch; recover it from the buffer via a
        // fresh full replay (since=0 returns all five).
        seqs.push(i);
    }
    // Recover the actual seq of "event 2" by replaying everything.
    let all = reg.replay_since(0, None, &groups(&[]));
    assert_eq!(all.len(), 5, "all five buffered");
    let seq_of_2 = all[1].seq;

    let resumed = reg.replay_since(seq_of_2, None, &groups(&[]));
    let names: Vec<&str> = resumed.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(names, ["e3", "e4", "e5"], "replays only what's after id 2");
    // Replayed frames keep their ORIGINAL seq so the SSE stream re-stamps id.
    assert!(resumed[0].seq > seq_of_2);
    assert_eq!(resumed[0].seq, all[2].seq);

    // Absent header → no replay (live-only).
    let none = reg.replay_since(all.last().unwrap().seq, None, &groups(&[]));
    assert!(none.is_empty(), "nothing after the newest event");
}

/// A `User(A)` reconnect only replays events its target would have received:
/// it gets `User(A)` + `Broadcast` + its joined groups, never `User(B)`-only.
#[tokio::test]
async fn replay_is_target_filtered() {
    let reg = Registry::default();

    reg.dispatch(&TargetKind::User(1), evt("for-a")).await; // A only
    reg.dispatch(&TargetKind::User(2), evt("for-b")).await; // B only
    reg.dispatch(&TargetKind::Broadcast, evt("for-all")).await; // both
    reg.dispatch(&TargetKind::Group("room:1".into()), evt("for-room"))
        .await; // group members

    // User A, member of room:1, reconnecting from the start.
    let a = reg.replay_since(0, Some(1), &groups(&["room:1"]));
    let a_names: Vec<&str> = a.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(a_names, ["for-a", "for-all", "for-room"]);
    assert!(
        !a_names.contains(&"for-b"),
        "A must not see B-only events on replay"
    );

    // User B, no groups: only its user event + the broadcast.
    let b = reg.replay_since(0, Some(2), &groups(&[]));
    let b_names: Vec<&str> = b.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(b_names, ["for-b", "for-all"]);
}

/// A tiny replay buffer keeps only the most recent N events; older ones are
/// evicted (the bounded-buffer caveat — anything dropped is unrecoverable).
#[tokio::test]
async fn bounded_buffer_retains_only_the_newest() {
    let reg = Registry::new(2, None); // cap 2

    for i in 1..=5 {
        reg.dispatch(&TargetKind::Broadcast, evt(&format!("e{i}")))
            .await;
    }

    let retained = reg.replay_since(0, None, &groups(&[]));
    let names: Vec<&str> = retained.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(names, ["e4", "e5"], "only the most recent 2 survive");
}

/// `max_connections(1)`: the 2nd registration is refused (`None` — the same
/// path the handlers turn into a 503), and a freed slot after `deregister`
/// re-admits a new connection.
#[tokio::test]
async fn connection_cap_refuses_then_readmits() {
    let reg = Registry::new(0, Some(1)); // replay off, cap 1

    let first = reg.register(None, groups(&[]), DEFAULT_BUFFER).await;
    let (id1, _rx1) = first.expect("first connection admitted");

    let second = reg.register(None, groups(&[]), DEFAULT_BUFFER).await;
    assert!(second.is_none(), "2nd refused at the cap → handler sends 503");
    assert_eq!(reg.connection_count().await, 1);

    // Free the slot; the next registration succeeds.
    reg.deregister(id1).await;
    assert_eq!(reg.connection_count().await, 0);
    let third = reg.register(None, groups(&[]), DEFAULT_BUFFER).await;
    assert!(third.is_some(), "a freed slot re-admits a new connection");
}
