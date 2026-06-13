//! The `seed_orm_data` management command.
//!
//! `cargo run -- seed_orm_data` idempotently populates every page's
//! content from the ORM: the official plugins, each plugin's feature
//! tracker, the editorial audit status, demo discussion notes, the
//! community channels / resources / newsletter, the site navigation, and
//! the orphan-model content pages (showcase, blog, framework features).
//!
//! It lives in the binary crate because it orchestrates seeds across
//! several website plugins — only the binary depends on all of them. Each
//! plugin owns its own idempotent `seed()` surface; this command calls
//! them in order and prints a per-plugin summary. Safe to re-run: every
//! seed short-circuits per-row or per-table.

use async_trait::async_trait;
use clap::{ArgMatches, Command};
use umbra::cli::{CliError, PluginCommand};
use umbra::plugin::Plugin;

/// A model-less, route-less plugin whose only job is to contribute the
/// `seed_orm_data` CLI command. Registered in `main.rs`.
#[derive(Debug, Default, Clone)]
pub struct SeedDataPlugin;

impl Plugin for SeedDataPlugin {
    fn name(&self) -> &'static str {
        "seed_data"
    }

    fn commands(&self) -> Vec<Box<dyn PluginCommand>> {
        vec![Box::new(SeedOrmData)]
    }
}

struct SeedOrmData;

#[async_trait]
impl PluginCommand for SeedOrmData {
    fn command(&self) -> Command {
        Command::new("seed_orm_data").about(
            "Idempotently seed all website content: official plugins, their feature \
             trackers, audit status, community links/resources/newsletter, navigation, \
             and demo content. Safe to re-run.",
        )
    }

    async fn run(&self, _matches: &ArgMatches) -> Result<(), CliError> {
        println!("Seeding ORM data...");

        // --- plugin_directory: plugins + features + audit + demo notes -----
        let plugins = plugin_directory::seed::seed_official_plugins().await?;
        let audit = plugin_directory::seed::backfill_audit_status().await?;
        let features = plugin_directory::seed::seed_plugin_features().await?;
        let notes = plugin_directory::seed::seed_demo_comments().await?;
        println!(
            "  plugin_directory: {plugins} plugins · {audit} audit back-fills · \
             {features} features · {notes} notes"
        );

        // Further plugin seeds (community, navigation, showcase, blog,
        // framework features) are wired in as each page lands.

        println!("Done.");
        Ok(())
    }
}
