//! Security proof (test 5): the action filter is respected. With
//! `actions(&[Created])`, an update or delete must NOT dispatch.

#![allow(dead_code)]

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use umbra_realtime::{DEFAULT_BUFFER, Expose, ModelAction, Realtime, RealtimePlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "act_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
}

#[tokio::test]
async fn action_filter_only_dispatches_listed_actions() {
    umbra::signals::clear_for_tests();
    let pool = umbra::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();
    umbra::App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(RealtimePlugin::new().expose::<Post>(
            Expose::to_group("public:act")
                .fields(&["id", "title"])
                .actions(&[ModelAction::Created]),
        ))
        .build()
        .expect("App::build");

    let mut groups = HashSet::new();
    groups.insert("public:act".to_string());
    let (_id, mut rx) = Realtime::registry()
        .register(None, groups, DEFAULT_BUFFER)
        .await
        .expect("registration admitted");

    // An update (created: false) — NOT in the filter, must be silent.
    umbra::signals::emit(
        "post_save:act_post",
        serde_json::json!({ "instance": { "id": 1, "title": "u" }, "created": false }),
    )
    .await;
    assert!(rx.try_recv().is_err(), "update is filtered out");

    // A delete — NOT in the filter, must be silent.
    umbra::signals::emit(
        "post_delete:act_post",
        serde_json::json!({ "instance": { "id": 1, "title": "u" } }),
    )
    .await;
    assert!(rx.try_recv().is_err(), "delete is filtered out");

    // A create — IS in the filter, must dispatch.
    umbra::signals::emit(
        "post_save:act_post",
        serde_json::json!({ "instance": { "id": 2, "title": "c" }, "created": true }),
    )
    .await;
    let ev = rx.try_recv().expect("create is dispatched");
    assert_eq!(ev.event, "created");
}
