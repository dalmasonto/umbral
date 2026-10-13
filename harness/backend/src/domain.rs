//! Shared query + serialization logic.
//!
//! Everything here is pure application behaviour: no tracing, no auth
//! resolution, no request extractors. The binaries' handlers own those, which
//! is what lets `bench-prod` measure the same queries `bench-instrumented`
//! decomposes.

use std::collections::HashMap;

use chrono::Utc;
use serde_json::{Value, json};
use umbral::prelude::*;
use umbral::web::ApiError;
use umbral_auth::{AuthUser, auth_user};

use crate::models::{Comment, Follow, Like, Post, comment, follow, like, post};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Truncate a post body to the app's 280-char limit, on a character boundary.
pub fn clamp_body(raw: &str) -> String {
    raw.chars().take(280).collect()
}

pub fn author_name(user: &AuthUser) -> String {
    user.username.clone()
}

/// Author name for a post fetched WITH `select_related("author")`.
pub fn hydrated_author_name(p: &Post) -> String {
    p.author
        .__resolved()
        .map(author_name)
        .unwrap_or_else(|| format!("user_{}", p.author.id()))
}

fn counts_by_fk<T: IntoIterator<Item = i64>>(ids: T) -> HashMap<i64, usize> {
    let mut m: HashMap<i64, usize> = HashMap::new();
    for id in ids {
        *m.entry(id).or_default() += 1;
    }
    m
}

// ---------------------------------------------------------------------------
// Feed
// ---------------------------------------------------------------------------

/// The batched feed read: one window of posts WITH their authors.
pub async fn recent_posts(limit: u64) -> Result<Vec<Post>, ApiError> {
    let posts = Post::objects()
        .select_related("author")
        .order_by(post::CREATED_AT.desc())
        .limit(limit)
        .fetch()
        .await?;
    Ok(posts)
}

/// Likes and comments for `ids`, as per-post counts: `(like_counts, comment_counts)`.
pub async fn engage_counts(
    ids: &[i64],
) -> Result<(HashMap<i64, usize>, HashMap<i64, usize>), ApiError> {
    let likes = if ids.is_empty() {
        Vec::new()
    } else {
        Like::objects().filter(like::POST.in_(ids)).fetch().await?
    };
    let comments = if ids.is_empty() {
        Vec::new()
    } else {
        Comment::objects()
            .filter(comment::POST.in_(ids))
            .fetch()
            .await?
    };
    let like_counts = counts_by_fk(likes.iter().map(|l| l.post.id()));
    let comment_counts = counts_by_fk(comments.iter().map(|c| c.post.id()));
    Ok((like_counts, comment_counts))
}

pub fn feed_payload(
    posts: &[Post],
    likes: &HashMap<i64, usize>,
    comments: &HashMap<i64, usize>,
) -> Value {
    let rows: Vec<Value> = posts
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "author": { "id": p.author.id(), "username": hydrated_author_name(p) },
                "body": p.body,
                "created_at": p.created_at,
                "likes": likes.get(&p.id).copied().unwrap_or(0),
                "comments": comments.get(&p.id).copied().unwrap_or(0),
            })
        })
        .collect();
    json!({ "posts": rows, "count": rows.len() })
}

/// The N+1 walk — intentionally unbatched so the detector flags it.
pub async fn naive_feed_payload(limit: u64) -> Result<Value, ApiError> {
    let posts = Post::objects()
        .order_by(post::CREATED_AT.desc())
        .limit(limit)
        .fetch()
        .await?;

    let mut rows = Vec::with_capacity(posts.len());
    for p in &posts {
        let author = AuthUser::objects()
            .filter(auth_user::ID.eq(p.author.id()))
            .first()
            .await?;
        let likes = Like::objects().filter(like::POST.eq(p.id)).count().await?;
        let comments = Comment::objects()
            .filter(comment::POST.eq(p.id))
            .count()
            .await?;

        rows.push(json!({
            "id": p.id,
            "author": {
                "id": p.author.id(),
                "username": author.map(|a| a.username).unwrap_or_default(),
            },
            "body": p.body,
            "created_at": p.created_at,
            "likes": likes,
            "comments": comments,
        }));
    }

    Ok(json!({ "posts": rows, "count": rows.len() }))
}

// ---------------------------------------------------------------------------
// The heavy profile aggregate
// ---------------------------------------------------------------------------

pub async fn profile_payload(user_id: i64) -> Result<Value, ApiError> {
    let user = AuthUser::objects()
        .filter(auth_user::ID.eq(user_id))
        .first()
        .await?
        .ok_or_else(|| ApiError::not_found(format!("user {user_id} not found")))?;

    let followers = Follow::objects()
        .filter(follow::FOLLOWING.eq(user_id))
        .count()
        .await?;
    let following = Follow::objects()
        .filter(follow::FOLLOWER.eq(user_id))
        .count()
        .await?;
    let post_count = Post::objects()
        .filter(post::AUTHOR.eq(user_id))
        .count()
        .await?;

    let recent = Post::objects()
        .filter(post::AUTHOR.eq(user_id))
        .select_related("author")
        .order_by(post::CREATED_AT.desc())
        .limit(10)
        .fetch()
        .await?;

    let ids: Vec<i64> = recent.iter().map(|p| p.id).collect();

    let (likes_received, comments_received, like_rows, comment_rows) = if ids.is_empty() {
        (0, 0, Vec::new(), Vec::new())
    } else {
        let lr = Like::objects().filter(like::POST.in_(&ids)).count().await?;
        let cr = Comment::objects()
            .filter(comment::POST.in_(&ids))
            .count()
            .await?;
        let lrows = Like::objects().filter(like::POST.in_(&ids)).fetch().await?;
        let crows = Comment::objects()
            .filter(comment::POST.in_(&ids))
            .fetch()
            .await?;
        (lr, cr, lrows, crows)
    };

    let like_counts = counts_by_fk(like_rows.iter().map(|l| l.post.id()));
    let comment_counts = counts_by_fk(comment_rows.iter().map(|c| c.post.id()));

    let recent_rows: Vec<Value> = recent
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "body": p.body,
                "created_at": p.created_at,
                "likes": like_counts.get(&p.id).copied().unwrap_or(0),
                "comments": comment_counts.get(&p.id).copied().unwrap_or(0),
            })
        })
        .collect();
    let top_post = recent_rows
        .iter()
        .max_by_key(|r| r["likes"].as_u64().unwrap_or(0))
        .cloned()
        .unwrap_or(Value::Null);

    Ok(json!({
        "user": { "id": user.id, "username": user.username, "joined": user.date_joined },
        "followers": followers,
        "following": following,
        "posts": post_count,
        "likes_received": likes_received,
        "comments_received": comments_received,
        "recent_posts": recent_rows,
        "top_post": top_post,
    }))
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

pub async fn create_post(user_id: i64, body: &str) -> Result<Value, ApiError> {
    let saved = Post::objects()
        .create(Post {
            id: 0,
            author: ForeignKey::new(user_id),
            body: body.to_string(),
            created_at: Utc::now(),
        })
        .await?;
    Ok(json!({ "id": saved.id, "author_id": user_id }))
}

pub async fn like_post(user_id: i64, post_id: i64) -> Result<Value, ApiError> {
    let exists = Like::objects()
        .filter(like::USER.eq(user_id) & like::POST.eq(post_id))
        .exists()
        .await?;
    if !exists {
        Like::objects()
            .create(Like {
                id: 0,
                user: ForeignKey::new(user_id),
                post: ForeignKey::new(post_id),
                created_at: Utc::now(),
            })
            .await?;
    }
    Ok(json!({ "ok": true, "created": !exists }))
}

pub async fn follow_user(user_id: i64, target_id: i64) -> Result<Value, ApiError> {
    let exists = Follow::objects()
        .filter(follow::FOLLOWER.eq(user_id) & follow::FOLLOWING.eq(target_id))
        .exists()
        .await?;
    if !exists {
        Follow::objects()
            .create(Follow {
                id: 0,
                follower: ForeignKey::new(user_id),
                following: ForeignKey::new(target_id),
                created_at: Utc::now(),
            })
            .await?;
    }
    Ok(json!({ "ok": true, "created": !exists }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_body_truncates_to_280_chars() {
        let long = "a".repeat(300);
        assert_eq!(clamp_body(&long).chars().count(), 280);
    }

    #[test]
    fn clamp_body_truncates_on_char_boundaries_not_bytes() {
        // 300 multi-byte chars: a byte-slice would panic or split a codepoint.
        let long = "é".repeat(300);
        let out = clamp_body(&long);
        assert_eq!(out.chars().count(), 280);
        assert_eq!(out, "é".repeat(280)); // valid UTF-8 preserved
    }

    #[test]
    fn clamp_body_leaves_short_bodies_untouched() {
        assert_eq!(clamp_body("hello"), "hello");
    }
}
