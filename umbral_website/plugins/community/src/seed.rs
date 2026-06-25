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

const SENTINMAIL_URL: &str =
    "https://sentinmail.app/subscribe/24479467-8815-497e-94f5-61aa11278687";

/// One seeded social channel.
struct Channel {
    name: &'static str,
    slug: &'static str,
    platform: SocialPlatform,
    url: &'static str,
    description: &'static str,
    /// Brand colour (CSS) for the card icon + `--brand` hover accent.
    color: &'static str,
    /// `true` → render the muted "Coming soon" card (channel not live yet).
    coming_soon: bool,
    order: i32,
}

const CHANNELS: &[Channel] = &[
    Channel {
        name: "GitHub",
        slug: "github",
        platform: SocialPlatform::GitHub,
        url: "https://github.com/dalmasonto/umbral",
        description: "Source, issues & pull requests",
        color: "var(--ink)",
        coming_soon: false,
        order: 10,
    },
    Channel {
        name: "Reddit",
        slug: "reddit",
        platform: SocialPlatform::Reddit,
        url: "https://reddit.com/r/umbralrs",
        description: "r/umbralrs - links & threads",
        color: "#FF4500",
        coming_soon: false,
        order: 40,
    },
    Channel {
        name: "Blog & RSS",
        slug: "blog",
        platform: SocialPlatform::Rss,
        url: "/blog",
        description: "Deep dives & release notes",
        color: "#E8771A",
        coming_soon: false,
        order: 60,
    },
    Channel {
        name: "Docs",
        slug: "docs",
        platform: SocialPlatform::Docs,
        url: "/docs",
        description: "Guides & API",
        color: "var(--accent)",
        coming_soon: false,
        order: 70,
    },
    Channel {
        name: "Discussions",
        slug: "discussions",
        platform: SocialPlatform::GitHub,
        url: "https://github.com/dalmasonto/umbral/discussions",
        description: "Q&A, ideas & show-and-tell",
        color: "#6E40C9",
        coming_soon: true,
        order: 20,
    },
    Channel {
        name: "Discord",
        slug: "discord",
        platform: SocialPlatform::Discord,
        url: "https://discord.gg/umbral",
        description: "Real-time chat & support",
        color: "#5865F2",
        coming_soon: true,
        order: 30,
    },
    Channel {
        name: "X",
        slug: "x",
        platform: SocialPlatform::X,
        url: "https://x.com/umbral_rs",
        description: "@umbral_rs - quick updates",
        color: "var(--ink)",
        coming_soon: true,
        order: 50,
    },
];

/// Seed the social channels. Idempotent UPSERT by slug: re-running refreshes
/// the config fields (colour, coming-soon, ordering, copy) on existing rows
/// AND inserts any newly-added channels. Returns the count touched.
pub async fn seed_social_links() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    use crate::models::social_link;
    use serde_json::json;

    let mut n = 0;
    for c in CHANNELS {
        let now = Utc::now();
        let existing = SocialLink::objects()
            .filter(social_link::SLUG.eq(c.slug))
            .first()
            .await?;
        if existing.is_some() {
            // Refresh the config fields in place (PATCH semantics — only the
            // columns in the map are written; created_at is left untouched).
            let mut values = serde_json::Map::new();
            values.insert("name".into(), json!(c.name));
            values.insert(
                "platform".into(),
                json!(format!("{:?}", c.platform).to_lowercase()),
            );
            values.insert("url".into(), json!(c.url));
            values.insert("description".into(), json!(c.description));
            values.insert("color".into(), json!(c.color));
            values.insert("coming_soon".into(), json!(c.coming_soon));
            values.insert("display_order".into(), json!(c.order));
            values.insert("active".into(), json!(true));
            values.insert("updated_at".into(), json!(now));
            SocialLink::objects()
                .filter(social_link::SLUG.eq(c.slug))
                .update_values(values)
                .await?;
        } else {
            let row = SocialLink {
                id: 0,
                name: c.name.to_string(),
                slug: c.slug.to_string(),
                platform: c.platform,
                url: c.url.to_string(),
                icon_key: format!("{:?}", c.platform).to_lowercase(),
                description: Some(c.description.to_string()),
                color: Some(c.color.to_string()),
                coming_soon: c.coming_soon,
                display_order: c.order,
                active: true,
                created_at: now,
                updated_at: now,
                deleted_at: None,
            };
            SocialLink::objects().create(row).await?;
        }
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
        name: "Umbral Newsletter".to_string(),
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
    NewsletterList {
        title: "Releases",
        summary: "every version, the moment it ships",
        order: 10,
    },
    NewsletterList {
        title: "The Umbral Monthly",
        summary: "features, plugin spotlights & tutorials",
        order: 20,
    },
    NewsletterList {
        title: "Security advisories",
        summary: "rare, but you'll want them",
        order: 30,
    },
    NewsletterList {
        title: "Plugin authors",
        summary: "contract changes & deprecations",
        order: 40,
    },
    NewsletterList {
        title: "Early access",
        summary: "preview builds & open RFCs",
        order: 50,
    },
    NewsletterList {
        title: "Built with Umbral",
        summary: "real-world showcases",
        order: 60,
    },
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
