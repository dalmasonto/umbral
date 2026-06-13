//! FeaturesPlugin — the `/features` framework capability catalog.
//!
//! Loads `FeatureCategory` rows with their `FrameworkFeature` children
//! (one categories query + one batched features query, grouped in memory)
//! and renders a category-grouped catalog with status badges.

pub mod models;
pub mod seed;

pub use models::{
    FeatureCategory, FeatureMaturity, FeatureStatus, FeatureStatusEvent, FrameworkFeature,
};

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use umbra::migrate::ModelMeta;
use umbra::plugin::{AppContext, Plugin, PluginError};
use umbra::templates::context;
use umbra::web::{Html, Router, StatusCode, get};

use models::{feature_category, framework_feature};

#[derive(Debug, Default, Clone)]
pub struct FeaturesPlugin;

impl Plugin for FeaturesPlugin {
    fn name(&self) -> &'static str {
        "features"
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![
            ModelMeta::for_::<models::FeatureCategory>(),
            ModelMeta::for_::<models::FrameworkFeature>(),
            ModelMeta::for_::<models::FeatureStatusEvent>(),
        ]
    }

    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }

    fn routes(&self) -> Router {
        Router::new().route("/features", get(features_page))
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        tokio::spawn(async move {
            match seed::seed().await {
                Ok((0, 0)) => {}
                Ok((c, f)) => tracing::info!("features: seeded {c} categories, {f} features"),
                Err(e) => tracing::warn!("features: seed failed: {e}"),
            }
        });
        Ok(())
    }
}

/// A category section on `/features`, with its feature rows.
#[derive(Debug, Serialize)]
struct CategoryView {
    name: String,
    description: String,
    features: Vec<FeatureView>,
}

/// One feature row in a category.
#[derive(Debug, Serialize)]
struct FeatureView {
    name: String,
    summary: String,
    status: String,
    /// "ok" / "warn" / "muted" — drives the badge colour.
    kind: &'static str,
    maturity: String,
}

/// Map a `FeatureStatus` to a (label, badge-kind) pair.
fn status_badge(s: FeatureStatus) -> (&'static str, &'static str) {
    match s {
        FeatureStatus::Shipped => ("shipped", "ok"),
        FeatureStatus::Usable => ("usable", "ok"),
        FeatureStatus::Experimental => ("experimental", "warn"),
        FeatureStatus::InProgress => ("in progress", "warn"),
        FeatureStatus::Planned => ("planned", "muted"),
        FeatureStatus::Deferred => ("deferred", "muted"),
        FeatureStatus::Deprecated => ("deprecated", "muted"),
        FeatureStatus::None => ("—", "muted"),
    }
}

async fn features_page() -> Result<Html<String>, (StatusCode, String)> {
    render_features()
        .await
        .map(Html)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Load + render `/features`. One categories query + one batched features
/// query (`WHERE category IN (ids)`), grouped in memory — no N+1. Public
/// so a render smoke-test can drive it without an axum runtime.
pub async fn render_features() -> Result<String, String> {
    let categories = FeatureCategory::objects()
        .filter(feature_category::VISIBLE.eq(true))
        .order_by(feature_category::DISPLAY_ORDER.asc())
        .order_by(feature_category::ID.asc())
        .fetch()
        .await
        .map_err(|e| e.to_string())?;

    let ids: Vec<i64> = categories.iter().map(|c| c.id).collect();
    let mut features_by_cat: HashMap<i64, Vec<FeatureView>> = HashMap::new();
    if !ids.is_empty() {
        let rows = FrameworkFeature::objects()
            .filter(framework_feature::CATEGORY.in_(&ids))
            .filter(framework_feature::VISIBLE.eq(true))
            .order_by(framework_feature::DISPLAY_ORDER.asc())
            .order_by(framework_feature::ID.asc())
            .fetch()
            .await
            .map_err(|e| e.to_string())?;
        for f in rows {
            let (status, kind) = status_badge(f.status);
            features_by_cat
                .entry(f.category.id())
                .or_default()
                .push(FeatureView {
                    name: f.name,
                    summary: f.short_summary,
                    status: status.to_string(),
                    kind,
                    maturity: format!("{:?}", f.maturity).to_lowercase(),
                });
        }
    }

    let cats: Vec<CategoryView> = categories
        .into_iter()
        .map(|c| CategoryView {
            name: c.name,
            description: c.description.unwrap_or_default(),
            features: features_by_cat.remove(&c.id).unwrap_or_default(),
        })
        .collect();

    umbra::templates::render("features/features.html", &context! { categories => cats })
        .map_err(|e| e.to_string())
}
