// plugins/projects/src/models.rs

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbral::prelude::*;
use umbral_auth::AuthUser;

// A small, closed set of states. `Choices` teaches the framework the
// allowed values, so the admin renders a dropdown, the OpenAPI schema
// lists an enum, and GraphQL gets an enum type - all from this one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Choices)]
#[choices(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Blocked,
    Done,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Project {
    pub id: i64,
    #[umbral(unique)]
    pub slug: String,
    #[umbral(string)]
    pub name: String,
    pub description: Option<String>,
    #[umbral(default = "false")]
    pub is_archived: bool,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Label {
    pub id: i64,
    #[umbral(unique, string)]
    pub name: String,
    #[umbral(default = "#888888")]
    pub color: String,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Task {
    pub id: i64,
    #[umbral(string)]
    pub title: String,
    pub description: Option<String>,
    #[umbral(choices)]
    pub status: TaskStatus,

    // A required FK: every task belongs to a project.
    pub project: ForeignKey<Project>,
    // An optional FK: a task may be unassigned. Nullable in the DB,
    // and therefore `Option<...>` in Rust. The type carries the nullability.
    pub assignee: Option<ForeignKey<AuthUser>>,

    // Many-to-many. The framework creates a `task_labels` junction table
    // at migration time; you never hand-write it.
    #[sqlx(skip)]
    #[serde(skip)]
    pub labels: M2M<Label>,

    pub due_date: Option<DateTime<Utc>>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
    #[umbral(auto_now)]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Comment {
    pub id: i64,
    pub task: ForeignKey<Task>,
    pub author: ForeignKey<AuthUser>,
    pub body: String,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
}
