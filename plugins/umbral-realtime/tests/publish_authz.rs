//! audit_2 realtime #2 — `MessageContext::publish` authorizes the sender
//! against the installed `GroupPolicy` BEFORE broadcasting, so a
//! client-supplied room can't be used to inject frames into a group the
//! sender may not post to (message spoofing / IDOR).
//!
//! Its own test binary because the ambient `Realtime` handle is a one-shot
//! `OnceLock` — installing a plugin here mustn't race the `ws.rs` install.

use umbral_realtime::{MessageContext, RealtimePlugin};

/// Build a minimal app so the ambient `Realtime` is installed with the
/// default `PublicGroupsOnly` policy (allows `public:*`, denies the rest).
async fn install_default_policy() {
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();
    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(RealtimePlugin::default())
        .build()
        .expect("App::build with RealtimePlugin");
}

#[tokio::test]
async fn ctx_publish_authorizes_before_broadcast() {
    install_default_policy().await;

    let ctx = MessageContext {
        conn_id: 1,
        user_id: Some("42".to_string()),
    };

    // can_send mirrors the installed policy.
    assert!(ctx.can_send("public:lobby"), "public rooms are sendable");
    assert!(
        !ctx.can_send("chat:secret"),
        "a non-public room is denied by the default policy"
    );

    // publish drops (and returns false for) an unauthorized room — the IDOR
    // guard: a joined client can't inject into a group it may not post to.
    let denied = ctx
        .publish(
            "chat:secret",
            "message",
            &serde_json::json!({"body": "leak"}),
        )
        .await;
    assert!(
        !denied,
        "publish to an unauthorized room must be dropped (realtime #2)"
    );

    // publish succeeds (returns true) for an authorized room.
    let allowed = ctx
        .publish(
            "public:lobby",
            "message",
            &serde_json::json!({"body": "hi"}),
        )
        .await;
    assert!(allowed, "publish to an authorized room succeeds");
}
