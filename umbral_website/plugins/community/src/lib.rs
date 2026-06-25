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
use umbral::migrate::ModelMeta;
use umbral::plugin::{AppContext, Plugin, PluginError};
use umbral::templates::context;
use umbral::web::{Html, Router, StatusCode, get};

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

/// A social channel card. `slug` keys the brand icon SVG in the template;
/// the rest (including the brand colour + coming-soon flag) is content from
/// `SocialLink`. Public so the homepage handler can reuse the same view via
/// [`home_channels`].
#[derive(Debug, Serialize)]
pub struct ChannelView {
    pub slug: String,
    pub name: String,
    pub url: String,
    pub description: String,
    /// CSS colour for the icon fill + `--brand` hover accent, straight from
    /// the model (falls back to `var(--accent)` when the row has none).
    pub color: String,
    /// Render the muted "Coming soon" card instead of a clickable link.
    pub coming_soon: bool,
    /// `true` for an off-site URL (→ `target="_blank"`); internal links
    /// like `/blog` stay same-tab.
    pub external: bool,
}

/// One newsletter list blurb (a SentinMail subscriber group).
#[derive(Debug, Serialize)]
struct ListView {
    title: String,
    summary: String,
}

/// The active channels as view models, ordered. Shared by `/community` and
/// the homepage's "Join the ecosystem" grid so both render the same
/// model-driven cards. The brand colour + coming-soon flag come straight
/// from each `SocialLink` row.
pub async fn home_channels() -> Result<Vec<ChannelView>, String> {
    Ok(SocialLink::objects()
        .filter(social_link::ACTIVE.eq(true))
        .order_by(social_link::DISPLAY_ORDER.asc())
        .order_by(social_link::ID.asc())
        .fetch()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|l| ChannelView {
            color: l.color.unwrap_or_else(|| "var(--accent)".into()),
            coming_soon: l.coming_soon,
            external: l.url.starts_with("http"),
            slug: l.slug,
            name: l.name,
            url: l.url,
            description: l.description.unwrap_or_default(),
        })
        .collect())
}

/// The active newsletter subscribe URL, with the canonical SentinMail link
/// as the fallback so the button is never dead before the seed runs. Shared
/// by `/community` and the homepage newsletter card.
pub async fn newsletter_url() -> String {
    NewsletterConfig::objects()
        .filter(models::newsletter_config::ACTIVE.eq(true))
        .order_by(models::newsletter_config::ID.asc())
        .first()
        .await
        .ok()
        .flatten()
        .map(|n| n.hosted_subscribe_url)
        .unwrap_or_else(|| {
            "https://sentinmail.app/subscribe/24479467-8815-497e-94f5-61aa11278687".to_string()
        })
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
    // Channels: active social links, ordered. Shared with the homepage.
    let channels = home_channels().await?;

    // The active newsletter config drives the subscribe button URL.
    let newsletter_url = newsletter_url().await;

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

    umbral::templates::render(
        "community/community.html",
        &context! {
            channels => channels,
            newsletter_url => newsletter_url,
            lists => lists,
        },
    )
    .map_err(|e| e.to_string())
}
