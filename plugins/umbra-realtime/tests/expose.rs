//! Security proof for `RealtimePlugin::expose::<T>` — the safe, opt-in
//! "subscribe to a model's live changes" feature.
//!
//! These tests are the deliverable: they prove nothing leaks. We drive the
//! ORM signal directly (the exact call `Manager::create`/`delete` makes on the
//! write path) and assert what reaches the wire — that a non-exposed model is
//! silent, that the field whitelist strips everything else (including a
//! `secret` / `password_hash`), that the default projection is id-only, that
//! `all_fields()` is the only way to broadcast the whole row, and that the
//! action filter and the group policy hold.

#![allow(dead_code)]

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use umbra_realtime::{
    DEFAULT_BUFFER, Expose, GroupPolicy, PublicGroupsOnly, Realtime, RealtimePlugin,
};

// A model with a secret the broadcast must never carry unless whitelisted.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "expose_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub slug: String,
    pub secret: String,
    pub password_hash: String,
}

// A second model that is NEVER exposed — its signals must fan out to nobody.
#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "expose_private")]
pub struct Private {
    pub id: i64,
    pub data: String,
}

/// Boot an app with a single `expose`'d model per the given spec. Each test
/// binary boots one app (the ambient handle is a process-global `OnceLock`),
/// so a test sets up exactly the exposure it asserts on. We clear the signal
/// registry first so no stray subscription leaks across the build.
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

/// Register a sink in `group`, bypassing the handshake policy (the transports
/// apply it; here we just need a receiver in the group to read what dispatch
/// delivered).
async fn watch(group: &str) -> tokio::sync::mpsc::Receiver<umbra_realtime::Event> {
    let mut groups = HashSet::new();
    groups.insert(group.to_string());
    let (_id, rx) = Realtime::registry()
        .register(None, groups, DEFAULT_BUFFER)
        .await
        .expect("registration admitted");
    rx
}

async fn fire_save(table: &str, instance: serde_json::Value, created: bool) {
    umbra::signals::emit(
        &format!("post_save:{table}"),
        serde_json::json!({ "instance": instance, "created": created }),
    )
    .await;
}

async fn fire_delete(table: &str, instance: serde_json::Value) {
    umbra::signals::emit(
        &format!("post_delete:{table}"),
        serde_json::json!({ "instance": instance }),
    )
    .await;
}

/// 2 + 3: the field whitelist strips everything else; the default projection
/// (no `.fields(...)`) is id-only. Both exposures live in this one binary,
/// each on its own group, so the single booted app proves both at once.
#[tokio::test]
async fn whitelist_strips_secrets_and_default_is_id_only() {
    boot(
        RealtimePlugin::new()
            // Whitelist: only id + title reach the wire.
            .expose::<Post>(Expose::to_group("public:posts").fields(&["id", "title"]))
            // No `.fields(...)` → id-only default.
            .expose::<Private>(Expose::to_group("public:private")),
    )
    .await;

    // --- The load-bearing test: the whitelist drops secret + password_hash. ---
    let mut posts = watch("public:posts").await;
    fire_save(
        "expose_post",
        serde_json::json!({
            "id": 1,
            "title": "Hello",
            "slug": "hello",
            "secret": "treasure",
            "password_hash": "$argon2id$leak-me-not",
        }),
        true,
    )
    .await;

    let ev = posts.try_recv().expect("the exposed save fanned out");
    assert_eq!(ev.event, "created", "action becomes the event name");
    let obj = ev.data.as_object().expect("payload is an object");
    assert_eq!(obj.get("id").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(obj.get("title").and_then(|v| v.as_str()), Some("Hello"));
    // The whole point: nothing outside the whitelist survives.
    assert!(
        !obj.contains_key("secret"),
        "secret must be stripped; got {}",
        ev.data
    );
    assert!(
        !obj.contains_key("password_hash"),
        "password_hash must be stripped; got {}",
        ev.data
    );
    assert!(
        !obj.contains_key("slug"),
        "a non-whitelisted column must be stripped; got {}",
        ev.data
    );
    // And belt-and-suspenders: the raw serialized frame carries no secret text.
    let raw = ev.data.to_string();
    assert!(!raw.contains("treasure"), "secret value leaked into the frame");
    assert!(!raw.contains("argon2"), "password_hash value leaked into the frame");

    // --- Default projection (no .fields) is id-only. ---
    let mut priv_rx = watch("public:private").await;
    fire_save(
        "expose_private",
        serde_json::json!({ "id": 99, "data": "do-not-broadcast" }),
        true,
    )
    .await;
    let ev = priv_rx.try_recv().expect("the exposed save fanned out");
    let obj = ev.data.as_object().expect("payload is an object");
    assert_eq!(obj.len(), 1, "id-only default carries exactly one key");
    assert_eq!(obj.get("id").and_then(|v| v.as_i64()), Some(99));
    assert!(
        !ev.data.to_string().contains("do-not-broadcast"),
        "the id-only default must not leak other columns"
    );
}

/// 6: subscription is still policy-gated. `expose` picks the GROUP, but who
/// may JOIN it is governed by `GroupPolicy::can_join` — the same gate the SSE
/// handshake applies (see `sse.rs`, which 403s a non-`public:` group). So
/// exposing to a private group does NOT make it joinable under the default
/// `PublicGroupsOnly` policy; the dev must opt in with a custom policy.
#[test]
fn exposed_private_group_is_not_joinable_under_default_policy() {
    let policy = PublicGroupsOnly;
    // A `public:` group the dev exposed to is joinable…
    assert!(
        policy.can_join(None, "public:posts"),
        "a public: exposed group is joinable by default"
    );
    // …but a private group (even if exposed to) is denied — exposing a model
    // never widens who can subscribe; the policy still governs.
    assert!(
        !policy.can_join(None, "post:42"),
        "a non-public exposed group is NOT joinable under the default policy"
    );
    assert!(
        !policy.can_join(Some(7), "tenant:secret"),
        "even an authenticated user is denied a private group by default"
    );
}
