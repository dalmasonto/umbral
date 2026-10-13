//! The bench domain: a social feed.
//!
//! Four tables on top of umbral-auth's `auth_user`:
//!   - `follow`  — directed follow edges (follower → following)
//!   - `post`    — authored posts
//!   - `like`    — one row per (user, post)
//!   - `comment` — replies to posts
//!
//! Shapes chosen so the read paths have real work to do: the heavy
//! `GET /api/users/{id}/profile` aggregates follower/following/post counts,
//! likes received, and a recent-post window — and `GET /api/feed-naive`
//! deliberately walks the N+1 path so the detector has a fingerprint to find.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use umbral::prelude::*;
use umbral_auth::AuthUser;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
// Columns are named after the FIELDS (`follower`, `following`) — NOT
// `follower_id`. unique_together entries must name real columns; a
// nonexistent name yields a corrupt constant-key unique index (logged as a
// gaps7 entry against the migration engine).
#[umbral(unique_together = [["follower", "following"]])]
pub struct Follow {
    pub id: i64,
    /// The user doing the following.
    #[umbral(index)]
    pub follower: ForeignKey<AuthUser>,
    /// The user being followed.
    #[umbral(index)]
    pub following: ForeignKey<AuthUser>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Post {
    pub id: i64,
    #[umbral(index)]
    pub author: ForeignKey<AuthUser>,
    /// 280-char post body — the realistic short-form shape.
    #[umbral(string, max_length = 280)]
    pub body: String,
    #[umbral(index)]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
// Field names are the column names — see the note on `Follow`.
#[umbral(unique_together = [["user", "post"]])]
pub struct Like {
    pub id: i64,
    #[umbral(index)]
    pub user: ForeignKey<AuthUser>,
    #[umbral(index)]
    pub post: ForeignKey<Post>,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Comment {
    pub id: i64,
    #[umbral(index)]
    pub post: ForeignKey<Post>,
    pub author: ForeignKey<AuthUser>,
    #[umbral(string, max_length = 280)]
    pub body: String,
    #[umbral(auto_now_add)]
    pub created_at: DateTime<Utc>,
}
