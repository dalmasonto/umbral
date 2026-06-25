//! Framework feature catalog models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbral::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum FeatureStatus {
    None,
    Shipped,
    Usable,
    Experimental,
    InProgress,
    Planned,
    Deferred,
    Deprecated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum FeatureMaturity {
    Stable,
    Beta,
    Alpha,
    Design,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    soft_delete,
    plugin = "features",
    display = "Feature categories",
    icon = "folder-kanban"
)]
pub struct FeatureCategory {
    pub id: i64,
    #[umbral(unique, string, max_length = 100)]
    pub name: String,
    #[umbral(unique, max_length = 120)]
    pub slug: String,
    pub description: Option<String>,
    #[umbral(default = "0", index)]
    pub display_order: i32,
    #[umbral(default = "true", index)]
    pub visible: bool,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(soft_delete, plugin = "features", display = "Framework features", icon = "sparkles")]
pub struct FrameworkFeature {
    pub id: i64,
    #[umbral(on_delete = "cascade")]
    pub category: ForeignKey<FeatureCategory>,
    #[umbral(string, max_length = 140)]
    pub name: String,
    #[umbral(unique, max_length = 160)]
    pub slug: String,
    pub short_summary: String,
    pub full_description: String,
    #[umbral(choices, index)]
    pub status: FeatureStatus,
    #[umbral(choices, index)]
    pub maturity: FeatureMaturity,
    pub docs_url: Option<String>,
    pub example_url: Option<String>,
    pub related_plugin_slug: Option<String>,
    pub release_target: Option<String>,
    #[umbral(default = "0", index)]
    pub display_order: i32,
    #[umbral(default = "true", index)]
    pub visible: bool,
    pub metadata: Option<serde_json::Value>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbral(
    soft_delete,
    plugin = "features",
    display = "Feature status events",
    icon = "history"
)]
pub struct FeatureStatusEvent {
    pub id: i64,
    #[umbral(on_delete = "cascade")]
    pub feature: ForeignKey<FrameworkFeature>,
    #[umbral(choices, default = "none")]
    pub previous_status: FeatureStatus,
    #[umbral(choices, index)]
    pub new_status: FeatureStatus,
    pub note: Option<String>,
    pub changed_by: Option<String>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbral(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}
