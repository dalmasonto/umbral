//! `security.object_scope` must also flag AUTO-EXPOSED writable models —
//! models registered via `.model::<T>()` with no explicit `ResourceConfig`
//! at all (gaps6 #2). Auto-exposed models are the largest slice of the IDOR
//! write surface and exactly the "forgot to scope it" case the check exists
//! to catch; before this change the check only ever walked
//! `configured_resources`, so a model nobody called `.resource(...)` on was
//! invisible to it.
//!
//! One real `App::build()` (OnceLocks: settings / db / backend), same
//! constraint as `object_scope_strict_build.rs`. `strict_object_scope` is
//! used to turn findings into a `BuildError` we can assert on directly,
//! covering the warn case plus every clearing mechanism in one build.

#![allow(dead_code, private_interfaces)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use umbral::BuildError;
use umbral_rest::{Action, ResourceConfig, RestPlugin};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Widget {
    id: i64,
    name: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Post {
    id: i64,
    owner_id: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Invoice {
    id: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Changelog {
    id: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct Country {
    id: i64,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
struct SecretDoc {
    id: i64,
}

#[tokio::test]
async fn auto_exposed_writable_model_is_flagged_and_clearing_mechanisms_still_work() {
    let mut settings = umbral::Settings::from_env().expect("figment defaults");
    settings.strict_object_scope = true;

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("rest_auto_exposed_object_scope.sqlite");
    std::mem::forget(tmp);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(
            SqliteConnectOptions::new()
                .busy_timeout(std::time::Duration::from_secs(5))
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    let result = umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        // `widget` gets NO `.resource(...)` call at all — the auto-exposed,
        // unscoped case this test exists to prove is now caught.
        .model::<Widget>()
        .model::<Post>()
        .model::<Invoice>()
        .model::<Changelog>()
        .model::<Country>()
        .model::<SecretDoc>()
        .plugin(
            RestPlugin::default()
                .resource(ResourceConfig::new("post").owned_by("owner_id"))
                .resource(ResourceConfig::new("invoice").rls_backed())
                .resource(ResourceConfig::new("changelog").unscoped_ok("public append-only feed"))
                .resource(ResourceConfig::new("country").views([Action::List, Action::Retrieve]))
                .exclude(["secret_doc"]),
        )
        .build();

    let findings = match result {
        Err(BuildError::SystemCheckFailed { findings }) => findings,
        Err(other) => panic!("expected SystemCheckFailed, got a different BuildError: {other:?}"),
        Ok(_) => panic!("an auto-exposed unscoped writable model must block strict boot"),
    };
    let flagged: Vec<&str> = findings
        .iter()
        .filter(|f| f.check_id == "security.object_scope")
        .map(|f| f.message.as_str())
        .collect();

    assert_eq!(
        flagged.len(),
        1,
        "only the unscoped auto-exposed `widget` model should be flagged; got {flagged:#?}"
    );
    assert!(
        flagged[0].contains("widget"),
        "expected the finding to name `widget`; got {:?}",
        flagged[0]
    );
    for clear in ["post", "invoice", "changelog", "country", "secret_doc"] {
        assert!(
            !flagged[0].contains(clear),
            "`{clear}` must not be flagged ({} clears it)",
            match clear {
                "post" => "owned_by",
                "invoice" => "rls_backed",
                "changelog" => "unscoped_ok",
                "country" => "read-only views (writes hidden)",
                "secret_doc" => "exclude",
                _ => unreachable!(),
            }
        );
    }
}
