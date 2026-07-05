//! audit_2 H17 — `settings.databases` entries are actually opened at boot.
//!
//! Before the fix, `[databases] reports = "..."` was deserialized and documented
//! but never turned into a pool: a model routed to `reports` tripped
//! `PluginDatabaseAlias` at build (or panicked `no database registered` at query
//! time). Now `App::build()` opens each declared alias as a LAZY pool, so the
//! model resolves and the config does what the docs say. Drives it end to end:
//! build succeeds, the alias is registered, and a real round-trip lands in the
//! reports DB.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tokio::sync::OnceCell;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "report_metric", database = "reports")]
pub struct ReportMetric {
    pub id: i64,
    #[umbral(string)]
    pub name: String,
}

static BOOT: OnceCell<()> = OnceCell::const_new();

async fn boot() {
    BOOT.get_or_init(|| async {
        let dir = std::env::temp_dir();
        let default_path = dir.join(format!("umbral_h17_default_{}.db", std::process::id()));
        let reports_path = dir.join(format!("umbral_h17_reports_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&default_path);
        let _ = std::fs::remove_file(&reports_path);

        let default_pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(
                SqliteConnectOptions::new()
                    .busy_timeout(std::time::Duration::from_secs(5))
                    .filename(&default_path)
                    .create_if_missing(true),
            )
            .await
            .expect("default pool");

        let mut settings = umbral::Settings::from_env().expect("settings");
        // The `reports` alias is provided ONLY through settings.databases —
        // there is NO `.database("reports", ...)` call. The framework must open
        // it lazily so the routed model resolves.
        settings.databases.insert(
            "reports".to_string(),
            format!("sqlite://{}?mode=rwc", reports_path.display()),
        );

        umbral::App::builder()
            .settings(settings)
            .database("default", default_pool)
            .model::<ReportMetric>()
            .build()
            .expect("App::build must open the settings.databases `reports` pool (H17)");

        // Migrate — the CreateTable for `report_metric` must land in the reports DB.
        let mig = tempfile::tempdir().expect("mig dir");
        let mig_path = mig.path().to_path_buf();
        std::mem::forget(mig);
        umbral::migrate::make_in(&mig_path)
            .await
            .expect("makemigrations");
        umbral::migrate::run_in(&mig_path).await.expect("migrate");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_databases_alias_is_registered() {
    boot().await;
    assert!(
        umbral::db::registered_aliases()
            .iter()
            .any(|a| a == "reports"),
        "the settings.databases `reports` alias must be a registered pool"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn routed_model_round_trips_through_the_lazy_pool() {
    boot().await;
    // A create + read-back on a model routed to `reports` exercises the lazily
    // opened pool (it connects on this first use).
    let created = ReportMetric::objects()
        .create(ReportMetric {
            id: 0,
            name: "signups".to_string(),
        })
        .await
        .expect("create on the reports-routed model");
    let fetched = ReportMetric::objects()
        .filter(umbral::orm::Predicate::<ReportMetric>::col_eq(
            "id", created.id,
        ))
        .first()
        .await
        .expect("query")
        .expect("row exists in the reports DB");
    assert_eq!(fetched.name, "signups");
}
