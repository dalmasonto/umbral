//! Idempotent seed for the community hub: social channels, the
//! newsletter config, and the newsletter list descriptions.
//!
//! Editorial content (channel names, descriptions, list blurbs) — safe to
//! seed. Each seeder short-circuits when its table already has rows.

use crate::models::{
    CommunityResource, CommunityResourceKind, NewsletterConfig, NewsletterProvider, SocialLink,
    SocialPlatform,
};
use chrono::Utc;

const SENTINMAIL_URL: &str = "https://sentinmail.app/subscribe/24479467-8815-497e-94f5-61aa11278687";

/// One seeded social channel.
struct Channel {
    name: &'static str,
    slug: &'static str,
    platform: SocialPlatform,
    url: &'static str,
    description: &'static str,
    order: i32,
}

const CHANNELS: &[Channel] = &[
    Channel { name: "GitHub", slug: "github", platform: SocialPlatform::GitHub, url: "https://github.com/dalmasonto/umbra", description: "Source, issues & pull requests", order: 10 },
    Channel { name: "Discussions", slug: "discussions", platform: SocialPlatform::GitHub, url: "https://github.com/dalmasonto/umbra/discussions", description: "Q&A, ideas & show-and-tell", order: 20 },
    Channel { name: "Discord", slug: "discord", platform: SocialPlatform::Discord, url: "https://discord.gg/umbra", description: "Real-time chat & support", order: 30 },
    Channel { name: "Reddit", slug: "reddit", platform: SocialPlatform::Reddit, url: "https://reddit.com/r/umbra", description: "r/umbra - links & threads", order: 40 },
    Channel { name: "X", slug: "x", platform: SocialPlatform::X, url: "https://x.com/umbra_rs", description: "@umbra_rs - quick updates", order: 50 },
    Channel { name: "Blog & RSS", slug: "blog", platform: SocialPlatform::Rss, url: "/blog", description: "Deep dives & release notes", order: 60 },
];

/// Seed the social channels. Idempotent (short-circuits if any exist).
pub async fn seed_social_links() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    if SocialLink::objects().count().await? > 0 {
        return Ok(0);
    }
    let mut n = 0;
    for c in CHANNELS {
        let now = Utc::now();
        let row = SocialLink {
            id: 0,
            name: c.name.to_string(),
            slug: c.slug.to_string(),
            platform: c.platform,
            url: c.url.to_string(),
            icon_key: format!("{:?}", c.platform).to_lowercase(),
            description: Some(c.description.to_string()),
            display_order: c.order,
            active: true,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };
        SocialLink::objects().create(row).await?;
        n += 1;
    }
    Ok(n)
}

/// Seed the newsletter config (the SentinMail subscribe URL). Idempotent.
pub async fn seed_newsletter() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    if NewsletterConfig::objects().count().await? > 0 {
        return Ok(0);
    }
    let now = Utc::now();
    let row = NewsletterConfig {
        id: 0,
        name: "Umbra Newsletter".to_string(),
        provider: NewsletterProvider::Sentinmail,
        hosted_subscribe_url: SENTINMAIL_URL.to_string(),
        api_endpoint: None,
        list_id: None,
        success_redirect_url: None,
        failure_redirect_url: None,
        daily_digest_time: None,
        active: true,
        metadata: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    };
    NewsletterConfig::objects().create(row).await?;
    Ok(1)
}

/// One seeded newsletter list (a SentinMail subscriber group).
struct NewsletterList {
    title: &'static str,
    summary: &'static str,
    order: i32,
}

const LISTS: &[NewsletterList] = &[
    NewsletterList { title: "Releases", summary: "every version, the moment it ships", order: 10 },
    NewsletterList { title: "The Umbra Monthly", summary: "features, plugin spotlights & tutorials", order: 20 },
    NewsletterList { title: "Security advisories", summary: "rare, but you'll want them", order: 30 },
    NewsletterList { title: "Plugin authors", summary: "contract changes & deprecations", order: 40 },
    NewsletterList { title: "Early access", summary: "preview builds & open RFCs", order: 50 },
    NewsletterList { title: "Built with Umbra", summary: "real-world showcases", order: 60 },
];

/// Seed the newsletter list descriptions as `CommunityResource` rows
/// (`kind = newsletter`). Idempotent (short-circuits if any newsletter
/// resource exists).
pub async fn seed_newsletter_lists() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    use crate::models::community_resource;
    if CommunityResource::objects()
        .filter(community_resource::KIND.eq("newsletter"))
        .count()
        .await?
        > 0
    {
        return Ok(0);
    }
    let mut n = 0;
    for l in LISTS {
        let now = Utc::now();
        let row = CommunityResource {
            id: 0,
            title: l.title.to_string(),
            slug: format!("newsletter-{}", l.title.to_lowercase().replace(' ', "-")),
            kind: CommunityResourceKind::Newsletter,
            url: SENTINMAIL_URL.to_string(),
            summary: Some(l.summary.to_string()),
            is_featured: false,
            display_order: l.order,
            metadata: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };
        CommunityResource::objects().create(row).await?;
        n += 1;
    }
    Ok(n)
}

/// Run every community seed. Returns `(channels, newsletter, lists)`.
pub async fn seed() -> Result<(usize, usize, usize), Box<dyn std::error::Error + Send + Sync>> {
    let channels = seed_social_links().await?;
    let newsletter = seed_newsletter().await?;
    let lists = seed_newsletter_lists().await?;
    Ok((channels, newsletter, lists))
}
