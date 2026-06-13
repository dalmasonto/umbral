//! CommunityPlugin — the `/community` hub page (channels + newsletter).
//!
//! Static for now; the `SocialLink` / `CommunityResource` /
//! `NewsletterConfig` models exist so this can become admin-managed
//! later, but a hardcoded hub unblocks the links that pointed at a
//! missing `/community`.

pub mod models;

pub use models::{
    CommunityResource, CommunityResourceKind, NewsletterConfig, NewsletterProvider, SocialLink,
    SocialPlatform,
};

use std::path::PathBuf;

use umbra::migrate::ModelMeta;
use umbra::plugin::{AppContext, Plugin, PluginError};
use umbra::templates::context;
use umbra::web::{Html, Router, StatusCode, get};

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
        Ok(())
    }
}

async fn community_page() -> Result<Html<String>, (StatusCode, String)> {
    let body = umbra::templates::render("community/community.html", &context! {})
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Html(body))
}
