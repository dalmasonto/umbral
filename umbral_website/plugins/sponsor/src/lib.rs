//! SponsorPlugin — the `/sponsor` page: partners grid + "Talk to us" form.
//!
//! The header's Sponsor button offers two paths: sponsor directly on
//! GitHub (external), or "Talk to us", which lands here. Partners are
//! admin-managed ([`Partner`]); the form captures a [`SponsorInquiry`] for
//! the team to follow up on.

pub mod models;

pub use models::{InquiryStatus, Partner, PartnerTier, SponsorInquiry};

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use umbral::forms::{FormValidate, ValidationErrors};
use umbral::migrate::ModelMeta;
use umbral::plugin::{AppContext, Plugin, PluginError};
use umbral::templates::context;
use umbral::web::{Form, Html, IntoResponse, Query, Redirect, Response, Router, StatusCode, get, post};

use models::partner;

/// The GitHub Sponsors URL the header button and the page link out to.
pub const GITHUB_SPONSORS_URL: &str = "https://github.com/sponsors/dalmasonto";

#[derive(Debug, Default, Clone)]
pub struct SponsorPlugin;

impl Plugin for SponsorPlugin {
    fn name(&self) -> &'static str {
        "sponsor"
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![
            ModelMeta::for_::<models::Partner>(),
            ModelMeta::for_::<models::SponsorInquiry>(),
        ]
    }

    fn templates_dirs(&self) -> Vec<PathBuf> {
        vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")]
    }

    fn routes(&self) -> Router {
        Router::new()
            .route("/sponsor", get(sponsor_page))
            .route("/sponsor", post(post_inquiry))
    }

    fn on_ready(&self, _ctx: &AppContext) -> Result<(), PluginError> {
        Ok(())
    }
}

/// One partner card on `/sponsor`.
#[derive(Debug, Serialize)]
struct PartnerView {
    name: String,
    description: String,
    website_url: Option<String>,
    /// Human tier label for the card badge (always set — defaults to
    /// "Community").
    tier: String,
    /// First letter of the name — the monogram when there's no logo.
    initial: String,
    /// Resolved logo URL, or `None` to render the monogram.
    logo: Option<String>,
}

impl From<Partner> for PartnerView {
    fn from(p: Partner) -> Self {
        let initial = p
            .name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "·".to_string());
        PartnerView {
            name: p.name,
            description: p.description,
            website_url: p.website_url,
            tier: p.tier.label().to_string(),
            initial,
            // StoragePlugin's media side serves uploads at /media/<key> (see main.rs).
            logo: p.logo.map(|f| format!("/media/{}", f.key())),
        }
    }
}

/// Query string for `/sponsor`: `?submitted=1` renders the success state.
#[derive(Debug, Default, serde::Deserialize)]
struct SponsorQuery {
    submitted: Option<String>,
}

async fn sponsor_page(
    Query(q): Query<SponsorQuery>,
) -> Result<Html<String>, (StatusCode, String)> {
    let submitted = q.submitted.as_deref() == Some("1");
    render_sponsor(submitted, None, &HashMap::new())
        .await
        .map(Html)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Handle a posted sponsor inquiry (`POST /sponsor`). On success persists a
/// `New` inquiry and redirects to `/sponsor?submitted=1`; on failure the
/// form re-renders with per-field errors and the typed values kept.
async fn post_inquiry(
    Form(form): Form<HashMap<String, String>>,
) -> Result<Response, (StatusCode, String)> {
    match create_inquiry(&form).await {
        Ok(_id) => Ok(Redirect::to("/sponsor?submitted=1").into_response()),
        Err(errs) => {
            let html = render_sponsor(false, Some(&errs), &form)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
            Ok((StatusCode::UNPROCESSABLE_ENTITY, Html(html)).into_response())
        }
    }
}

/// Validate a sponsor inquiry through the `SponsorInquiry` Form derive, set
/// the server-managed status to `New`, persist it, and return the new id.
/// Public so a smoke-test can drive the validate → create path without an
/// axum runtime.
pub async fn create_inquiry(data: &HashMap<String, String>) -> Result<i64, ValidationErrors> {
    let mut inquiry = SponsorInquiry::validate(data).await?;
    inquiry.status = InquiryStatus::New;
    let created = SponsorInquiry::objects()
        .create(inquiry)
        .await
        .map_err(write_to_validation)?;
    Ok(created.id)
}

/// Load active partners (featured first) and render `/sponsor`. Public so a
/// render smoke-test can drive the full path without an axum runtime.
pub async fn render_sponsor(
    submitted: bool,
    errors: Option<&ValidationErrors>,
    form: &HashMap<String, String>,
) -> Result<String, String> {
    // Partners are best-effort: before the sponsor migration is applied the
    // table doesn't exist, so a query error means "no partners yet" — we
    // log it and render the honest empty state rather than 500 the page.
    let partners: Vec<PartnerView> = match Partner::objects()
        .filter(partner::ACTIVE.eq(true))
        .order_by(partner::FEATURED.desc())
        .order_by(partner::DISPLAY_ORDER.asc())
        .order_by(partner::ID.asc())
        .fetch()
        .await
    {
        Ok(rows) => rows.into_iter().map(PartnerView::from).collect(),
        Err(e) => {
            tracing::warn!(
                "sponsor: partners query failed (run makemigrations + migrate?): {e}"
            );
            Vec::new()
        }
    };

    umbral::templates::render(
        "sponsor/sponsor.html",
        &context! {
            partners => partners,
            submitted => submitted,
            errors => errors_to_ctx(errors),
            form => form,
            github_sponsors_url => GITHUB_SPONSORS_URL,
        },
    )
    .map_err(|e| e.to_string())
}

/// Flatten [`ValidationErrors`] into the `{ field: first_message, form:
/// first_non_field }` shape the template reads.
fn errors_to_ctx(errors: Option<&ValidationErrors>) -> serde_json::Value {
    let Some(errors) = errors else {
        return serde_json::Value::Null;
    };
    let mut out = serde_json::Map::new();
    for (field, msgs) in &errors.fields {
        if let Some(first) = msgs.first() {
            out.insert(field.clone(), serde_json::Value::String(first.clone()));
        }
    }
    if let Some(first) = errors.non_field.first() {
        out.insert("form".to_string(), serde_json::Value::String(first.clone()));
    }
    serde_json::Value::Object(out)
}

/// Turn an ORM write error into a field-keyed [`ValidationErrors`] so a
/// persistence failure renders friendly text rather than a 500.
fn write_to_validation(e: umbral::orm::write::WriteError) -> ValidationErrors {
    let mut errs = ValidationErrors::new();
    errs.add_non_field(format!("Could not save your inquiry: {e}"));
    errs
}
