//! Shared query + serialization logic.
//!
//! Everything here is pure application behaviour: no tracing, no auth
//! resolution, no request extractors. The binaries' handlers own those, which
//! is what lets `bench-prod` measure the same queries `bench-instrumented`
//! decomposes.
//!
//! # The query/assemble seam
//!
//! Each read path is deliberately cut in two, because the two halves belong to
//! different buckets in the layer ledger and only the caller can say where the
//! cut is:
//!
//! - a `*_query` / `recent_posts` / `*_page_data` function does the reads and
//!   returns the rows — the caller stamps it as **db wall**;
//! - a `*_payload` / `*_page_payload` function folds those rows and builds the
//!   response — the caller stamps it as **serialize**.
//!
//! Folding the rows into counts is assembly, not database work, so it lives in
//! the payload half. Collapsing the two halves into one call would move that
//! fold into the db bucket and leave `serialize` reading zero on a route that
//! serializes a payload — a diagnostic that states something false.

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

/// The feed's `limit` query parameter, defaulted and clamped. Shared so the
/// two binaries cannot drift on `?limit=0` or `?limit=999`.
pub fn feed_limit(raw: Option<i64>) -> u64 {
    raw.unwrap_or(25).clamp(1, 100) as u64
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

/// Fold one id per row into per-id counts. Pure assembly: callers run it
/// inside their serialize window, never around an await.
fn counts_by_fk<T: IntoIterator<Item = i64>>(ids: T) -> HashMap<i64, usize> {
    let mut m: HashMap<i64, usize> = HashMap::new();
    for id in ids {
        *m.entry(id).or_default() += 1;
    }
    m
}

// ---------------------------------------------------------------------------
// Feed — the batched read
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

/// The raw engagement rows for `ids` — likes, then comments. Counting them is
/// the payload half's job.
pub async fn engage_rows(ids: &[i64]) -> Result<(Vec<Like>, Vec<Comment>), ApiError> {
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
    Ok((likes, comments))
}

pub fn feed_payload(posts: &[Post], likes: &[Like], comments: &[Comment]) -> Value {
    let like_counts = counts_by_fk(likes.iter().map(|l| l.post.id()));
    let comment_counts = counts_by_fk(comments.iter().map(|c| c.post.id()));
    let rows: Vec<Value> = posts
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "author": { "id": p.author.id(), "username": hydrated_author_name(p) },
                "body": p.body,
                "created_at": p.created_at,
                "likes": like_counts.get(&p.id).copied().unwrap_or(0),
                "comments": comment_counts.get(&p.id).copied().unwrap_or(0),
            })
        })
        .collect();
    json!({ "posts": rows, "count": rows.len() })
}

// ---------------------------------------------------------------------------
// Feed — the N+1 walk
// ---------------------------------------------------------------------------

/// One post plus the per-post facts the walk fetches for it.
pub struct NaivePost {
    pub post: Post,
    pub author: Option<AuthUser>,
    pub likes: i64,
    pub comments: i64,
}

/// The N+1 walk — intentionally unbatched so the detector flags it: one
/// author fetch and two COUNTs per post, in that order.
pub async fn naive_feed_query(limit: u64) -> Result<Vec<NaivePost>, ApiError> {
    let posts = Post::objects()
        .order_by(post::CREATED_AT.desc())
        .limit(limit)
        .fetch()
        .await?;

    let mut facts = Vec::with_capacity(posts.len());
    for post in posts {
        let author = AuthUser::objects()
            .filter(auth_user::ID.eq(post.author.id()))
            .first()
            .await?;
        let likes = Like::objects()
            .filter(like::POST.eq(post.id))
            .count()
            .await?;
        let comments = Comment::objects()
            .filter(comment::POST.eq(post.id))
            .count()
            .await?;
        facts.push(NaivePost {
            post,
            author,
            likes,
            comments,
        });
    }
    Ok(facts)
}

pub fn naive_feed_payload(facts: &[NaivePost]) -> Value {
    let rows: Vec<Value> = facts
        .iter()
        .map(|f| {
            json!({
                "id": f.post.id,
                "author": {
                    "id": f.post.author.id(),
                    "username": f.author.as_ref().map(|a| a.username.clone()).unwrap_or_default(),
                },
                "body": f.post.body,
                "created_at": f.post.created_at,
                "likes": f.likes,
                "comments": f.comments,
            })
        })
        .collect();
    json!({ "posts": rows, "count": rows.len() })
}

// ---------------------------------------------------------------------------
// The heavy profile aggregate
// ---------------------------------------------------------------------------

/// Everything the profile read fetches, before any folding.
pub struct ProfileData {
    pub user: AuthUser,
    pub followers: i64,
    pub following: i64,
    pub post_count: i64,
    pub recent: Vec<Post>,
    pub likes_received: i64,
    pub comments_received: i64,
    pub like_rows: Vec<Like>,
    pub comment_rows: Vec<Comment>,
}

pub async fn profile_query(user_id: i64) -> Result<ProfileData, ApiError> {
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

    Ok(ProfileData {
        user,
        followers,
        following,
        post_count,
        recent,
        likes_received,
        comments_received,
        like_rows,
        comment_rows,
    })
}

pub fn profile_payload(data: &ProfileData) -> Value {
    let like_counts = counts_by_fk(data.like_rows.iter().map(|l| l.post.id()));
    let comment_counts = counts_by_fk(data.comment_rows.iter().map(|c| c.post.id()));

    let recent_rows: Vec<Value> = data
        .recent
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

    json!({
        "user": { "id": data.user.id, "username": data.user.username, "joined": data.user.date_joined },
        "followers": data.followers,
        "following": data.following,
        "posts": data.post_count,
        "likes_received": data.likes_received,
        "comments_received": data.comments_received,
        "recent_posts": recent_rows,
        "top_post": top_post,
    })
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

// ---------------------------------------------------------------------------
// HTML pages
//
// The page reads live here for the same reason the JSON ones do: `bench-prod`
// mounts the same pages and constructs no telemetry, so the pages cannot reach
// for a `Trace` extension. Rendering stays in `views.rs`.
// ---------------------------------------------------------------------------

/// One line of the "created_at" the page templates print.
fn page_timestamp(t: &chrono::DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M").to_string()
}

/// The feed page's data — the same two reads the JSON feed does.
pub struct FeedPageData {
    pub posts: Vec<Post>,
    pub likes: Vec<Like>,
    pub comments: Vec<Comment>,
}

pub async fn feed_page_data(limit: u64) -> Result<FeedPageData, ApiError> {
    let posts = Post::objects()
        .select_related("author")
        .order_by(post::CREATED_AT.desc())
        .limit(limit)
        .fetch()
        .await?;
    let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();
    let (likes, comments) = engage_rows(&ids).await?;
    Ok(FeedPageData {
        posts,
        likes,
        comments,
    })
}

/// The `posts` rows `feed.html` iterates.
pub fn feed_page_payload(data: &FeedPageData) -> Vec<Value> {
    let like_counts = counts_by_fk(data.likes.iter().map(|l| l.post.id()));
    let comment_counts = counts_by_fk(data.comments.iter().map(|c| c.post.id()));
    data.posts
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "author_id": p.author.id(),
                "author": p.author.__resolved().map(|u| u.username.clone()).unwrap_or_default(),
                "body": p.body,
                "created_at": page_timestamp(&p.created_at),
                "likes": like_counts.get(&p.id).copied().unwrap_or(0),
                "comments": comment_counts.get(&p.id).copied().unwrap_or(0),
            })
        })
        .collect()
}

/// The profile page's data — five reads, and no engagement fetch.
pub struct ProfilePageData {
    pub user: AuthUser,
    pub followers: i64,
    pub following: i64,
    pub post_count: i64,
    pub recent: Vec<Post>,
}

pub async fn profile_page_data(user_id: i64) -> Result<ProfilePageData, ApiError> {
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
        .order_by(post::CREATED_AT.desc())
        .limit(10)
        .fetch()
        .await?;
    Ok(ProfilePageData {
        user,
        followers,
        following,
        post_count,
        recent,
    })
}

/// Everything `profile.html` renders.
pub struct ProfilePageVars {
    pub username: String,
    pub user_id: i64,
    pub followers: i64,
    pub following: i64,
    pub post_count: i64,
    pub posts: Vec<Value>,
}

pub fn profile_page_payload(data: &ProfilePageData) -> ProfilePageVars {
    ProfilePageVars {
        username: data.user.username.clone(),
        user_id: data.user.id,
        followers: data.followers,
        following: data.following,
        post_count: data.post_count,
        posts: data
            .recent
            .iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "body": p.body,
                    "created_at": page_timestamp(&p.created_at),
                })
            })
            .collect(),
    }
}

/// The post page's data — one row plus its two counts.
pub struct PostPageData {
    pub post: Post,
    pub likes: i64,
    pub comments: i64,
}

/// The post page's data — one statement, where there used to be three: the
/// post (with its author prefetched alongside — `fetch_annotated` hydrates
/// `select_related` since 2026-10-15) plus both engagement counts as
/// correlated scalar subqueries on the same row. The counts do the same
/// index work as the two separate `COUNT(*)`s they replace; what goes away
/// is the round trips themselves — and this route is the second read on 90%
/// of the workload's iterations, so two of ~4.7 statements per request is
/// the largest single statement cut in the app.
pub async fn post_page_data(post_id: i64) -> Result<PostPageData, ApiError> {
    let mut rows = Post::objects()
        .select_related("author")
        .filter(post::ID.eq(post_id))
        .annotate_count("like")
        .annotate_count("comment")
        .fetch_annotated()
        .await?;
    let Some((post, annotations)) = rows.pop() else {
        return Err(ApiError::not_found(format!("post {post_id} not found")));
    };
    let count_of = |key: &str| {
        annotations
            .get(key)
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    };
    Ok(PostPageData {
        post,
        likes: count_of("like_count"),
        comments: count_of("comment_count"),
    })
}

/// Everything `post.html` renders.
pub struct PostPageVars {
    pub post_id: i64,
    pub author_id: i64,
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub likes: i64,
    pub comments: i64,
}

pub fn post_page_payload(data: &PostPageData) -> PostPageVars {
    PostPageVars {
        post_id: data.post.id,
        author_id: data.post.author.id(),
        author: data
            .post
            .author
            .__resolved()
            .map(|u| u.username.clone())
            .unwrap_or_default(),
        body: data.post.body.clone(),
        created_at: page_timestamp(&data.post.created_at),
        likes: data.likes,
        comments: data.comments,
    }
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

    #[test]
    fn feed_limit_defaults_and_clamps() {
        assert_eq!(feed_limit(None), 25);
        assert_eq!(feed_limit(Some(0)), 1);
        assert_eq!(feed_limit(Some(-5)), 1);
        assert_eq!(feed_limit(Some(999)), 100);
        assert_eq!(feed_limit(Some(5)), 5);
    }
}
