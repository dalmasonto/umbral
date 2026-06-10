//! Framework feature catalog models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbra::prelude::*;

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
#[umbra(
    soft_delete,
    plugin = "features",
    display = "Feature categories",
    icon = "folder-kanban"
)]
pub struct FeatureCategory {
    pub id: i64,
    #[umbra(unique, string, max_length = 100)]
    pub name: String,
    #[umbra(unique, max_length = 120)]
    pub slug: String,
    pub description: Option<String>,
    #[umbra(default = "0", index)]
    pub display_order: i32,
    #[umbra(default = "true", index)]
    pub visible: bool,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(soft_delete, plugin = "features", display = "Framework features", icon = "sparkles")]
pub struct FrameworkFeature {
    pub id: i64,
    #[umbra(on_delete = "cascade")]
    pub category: ForeignKey<FeatureCategory>,
    #[umbra(string, max_length = 140)]
    pub name: String,
    #[umbra(unique, max_length = 160)]
    pub slug: String,
    pub short_summary: String,
    pub full_description: String,
    #[umbra(choices, index)]
    pub status: FeatureStatus,
    #[umbra(choices, index)]
    pub maturity: FeatureMaturity,
    pub docs_url: Option<String>,
    pub example_url: Option<String>,
    pub related_plugin_slug: Option<String>,
    pub release_target: Option<String>,
    #[umbra(default = "0", index)]
    pub display_order: i32,
    #[umbra(default = "true", index)]
    pub visible: bool,
    pub metadata: Option<serde_json::Value>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
#[umbra(
    soft_delete,
    plugin = "features",
    display = "Feature status events",
    icon = "history"
)]
pub struct FeatureStatusEvent {
    pub id: i64,
    #[umbra(on_delete = "cascade")]
    pub feature: ForeignKey<FrameworkFeature>,
    #[umbra(choices, default = "none")]
    pub previous_status: FeatureStatus,
    #[umbra(choices, index)]
    pub new_status: FeatureStatus,
    pub note: Option<String>,
    pub changed_by: Option<String>,
    #[umbra(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbra(auto_now)]
    pub updated_at: DateTime<Utc>,
    #[umbra(index)]
    pub deleted_at: Option<DateTime<Utc>>,
}
