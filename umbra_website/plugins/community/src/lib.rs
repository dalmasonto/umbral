//! CommunityPlugin — the `/community` hub page (channels + newsletter).
//!
//! DB-driven: channels come from `SocialLink`, the subscribe URL from the
//! active `NewsletterConfig`, and the newsletter list blurbs from
//! `CommunityResource` rows (`kind = newsletter`). Seeded idempotently in
//! `on_ready` and via the `seed_orm_data` command.

pub mod models;
pub mod seed;

pub use models::{
    CommunityResource, CommunityResourceKind, NewsletterConfig, NewsletterProvider, SocialLink,
    SocialPlatform,
};

use std::path::PathBuf;

use serde::Serialize;
use umbra::migrate::ModelMeta;
use umbra::plugin::{AppContext, Plugin, PluginError};
use umbra::templates::context;
use umbra::web::{Html, Router, StatusCode, get};

use models::{community_resource, social_link};

#[derive(Debug, Default, Clone)]
pub struct CommunityPlugin;

impl Plugin for CommunityPlugin {
    fn name(&self) -> &'static str {
        "community"
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![
            ModelMeta::for_::<models::SocialLink>(),
            ModelMeta::for_::<models::CommunityResource>(),
            ModelMeta::for_::<models::NewsletterConfig>(),
        ]
    }

    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }

    fn routes(&self) -> Router {
        Router::new().route("/community", get(community_page))
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        // Seed the hub's content on first boot. Idempotent; failures log
        // but never crash startup (the page falls back to empty sections).
        tokio::spawn(async move {
            match seed::seed().await {
                Ok((0, 0, 0)) => {}
                Ok((c, n, l)) => {
                    tracing::info!("community: seeded {c} channels, {n} newsletter, {l} lists")
                }
                Err(e) => tracing::warn!("community: seed failed: {e}"),
            }
        });
        Ok(())
    }
}

/// A social channel card on `/community`. `slug` keys the icon SVG and
/// brand colour in the template; the rest is content from `SocialLink`.
#[derive(Debug, Serialize)]
struct ChannelView {
    slug: String,
    name: String,
    url: String,
    description: String,
    /// CSS colour for the `--brand` hover accent, keyed by slug.
    brand: &'static str,
    /// `true` for an off-site URL (→ `target="_blank"`); internal links
    /// like `/blog` stay same-tab.
    external: bool,
}

/// One newsletter list blurb (a SentinMail subscriber group).
#[derive(Debug, Serialize)]
struct ListView {
    title: String,
    summary: String,
}

/// Per-channel brand colour (the `--brand` hover token). Keyed by slug
/// because GitHub and Discussions share `platform = GitHub` but want
/// different accents.
fn brand_for(slug: &str) -> &'static str {
    match slug {
        "github" | "x" => "var(--ink)",
        "discussions" => "#6E40C9",
        "discord" => "#5865F2",
        "reddit" => "#FF4500",
        "blog" => "#E8771A",
        _ => "var(--accent)",
    }
}

async fn community_page() -> Result<Html<String>, (StatusCode, String)> {
    render_community()
        .await
        .map(Html)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Load + render `/community`. Public so a render smoke-test can drive the
/// query → view-model → template path without an axum runtime.
pub async fn render_community() -> Result<String, String> {
    // Channels: active social links, ordered.
    let channels: Vec<ChannelView> = SocialLink::objects()
        .filter(social_link::ACTIVE.eq(true))
        .order_by(social_link::DISPLAY_ORDER.asc())
        .order_by(social_link::ID.asc())
        .fetch()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|l| ChannelView {
            brand: brand_for(&l.slug),
            external: l.url.starts_with("http"),
            slug: l.slug,
            name: l.name,
            url: l.url,
            description: l.description.unwrap_or_default(),
        })
        .collect();

    // The active newsletter config drives the subscribe button URL.
    // Falls back to the canonical SentinMail link so the button is never
    // dead even before the seed runs.
    let newsletter_url = NewsletterConfig::objects()
        .filter(models::newsletter_config::ACTIVE.eq(true))
        .order_by(models::newsletter_config::ID.asc())
        .first()
        .await
        .map_err(|e| e.to_string())?
        .map(|n| n.hosted_subscribe_url)
        .unwrap_or_else(|| {
            "https://sentinmail.app/subscribe/24479467-8815-497e-94f5-61aa11278687".to_string()
        });

    // Newsletter list blurbs.
    let lists: Vec<ListView> = CommunityResource::objects()
        .filter(community_resource::KIND.eq("newsletter"))
        .order_by(community_resource::DISPLAY_ORDER.asc())
        .order_by(community_resource::ID.asc())
        .fetch()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|r| ListView {
            title: r.title,
            summary: r.summary.unwrap_or_default(),
        })
        .collect();

    umbra::templates::render(
        "community/community.html",
        &context! {
            channels => channels,
            newsletter_url => newsletter_url,
            lists => lists,
        },
    )
    .map_err(|e| e.to_string())
}
