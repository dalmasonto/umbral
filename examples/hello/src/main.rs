//! The smallest umbra app that actually does something.
//!
//! Everything below is reachable through `umbra::prelude::*` or the
//! `umbra::db` / `umbra::web` facades. Nothing in this file touches
//! `umbra_core` or `umbra_macros` directly. If a future change to
//! the facade breaks this file, the facade has regressed, not the
//! example.

use umbra::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Settings: defaults → umbra.toml → UMBRA_*-prefixed env vars.
    // With no toml and no env in scope this resolves to the dev defaults
    // baked into `Settings`, which is exactly what the example wants.
    let settings = Settings::from_env()?;

    // Open the default pool up front so a bad DATABASE_URL fails before
    // we bind a port. `App::build()` would otherwise auto-connect.
    let pool = umbra::db::connect(&settings.database_url).await?;

    let router = Router::new()
        .route("/", get(root))
        .route("/settings", get(settings_view));

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .router(router)
        .build()?;

    app.serve("127.0.0.1:3000".parse::<std::net::SocketAddr>()?)
        .await?;

    Ok(())
}

async fn root() -> &'static str {
    "hello from umbra-hello"
}

/// Tiny JSON-shaped view over the loaded settings.
///
/// We deliberately do not include `secret_key`. The body is hand-formatted
/// JSON returned as a string so the example stays serde-free. Swapping in
/// `JsonResponse` once the facade ships a re-export of `serde_json::Value`
/// (or accepts a borrowed `&str` payload) is a one-line change.
async fn settings_view() -> String {
    let s = umbra::Settings::from_env()
        .expect("settings already loaded once; second load should not fail");

    let env_label = match s.environment {
        Environment::Dev => "Dev",
        Environment::Test => "Test",
        Environment::Prod => "Prod",
    };

    format!(
        "{{\"database_url\":\"{}\",\"environment\":\"{}\"}}",
        s.database_url, env_label,
    )
}
