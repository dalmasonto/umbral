//! Security proof (test 4): `all_fields()` is the explicit, conspicuous opt-in
//! to broadcast the WHOLE row — proving the difference from the safe default.
//! With `all_fields()` the other columns DO appear; without it (the default)
//! they don't (covered by `expose.rs`).

#![allow(dead_code)]

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use umbral_realtime::{DEFAULT_BUFFER, Expose, Realtime, RealtimePlugin};

#[derive(Debug, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "allf_post")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub extra: String,
}

#[tokio::test]
async fn all_fields_opts_into_the_full_row() {
    umbral::signals::clear_for_tests();
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = "sqlite::memory:".to_string();
    umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        // Explicit, conspicuous: the dev knowingly broadcasts the whole row.
        .plugin(
            RealtimePlugin::new()
                .expose::<Post>(Expose::to_group("public:all").all_fields()),
        )
        .build()
        .expect("App::build");

    let mut groups = HashSet::new();
    groups.insert("public:all".to_string());
    let (_id, mut rx) = Realtime::registry()
        .register(None, groups, DEFAULT_BUFFER)
        .await
        .expect("registration admitted");

    umbral::signals::emit(
        "post_save:allf_post",
        serde_json::json!({
            "instance": { "id": 5, "title": "T", "extra": "everything" },
            "created": true,
        }),
    )
    .await;

    let ev = rx.try_recv().expect("the exposed save fanned out");
    let obj = ev.data.as_object().expect("payload is an object");
    // all_fields → every column present, unlike the id-only default.
    assert_eq!(obj.get("id").and_then(|v| v.as_i64()), Some(5));
    assert_eq!(obj.get("title").and_then(|v| v.as_str()), Some("T"));
    assert_eq!(
        obj.get("extra").and_then(|v| v.as_str()),
        Some("everything"),
        "all_fields() includes the otherwise-hidden columns"
    );
}
