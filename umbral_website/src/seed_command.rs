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
use umbral::cli::{CliError, PluginCommand};
use umbral::plugin::Plugin;

/// A model-less, route-less plugin whose only job is to contribute the
/// `seed_orm_data` CLI command. Registered in `main.rs`.
#[derive(Debug, Default, Clone)]
pub struct SeedDataPlugin;

impl Plugin for SeedDataPlugin {
    fn name(&self) -> &'static str {
        "seed_data"
    }

    fn commands(&self) -> Vec<Box<dyn PluginCommand>> {
        vec![Box::new(SeedOrmData), Box::new(SeedChannels)]
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

        // --- community: channels + newsletter + list blurbs ----------------
        let (channels, newsletter, lists) = community::seed::seed().await?;
        println!("  community: {channels} channels · {newsletter} newsletter · {lists} lists");

        // --- features: the framework capability catalog --------------------
        let (cats, feats) = features::seed::seed().await?;
        println!("  features: {cats} categories · {feats} features");

        // --- reviews: developer testimonials -------------------------------
        let reviews = reviews::seed::seed().await?;
        println!("  reviews: {reviews} reviews");

        // --- showcase: dogfooding gallery entries --------------------------
        let showcase = showcase::seed::seed().await?;
        println!("  showcase: {showcase} entries");

        // Further plugin seeds (navigation, blog, changelog) are wired in
        // as each page lands.

        println!("Done.");
        Ok(())
    }
}

/// `cargo run -- seed_channels` — (re)seed just the community channels.
/// Idempotent UPSERT by slug, so re-running refreshes each channel's brand
/// colour + coming-soon state and adds any newly-defined channels.
struct SeedChannels;

#[async_trait]
impl PluginCommand for SeedChannels {
    fn command(&self) -> Command {
        Command::new("seed_channels")
            .about("(Re)seed the community channels (idempotent upsert by slug).")
    }

    async fn run(&self, _matches: &ArgMatches) -> Result<(), CliError> {
        let channels = community::seed::seed_social_links().await?;
        println!("Seeded {channels} community channels.");
        Ok(())
    }
}
