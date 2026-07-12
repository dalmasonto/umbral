//! features #73 — a materialized view on SQLite must fail the boot.
//!
//! Its own test binary, because the assertion IS that `App::build()` returns Err —
//! and an App is a process-global that can only be built once.
//!
//! The tempting alternative is to render a materialized view as a plain view on
//! SQLite so "it works everywhere". That would be the worst possible outcome. A plain
//! view recomputes its SELECT on every read; a materialized one serves stored rows.
//! Swapping them silently gives you a dev and test backend whose results are always
//! right and whose *performance contract is inverted* — the expensive query you
//! reached for a materialized view to avoid is now running on every request, and you
//! will not find out until production is under load. Correct answers, wrong shape,
//! no error: exactly the class of bug the design principles ban the SQLite fallback
//! branch to prevent.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(
    table = "mv_standings",
    materialized_view = "SELECT 1 AS id, 0 AS points"
)]
pub struct MvStandings {
    pub id: i64,
    pub points: i64,
}

#[tokio::test]
async fn materialized_view_on_sqlite_fails_the_boot_with_a_clear_message() {
    let settings = umbral::Settings::from_env().expect("figment defaults");
    let tmp = tempfile::tempdir().expect("tempdir");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(tmp.path().join("mv.sqlite"))
                .create_if_missing(true),
        )
        .await
        .expect("pool");

    // `App` is not Debug, so unwrap the Result by hand rather than `expect_err`.
    let msg = match umbral::App::builder()
        .settings(settings)
        .database("default", pool)
        .model::<MvStandings>()
        .build()
    {
        Ok(_) => panic!("a materialized view on SQLite must not boot"),
        Err(e) => format!("{e}"),
    };
    assert!(
        msg.contains("MvStandings") && msg.contains("materialized"),
        "the failure must name the model and the reason, got: {msg}"
    );
    assert!(
        msg.contains("postgres"),
        "the failure must say where a materialized view DOES work, got: {msg}"
    );
}
