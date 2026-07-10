//! ReviewsPlugin — the `/reviews` developer testimonials page.
//!
//! Renders approved `Review` rows (featured first). The submission form
//! lands later; for now this is a read page backed by seeded testimonials.

pub mod models;
pub mod seed;

pub use models::{Review, ReviewModeration, ReviewUsageContext};

use std::path::PathBuf;

use serde::Serialize;
use umbral::migrate::ModelMeta;
use umbral::plugin::{AppContext, Plugin, PluginError};
use umbral::templates::context;
use umbral::web::{Html, Router, StatusCode, get};

use models::review;

#[derive(Debug, Default, Clone)]
pub struct ReviewsPlugin;

impl Plugin for ReviewsPlugin {
    fn name(&self) -> &'static str {
        "reviews"
    }

    /// FKs into `auth_user`. Held by alphabetical luck before; now declared.
    fn dependencies(&self) -> &'static [&'static str] {
        &["auth"]
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![ModelMeta::for_::<models::Review>()]
    }

    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }

    fn routes(&self) -> Router {
        Router::new().route("/reviews", get(reviews_page))
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        // Review seed writes are command-driven via `seed_orm_data`.
        Ok(())
    }
}

/// One testimonial card. Used by `/reviews` and (via
/// [`featured_reviews`]) the homepage trust strip, so it's `pub` —
/// the byline/initials presentation logic lives here once.
#[derive(Debug, Serialize)]
pub struct ReviewView {
    pub rating: i32,
    pub title: String,
    /// Markdown body (rendered with `| markdown` in the template).
    pub body: String,
    /// "Staff Engineer · Acme Inc." — role and company joined.
    pub byline: String,
    /// Short usage-context tag, e.g. "Work project".
    pub context: &'static str,
    /// Two-letter monogram from the company/role.
    pub initials: String,
    pub verified: bool,
    /// The Umbral version the review is grounded in (e.g. "0.0.1").
    /// `None` hides the version footer rather than fabricating one.
    pub umbral_version: Option<String>,
}

impl From<Review> for ReviewView {
    fn from(r: Review) -> Self {
        let company = r.company.clone().unwrap_or_default();
        let role = r.role.clone().unwrap_or_default();
        let byline = match (role.is_empty(), company.is_empty()) {
            (false, false) => format!("{role} · {company}"),
            (false, true) => role.clone(),
            (true, false) => company.clone(),
            (true, true) => "Umbral developer".to_string(),
        };
        let mono_src = if !company.is_empty() { &company } else { &role };
        ReviewView {
            rating: r.rating.clamp(0, 5),
            title: r.title,
            body: r.body,
            byline,
            context: context_label(r.usage_context),
            initials: initials(mono_src),
            verified: r.verified_developer,
            umbral_version: r.umbral_version,
        }
    }
}

fn context_label(c: ReviewUsageContext) -> &'static str {
    match c {
        ReviewUsageContext::SideProject => "Side project",
        ReviewUsageContext::WorkProject => "Work project",
        ReviewUsageContext::InternalTool => "Internal tool",
        ReviewUsageContext::Library => "Library",
        ReviewUsageContext::Evaluation => "Evaluation",
    }
}

fn initials(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().filter(|w| !w.is_empty()).collect();
    let out: String = match words.as_slice() {
        [] => "★".to_string(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
    };
    out.to_uppercase()
}

async fn reviews_page() -> Result<Html<String>, (StatusCode, String)> {
    render_reviews()
        .await
        .map(Html)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Approved reviews, featured first. Shared by `/reviews` (all of them)
/// and the homepage trust strip (a small curated slice). `limit = None`
/// returns every approved review.
pub async fn approved_reviews(limit: Option<usize>) -> Result<Vec<ReviewView>, String> {
    let mut qs = Review::objects()
        .filter(review::MODERATION.eq("approved"))
        .order_by(review::FEATURED.desc())
        .order_by(review::CREATED_AT.desc())
        .order_by(review::ID.desc());
    if let Some(n) = limit {
        qs = qs.limit(n as u64);
    }
    Ok(qs
        .fetch()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(ReviewView::from)
        .collect())
}

/// The homepage's curated set of approved reviews (featured first).
/// A thin alias over [`approved_reviews`] so the public plugin reads
/// intent at the call site.
pub async fn featured_reviews(limit: usize) -> Result<Vec<ReviewView>, String> {
    approved_reviews(Some(limit)).await
}

/// Load + render `/reviews`: approved reviews, featured first. Public so a
/// render smoke-test can drive it without an axum runtime.
pub async fn render_reviews() -> Result<String, String> {
    let reviews = approved_reviews(None).await?;
    umbral::templates::render("reviews/reviews.html", &context! { reviews => reviews })
        .map_err(|e| e.to_string())
}
