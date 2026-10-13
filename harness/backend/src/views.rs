//! Server-rendered HTML pages — the template-rendering arm of the harness.
//!
//! The same data the JSON API serves, rendered through minijinja so the
//! `render` bucket in the layer ledger has a real workload: the feed and
//! profile pages both fetch, assemble a context, and render a template.

use std::collections::HashMap;
use std::time::Instant;

use axum::extract::Extension;

use umbral::templates::context;
use umbral::web::{ApiError, Html, Path, Query};
use umbral_auth::{AuthUser, auth_user};

use crate::api::FeedQuery;
use crate::models::{Comment, Follow, Like, Post, comment, follow, like, post};
use crate::telemetry::Trace;

fn count_by_post<T: IntoIterator<Item = i64>>(ids: T) -> HashMap<i64, usize> {
    let mut m: HashMap<i64, usize> = HashMap::new();
    for id in ids {
        *m.entry(id).or_default() += 1;
    }
    m
}

pub async fn feed_page(
    Query(q): Query<FeedQuery>,
    Extension(trace): Extension<Trace>,
) -> Result<Html<String>, ApiError> {
    trace.handler_start();
    let limit = q.limit.unwrap_or(25).clamp(1, 100) as u64;

    let t = Instant::now();
    let posts = Post::objects()
        .select_related("author")
        .order_by(post::CREATED_AT.desc())
        .limit(limit)
        .fetch()
        .await?;
    let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();
    let likes = if ids.is_empty() {
        Vec::new()
    } else {
        Like::objects().filter(like::POST.in_(&ids)).fetch().await?
    };
    let comments = if ids.is_empty() {
        Vec::new()
    } else {
        Comment::objects()
            .filter(comment::POST.in_(&ids))
            .fetch()
            .await?
    };
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let like_counts = count_by_post(likes.iter().map(|l| l.post.id()));
    let comment_counts = count_by_post(comments.iter().map(|c| c.post.id()));
    let rows: Vec<serde_json::Value> = posts
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "author_id": p.author.id(),
                "author": p.author.__resolved().map(|u| u.username.clone()).unwrap_or_default(),
                "body": p.body,
                "created_at": p.created_at.format("%Y-%m-%d %H:%M").to_string(),
                "likes": like_counts.get(&p.id).copied().unwrap_or(0),
                "comments": comment_counts.get(&p.id).copied().unwrap_or(0),
            })
        })
        .collect();

    let t = Instant::now();
    let body = umbral::templates::render("feed.html", &context!(posts => rows))?;
    trace.add_render(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Html(body))
}

pub async fn profile_page(
    Path(id): Path<i64>,
    Extension(trace): Extension<Trace>,
) -> Result<Html<String>, ApiError> {
    trace.handler_start();

    let t = Instant::now();
    let user = AuthUser::objects()
        .filter(auth_user::ID.eq(id))
        .first()
        .await?
        .ok_or_else(|| ApiError::not_found(format!("user {id} not found")))?;
    let followers = Follow::objects().filter(follow::FOLLOWING.eq(id)).count().await?;
    let following = Follow::objects().filter(follow::FOLLOWER.eq(id)).count().await?;
    let post_count = Post::objects().filter(post::AUTHOR.eq(id)).count().await?;
    let recent = Post::objects()
        .filter(post::AUTHOR.eq(id))
        .order_by(post::CREATED_AT.desc())
        .limit(10)
        .fetch()
        .await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let t = Instant::now();
    let rows: Vec<serde_json::Value> = recent
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "body": p.body,
                "created_at": p.created_at.format("%Y-%m-%d %H:%M").to_string(),
            })
        })
        .collect();

    let body = umbral::templates::render(
        "profile.html",
        &context!(
            username => user.username,
            user_id => user.id,
            followers, following, post_count,
            posts => rows
        ),
    )?;
    trace.add_render(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Html(body))
}

pub async fn post_page(
    Path(id): Path<i64>,
    Extension(trace): Extension<Trace>,
) -> Result<Html<String>, ApiError> {
    trace.handler_start();

    let t = Instant::now();
    let post = Post::objects()
        .select_related("author")
        .filter(post::ID.eq(id))
        .first()
        .await?
        .ok_or_else(|| ApiError::not_found(format!("post {id} not found")))?;
    let likes = Like::objects().filter(like::POST.eq(id)).count().await?;
    let comments = Comment::objects()
        .filter(comment::POST.eq(id))
        .count()
        .await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let t = Instant::now();
    let body = umbral::templates::render(
        "post.html",
        &context!(
            post_id => post.id,
            author_id => post.author.id(),
            author => post.author.__resolved().map(|u| u.username.clone()).unwrap_or_default(),
            body => post.body,
            created_at => post.created_at.format("%Y-%m-%d %H:%M").to_string(),
            likes, comments
        ),
    )?;
    trace.add_render(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Html(body))
}
