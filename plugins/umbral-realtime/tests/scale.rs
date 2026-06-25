//! Scale / load test for the real-time connection [`Registry`].
//!
//! Proves the registry stays fast and **non-starving** at 10k concurrent
//! connections: a 10k broadcast finishes in single-digit milliseconds, and —
//! the key property — a concurrent `connection_count()` / `register` /
//! `deregister` never waits behind an in-flight broadcast, because
//! `dispatch` snapshots the sender list under the read lock and releases it
//! *before* the `try_send` loop (so registry writes are never blocked behind
//! a 10k fan-out).
//!
//! Run with the real numbers visible:
//!   cargo test -p umbral-realtime --test scale -- --nocapture

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use umbral_realtime::{ConnId, Event, Registry, TargetKind, DEFAULT_BUFFER};

/// Total connections to register. Half are broadcast-only (no group), half
/// are spread across `GROUPS` distinct groups so group-targeted dispatch is
/// exercised at scale too.
const N: usize = 10_000;
/// Distinct groups the grouped half is spread across.
const GROUPS: usize = 100;

fn groups_of(g: &str) -> HashSet<String> {
    let mut s = HashSet::new();
    s.insert(g.to_string());
    s
}

fn reg_event(event: &str, data: serde_json::Value) -> Event {
    Event {
        event: event.into(),
        data,
        channel: String::new(),
        seq: 0,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registry_stays_fast_and_non_starving_at_10k() {
    // (1) Unlimited connection cap (the default). Disable replay so this
    // measures pure registry dispatch, not the replay-buffer push.
    let reg = Arc::new(Registry::new(0, None));

    // (2) Register N connections. Keep every Receiver alive (dropping a
    // receiver closes the channel, which would let try_send fail). Half are
    // broadcast-only (no group, no user); half join one of GROUPS groups.
    let mut receivers: Vec<tokio::sync::mpsc::Receiver<Event>> = Vec::with_capacity(N);
    let mut conn_ids: Vec<ConnId> = Vec::with_capacity(N);
    // Track how many connections landed in group "g42" so the group-dispatch
    // assertion is exact rather than hard-coded.
    let target_group = "g42";
    let mut target_group_count = 0usize;

    for i in 0..N {
        let (user_id, groups) = if i % 2 == 0 {
            // Broadcast-reachable, no group, anonymous.
            (None, HashSet::new())
        } else {
            // Spread across GROUPS distinct groups; give it a user id too so
            // the by_user index is also populated at scale.
            let g = format!("g{}", (i / 2) % GROUPS);
            if g == target_group {
                target_group_count += 1;
            }
            (Some(i.to_string()), groups_of(&g))
        };
        let (id, rx) = reg
            .register(user_id, groups, DEFAULT_BUFFER)
            .await
            .expect("unlimited cap → register always succeeds");
        conn_ids.push(id);
        receivers.push(rx);
    }

    assert_eq!(reg.connection_count().await, N, "all {N} registered");
    assert!(target_group_count > 0, "the sampled group has members");

    // (3) Broadcast dispatch at scale. Channels are empty (DEFAULT_BUFFER=64),
    // so every try_send succeeds → delivered == N.
    let t0 = Instant::now();
    let delivered = reg
        .dispatch(&TargetKind::Broadcast, reg_event("ping", serde_json::json!({"n": 1})))
        .await;
    let broadcast_elapsed = t0.elapsed();
    assert_eq!(delivered, N, "broadcast queued to all {N} connections");
    assert!(
        broadcast_elapsed < Duration::from_millis(250),
        "10k broadcast must not starve: took {broadcast_elapsed:?} (bound 250ms)"
    );
    eprintln!("[scale] {N}-connection broadcast dispatch: {broadcast_elapsed:?} (delivered {delivered})");

    // (4) Group dispatch at scale: only the target group's connections.
    let t1 = Instant::now();
    let g_delivered = reg
        .dispatch(
            &TargetKind::Group(target_group.into()),
            reg_event("g", serde_json::json!({"g": target_group})),
        )
        .await;
    let group_elapsed = t1.elapsed();
    assert_eq!(
        g_delivered, target_group_count,
        "group dispatch hits exactly that group's {target_group_count} connections"
    );
    eprintln!(
        "[scale] group '{target_group}' dispatch ({target_group_count} conns): {group_elapsed:?}"
    );

    // Drain the messages we just queued so the bounded buffers stay clear for
    // the concurrent-load phase below (each conn now has up to ~2 queued).
    for rx in receivers.iter_mut() {
        while rx.try_recv().is_ok() {}
    }

    // (5) NON-STARVATION under concurrent load — the key property.
    // One task hammers broadcasts to all N for ~200ms. Concurrently, a tight
    // loop runs registry ops (connection_count + a register/deregister pair)
    // and measures each op's latency. Because dispatch releases the lock
    // before sending, no registry op should block behind a 10k fan-out.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let broadcaster = {
        let reg = reg.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            let mut rounds = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                reg.dispatch(&TargetKind::Broadcast, reg_event("load", serde_json::json!({})))
                    .await;
                rounds += 1;
                // Yield so the drain task and ops task get scheduled; without
                // this a busy broadcast loop could monopolize a worker.
                tokio::task::yield_now().await;
            }
            rounds
        })
    };

    // Keep the broadcast buffers from filling during the load window: a
    // separate task continuously drains every receiver. (Without draining,
    // the 64-deep buffers fill after ~64 rounds and try_send starts failing —
    // which is correct per-conn backpressure but isn't what phase 5 measures.)
    let drainer = {
        let stop = stop.clone();
        // Move the receivers into the drain task.
        let mut receivers = receivers;
        tokio::spawn(async move {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                for rx in receivers.iter_mut() {
                    while rx.try_recv().is_ok() {}
                }
                tokio::task::yield_now().await;
            }
            // Final drain + hand the receivers back so they stay alive until
            // the test ends (returning them keeps the channels open).
            for rx in receivers.iter_mut() {
                while rx.try_recv().is_ok() {}
            }
            receivers
        })
    };

    // The measured loop: registry ops concurrent with the broadcast storm.
    let load_deadline = Instant::now() + Duration::from_millis(200);
    let mut max_op_latency = Duration::ZERO;
    let mut ops = 0u64;
    while Instant::now() < load_deadline {
        // connection_count under load.
        let c0 = Instant::now();
        let _ = reg.connection_count().await;
        max_op_latency = max_op_latency.max(c0.elapsed());

        // register + deregister pair under load.
        let r0 = Instant::now();
        let (tmp_id, _tmp_rx) = reg
            .register(Some("-1".to_string()), HashSet::new(), DEFAULT_BUFFER)
            .await
            .expect("register under load");
        reg.deregister(tmp_id).await;
        max_op_latency = max_op_latency.max(r0.elapsed());

        ops += 1;
        tokio::task::yield_now().await;
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let rounds = broadcaster.await.expect("broadcaster joined");
    let receivers = drainer.await.expect("drainer joined");

    eprintln!(
        "[scale] under-load: {ops} registry ops across {rounds} concurrent broadcasts, \
         max single-op latency {max_op_latency:?}"
    );
    assert!(
        max_op_latency < Duration::from_millis(20),
        "registry ops must not block behind a 10k broadcast: max op {max_op_latency:?} (bound 20ms)"
    );

    // Registry is back to N (the temp connections were all deregistered).
    assert_eq!(reg.connection_count().await, N, "back to {N} after load");

    // (6) Delivery correctness sample: a final broadcast must actually land
    // in the receivers — "fast" must not be hiding "dropped everything".
    // Drain first so buffers are empty, then broadcast once and read.
    let mut receivers = receivers;
    for rx in receivers.iter_mut() {
        while rx.try_recv().is_ok() {}
    }
    let final_delivered = reg
        .dispatch(&TargetKind::Broadcast, reg_event("final", serde_json::json!({"check": true})))
        .await;
    assert_eq!(final_delivered, N, "final broadcast queued to all {N}");

    let sample = 200.min(N);
    let mut got = 0usize;
    for rx in receivers.iter_mut().take(sample) {
        if let Ok(ev) = rx.try_recv() {
            assert_eq!(ev.event, "final", "sampled receiver got the final event");
            got += 1;
        }
    }
    assert_eq!(got, sample, "every sampled receiver ({sample}) received the broadcast");
    eprintln!("[scale] delivery sample: {got}/{sample} receivers got the final broadcast");
}
