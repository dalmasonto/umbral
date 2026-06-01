//! Wires the `BlogPlugin` module into a runnable app.
//!
//! The whole point of this example is to show that registering a plugin
//! is a single `.plugin(...)` line — the plugin contributes the model,
//! the routes, the REST customisation (via the resource), the `@action`
//! endpoints, and the seed-on-boot hook. Notice what's NOT here:
//!
//! - No `.model::<Post>()` — the plugin's `Plugin::models()` hook auto-
//!   registers it.
//! - No `RestPlugin::default().hide(...)/.transform(...)/.computed(...)`
//!   chain — the resource bundles all of that.
//! - No seeding code in `main.rs` — the plugin's `on_ready()` does it
//!   the first time the binary runs against an empty database.

mod blog;

use umbra::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let settings = Settings::from_env()?;

    // Override `sqlite::memory:` with a file-backed URL so the schema
    // and seed survive across `cargo run` invocations.
    let database_url = if settings.database_url == "sqlite::memory:" {
        "sqlite://blog-plugin.db?mode=rwc".to_string()
    } else {
        settings.database_url.clone()
    };
    let pool = umbra::db::connect(&database_url).await?;

    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        // One line. Model + routes + on_ready all flow in from here.
        .plugin(blog::BlogPlugin)
        // REST plugin picks up the blog plugin's customisation —
        // hide / transform / computed / @action publish / @action recent.
        .plugin(
            umbra_rest::RestPlugin::default()
                .resource(blog::rest_resource()),
        )
        .build()?;

    // Auto-migrate on startup. Demo-only — production apps run
    // `cargo run -- makemigrations` and `cargo run -- migrate` as
    // separate, audited steps.
    auto_migrate().await?;
    // Seed the table the plugin owns. The plugin exposes `seed()`
    // because `on_ready` runs before migrate has applied schema
    // (the schema is a separate phase).
    blog::seed().await?;

    let addr = "127.0.0.1:3002".parse::<std::net::SocketAddr>()?;
    println!("blog example listening on http://{addr}");
    println!("  GET  /blog");
    println!("  GET  /blog/{{id}}");
    println!("  GET  /api/post/                       (list)");
    println!("  GET  /api/post/recent/?limit=N        (collection @action)");
    println!("  POST /api/post/{{id}}/publish/         (detail @action)");

    app.serve(addr).await?;
    Ok(())
}

async fn auto_migrate() -> Result<(), Box<dyn std::error::Error>> {
    match umbra::migrate::make().await {
        Ok(paths) => {
            for path in paths {
                eprintln!("auto-migrate: wrote {}", path.display());
            }
        }
        Err(e) => {
            eprintln!("auto-migrate: makemigrations skipped ({e})");
        }
    }
    umbra::migrate::run().await.map_err(|e: umbra::migrate::MigrateError| {
        format!("migrate run: {e}")
    })?;
    Ok(())
}
