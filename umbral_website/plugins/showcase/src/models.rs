//! Models for the `showcase` plugin.
//!
//! `ShowcaseEntry` is a public submission of a site or app built
//! on Umbral. Visitors submit via the form; the admin moderates
//! from the queue and approves / rejects / features entries.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use umbral::prelude::*;
use umbral_auth::AuthUser;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ShowcaseStatus {
    #[default]
    Draft,
    Submitted,
    Verified,
    Featured,
    Archived,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ShowcaseProjectType {
    #[default]
    Website,
    Dashboard,
    ApiService,
    InternalTool,
    MobileBackend,
    Demo,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ShowcaseDatabase {
    #[default]
    Sqlite,
    Postgres,
    Mysql,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices, Default)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ShowcaseDeployment {
    #[default]
    SelfHosted,
    FlyIo,
    Railway,
    Render,
    Aws,
    Gcp,
    Azure,
    Vercel,
    Other,
}

/// A website or application built on Umbral. Public form lets
/// site owners submit their project; admin moderation drives the
/// `status` enum.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model, umbral::forms::Form)]
#[umbral(
    soft_delete,
    plugin = "showcase",
    display = "Showcase",
    icon = "gallery-horizontal"
)]
pub struct ShowcaseEntry {
    pub id: i64,

    #[umbral(noform, on_delete = "set_null")]
    pub submitted_by: Option<ForeignKey<AuthUser>>,

    #[form(required, length(min = 2, max = 120))]
    pub project_name: String,

    #[form(required, url, length(max = 400))]
    pub url: String,

    #[form(required, length(min = 2, max = 120))]
    pub owner: String,

    #[form(required, length(min = 20, max = 400))]
    pub short_description: String,

    #[form(optional, length(max = 20_000))]
    #[umbral(widget = "markdown")]
    pub long_content: Option<String>,

    #[form(optional, url, length(max = 400))]
    pub screenshot_url: Option<String>,

    #[form(optional, url, length(max = 400))]
    pub logo_url: Option<String>,

    #[umbral(noform, choices, index, default = "website")]
    pub project_type: ShowcaseProjectType,

    /// Comma-separated list of Umbral plugins used (e.g. "auth,
    /// admin, rest"). Free-text for now; an admin-curated picker
    /// is a future round.
    #[form(optional, length(max = 400))]
    #[umbral(help = "A comma-separated list of Umbral plugins used (e.g. \"auth, admin, rest\")")]
    pub plugins_used: Option<String>,

    #[umbral(noform, choices, default = "sqlite")]
    pub database_backend: ShowcaseDatabase,

    #[umbral(noform, choices, default = "self_hosted")]
    pub deployment_platform: ShowcaseDeployment,

    /// When the project launched. Optional because early-stage
    /// demos often don't have a launch date. The form macro v1
    /// doesn't support `NaiveDate`, so this is admin-set only;
    /// the public form leaves it null.
    #[umbral(noform)]
    pub launch_date: Option<NaiveDate>,

    #[form(optional, url, length(max = 400))]
    pub source_url: Option<String>,

    /// Set to true by an admin after verifying the site actually
    /// runs on Umbral. Public form leaves it false.
    #[umbral(noform, default = "false", index)]
    pub verified: bool,

    /// Homepage highlight; admin-set.
    #[umbral(noform, default = "false", index)]
    pub featured: bool,

    #[umbral(noform, choices, index, default = "submitted")]
    pub status: ShowcaseStatus,

    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,

    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,

    #[umbral(noform, index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

impl Default for ShowcaseEntry {
    fn default() -> Self {
        Self {
            id: 0,
            submitted_by: None,
            project_name: String::new(),
            url: String::new(),
            owner: String::new(),
            short_description: String::new(),
            long_content: None,
            screenshot_url: None,
            logo_url: None,
            project_type: ShowcaseProjectType::default(),
            plugins_used: None,
            database_backend: ShowcaseDatabase::default(),
            deployment_platform: ShowcaseDeployment::default(),
            launch_date: None,
            source_url: None,
            verified: false,
            featured: false,
            status: ShowcaseStatus::default(),
            created_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
            updated_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
            deleted_at: None,
        }
    }
}
