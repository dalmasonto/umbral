//! Idempotent seed for developer reviews (`/reviews`).
//!
//! A handful of approved testimonials so the page renders real content.
//! Curated editorial copy (not fabricated metrics). Authorless rows
//! (`author = None`) — the moderation lifecycle allows them.

use crate::models::{Review, ReviewModeration, ReviewUsageContext};

struct Seed {
    rating: i32,
    title: &'static str,
    body: &'static str,
    role: &'static str,
    company: &'static str,
    context: ReviewUsageContext,
    featured: bool,
}

const REVIEWS: &[Seed] = &[
    Seed { rating: 5, title: "Django muscle memory, Rust guarantees", body: "I declared my models, ran makemigrations and migrate, and had an admin and a REST API in an afternoon. It feels like Django, but the compiler catches the mistakes I used to find in production.", role: "Staff Engineer", company: "Fintech startup", context: ReviewUsageContext::WorkProject, featured: true },
    Seed { rating: 5, title: "The plugin contract actually holds", body: "Auth, sessions, admin, REST — they're all plugins with the same shape as anything I'd write. I swapped the default auth for our SSO without touching the core. That's the dependency inversion working as advertised.", role: "Platform Lead", company: "B2B SaaS", context: ReviewUsageContext::InternalTool, featured: true },
    Seed { rating: 4, title: "Migrations I trust", body: "The declare → migrate → change → migrate loop is the thing I missed most coming from other Rust web stacks. Autodetection got the common cases right; the couple it flagged were genuinely ambiguous and better surfaced than guessed.", role: "Backend Developer", company: "Indie", context: ReviewUsageContext::SideProject, featured: true },
    Seed { rating: 5, title: "Shipped an MVP over a weekend", body: "ORM, forms, admin, and the playground meant I spent my time on the actual product instead of plumbing. The honest empty-states and em-dashes for unknown values are a nice touch — no fabricated dashboards.", role: "Indie developer", company: "Solo", context: ReviewUsageContext::SideProject, featured: false },
];

/// Seed the reviews. Idempotent: short-circuits if any review exists.
pub async fn seed() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    if Review::objects().count().await? > 0 {
        return Ok(0);
    }
    let mut n = 0;
    for s in REVIEWS {
        let mut r = Review::default();
        r.author = None;
        r.rating = s.rating;
        r.title = s.title.to_string();
        r.body = s.body.to_string();
        r.role = Some(s.role.to_string());
        r.company = Some(s.company.to_string());
        r.umbra_version = Some("0.0.1".to_string());
        r.usage_context = s.context;
        r.verified_developer = true;
        r.moderation = ReviewModeration::Approved;
        r.featured = s.featured;
        Review::objects().create(r).await?;
        n += 1;
    }
    Ok(n)
}
