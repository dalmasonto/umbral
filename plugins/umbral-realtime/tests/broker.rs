//! Broker tests (P6 phase 5).
//!
//! The `Envelope` wire-format round-trip runs always. The multi-instance
//! `RedisBroker` relay is gated on BOTH the `redis` cargo feature and a
//! live `REDIS_URL` (skipped at runtime when absent), mirroring
//! umbral-cache's redis integration tests:
//!
//!   REDIS_URL=redis://localhost:6379/15 cargo test --features redis -p umbral-realtime --test broker

use umbral_realtime::{Envelope, TargetKind};

#[test]
fn envelope_round_trips_through_json() {
    // The exact shape the RedisBroker ships over pub/sub. Every TargetKind
    // variant must survive the trip so `to_user` / `to_group` / `broadcast`
    // all relay across instances.
    for target in [
        TargetKind::User("42".to_string()),
        TargetKind::Group("public:plugin-7".to_string()),
        TargetKind::Broadcast,
    ] {
        let env = Envelope {
            target: target.clone(),
            event: "note".to_string(),
            data: serde_json::json!({ "author": "ada", "pending": true }),
        };
        let json = serde_json::to_string(&env).expect("serialize");
        let back: Envelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.target, target);
        assert_eq!(back.event, "note");
        assert_eq!(
            back.data,
            serde_json::json!({ "author": "ada", "pending": true })
        );
    }
}

#[cfg(feature = "redis")]
mod redis_relay {
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;

    use umbral_realtime::{Broker, Envelope, RedisBroker, Registry, TargetKind};

    fn redis_url() -> Option<String> {
        std::env::var("REDIS_URL").ok()
    }

    /// Two "instances" (separate registries + broker pumps) share one Redis.
    /// A connection lives on instance B; a publish on instance A must reach
    /// it through the Redis relay — the whole point of the multi-instance
    /// broker.
    #[tokio::test]
    async fn publish_on_one_instance_reaches_a_socket_on_another() {
        let Some(url) = redis_url() else {
            eprintln!("REDIS_URL not set — skipping the live Redis relay test");
            return;
        };

        let reg_a = Arc::new(Registry::default());
        let reg_b = Arc::new(Registry::default());
        let broker_a = RedisBroker::start(url.clone(), reg_a.clone());
        let _broker_b = RedisBroker::start(url.clone(), reg_b.clone());

        // Process-unique user id so concurrent test binaries against the same
        // Redis don't cross-deliver (the channel is shared globally).
        let user = std::process::id().to_string();

        // A live connection for `user`, on instance B only.
        let (_id, mut rx) = reg_b.register(Some(user.clone()), HashSet::new(), 8).await.expect("test registration should not be refused (no cap here)");

        // Let both pumps finish SUBSCRIBE before the publish.
        tokio::time::sleep(Duration::from_millis(400)).await;

        broker_a
            .publish(Envelope {
                target: TargetKind::User(user.clone()),
                event: "ping".to_string(),
                data: serde_json::json!({ "hello": "world" }),
            })
            .await;

        // The event crosses A → Redis → B and lands on B's socket.
        let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("event delivered within 3s")
            .expect("connection channel still open");
        assert_eq!(event.event, "ping");
        assert_eq!(event.data, serde_json::json!({ "hello": "world" }));
    }
}
