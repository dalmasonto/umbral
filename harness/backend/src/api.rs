//! JSON API — the load-test targets.
//!
//! Two deliberately different feed implementations:
//!   - `GET /api/feed`        — batched (select_related + IN-queries): the
//!                              shape the ORM is designed for.
//!   - `GET /api/feed-naive`  — the classic N+1 walk: one author fetch and
//!                              one COUNT per post. Exists so the telemetry's
//!                              N+1 detector has a real fingerprint to flag.
//!
//! `GET /api/users/{id}/profile` is the heavy aggregate the design's
//! scenarios hammer: follower/following/post counts, likes and comments
//! received, a recent-post window with per-post counts, and a top post.

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::extract::Extension;
use axum::response::sse::{Event, KeepAlive, Sse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use umbral::prelude::*;
use umbral::web::{ApiError, Json, Path, Query};
use umbral_auth::{AuthUser, auth_user};

use crate::models::{Comment, Follow, Like, Post, comment, follow, like, post};
use crate::probes::BenchUser;
use crate::telemetry::Trace;
use crate::{loadgen, telemetry};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn name_of(user: &AuthUser) -> String {
    user.username.clone()
}

/// Author name for a post fetched WITH `select_related("author")`.
fn hydrated_author_name(p: &Post) -> String {
    p.author
        .__resolved()
        .map(name_of)
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

#[derive(Debug, Deserialize)]
pub struct FeedQuery {
    pub limit: Option<i64>,
}

pub async fn feed(
    Query(q): Query<FeedQuery>,
    Extension(trace): Extension<Trace>,
) -> Result<Json<Value>, ApiError> {
    trace.handler_start();
    let limit = q.limit.unwrap_or(25).clamp(1, 100) as u64;

    let t = Instant::now();
    let posts = Post::objects()
        .select_related("author")
        .order_by(post::CREATED_AT.desc())
        .limit(limit)
        .fetch()
        .await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();

    let t = Instant::now();
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

    let t = Instant::now();
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
    let payload = json!({ "posts": rows, "count": rows.len() });
    trace.add_serialize(t.elapsed().as_micros() as u64);
    trace.handler_end();

    Ok(Json(payload))
}

/// The N+1 walk — intentionally unbatched so the detector flags it.
pub async fn feed_naive(
    Query(q): Query<FeedQuery>,
    Extension(trace): Extension<Trace>,
) -> Result<Json<Value>, ApiError> {
    trace.handler_start();
    let limit = q.limit.unwrap_or(25).clamp(1, 100) as u64;

    let t = Instant::now();
    let posts = Post::objects()
        .order_by(post::CREATED_AT.desc())
        .limit(limit)
        .fetch()
        .await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let mut rows = Vec::with_capacity(posts.len());
    for p in &posts {
        let t = Instant::now();
        let author = AuthUser::objects()
            .filter(auth_user::ID.eq(p.author.id()))
            .first()
            .await?;
        let likes = Like::objects().filter(like::POST.eq(p.id)).count().await?;
        let comments = Comment::objects()
            .filter(comment::POST.eq(p.id))
            .count()
            .await?;
        trace.add_db_wall(t.elapsed().as_micros() as u64);

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

    let t = Instant::now();
    let payload = json!({ "posts": rows, "count": rows.len() });
    trace.add_serialize(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Json(payload))
}

// ---------------------------------------------------------------------------
// The heavy profile aggregate
// ---------------------------------------------------------------------------

pub async fn profile(
    Path(id): Path<i64>,
    Extension(trace): Extension<Trace>,
) -> Result<Json<Value>, ApiError> {
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
        .select_related("author")
        .order_by(post::CREATED_AT.desc())
        .limit(10)
        .fetch()
        .await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let ids: Vec<i64> = recent.iter().map(|p| p.id).collect();

    let t = Instant::now();
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
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let t = Instant::now();
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

    let payload = json!({
        "user": { "id": user.id, "username": user.username, "joined": user.date_joined },
        "followers": followers,
        "following": following,
        "posts": post_count,
        "likes_received": likes_received,
        "comments_received": comments_received,
        "recent_posts": recent_rows,
        "top_post": top_post,
    });
    trace.add_serialize(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Json(payload))
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreatePost {
    pub body: String,
}

pub async fn create_post(
    Extension(user): Extension<Option<BenchUser>>,
    Extension(trace): Extension<Trace>,
    Json(input): Json<CreatePost>,
) -> Result<Json<Value>, ApiError> {
    trace.handler_start();
    let user = user.ok_or_else(|| ApiError::unauthorized("x-bench-user header required"))?;
    let body: String = input.body.chars().take(280).collect();

    let t = Instant::now();
    let saved = Post::objects()
        .create(Post {
            id: 0,
            author: ForeignKey::new(user.id),
            body,
            created_at: Utc::now(),
        })
        .await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Json(json!({ "id": saved.id, "author_id": user.id })))
}

pub async fn like_post(
    Path(id): Path<i64>,
    Extension(user): Extension<Option<BenchUser>>,
    Extension(trace): Extension<Trace>,
) -> Result<Json<Value>, ApiError> {
    trace.handler_start();
    let user = user.ok_or_else(|| ApiError::unauthorized("x-bench-user header required"))?;

    let t = Instant::now();
    let exists = Like::objects()
        .filter(like::USER.eq(user.id) & like::POST.eq(id))
        .exists()
        .await?;
    if !exists {
        Like::objects()
            .create(Like {
                id: 0,
                user: ForeignKey::new(user.id),
                post: ForeignKey::new(id),
                created_at: Utc::now(),
            })
            .await?;
    }
    trace.add_db_wall(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Json(json!({ "ok": true, "created": !exists })))
}

pub async fn follow_user(
    Path(id): Path<i64>,
    Extension(user): Extension<Option<BenchUser>>,
    Extension(trace): Extension<Trace>,
) -> Result<Json<Value>, ApiError> {
    trace.handler_start();
    let user = user.ok_or_else(|| ApiError::unauthorized("x-bench-user header required"))?;
    if user.id == id {
        return Err(ApiError::bad_request("cannot follow yourself"));
    }

    let t = Instant::now();
    let exists = Follow::objects()
        .filter(follow::FOLLOWER.eq(user.id) & follow::FOLLOWING.eq(id))
        .exists()
        .await?;
    if !exists {
        Follow::objects()
            .create(Follow {
                id: 0,
                follower: ForeignKey::new(user.id),
                following: ForeignKey::new(id),
                created_at: Utc::now(),
            })
            .await?;
    }
    trace.add_db_wall(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Json(json!({ "ok": true, "created": !exists })))
}

// ---------------------------------------------------------------------------
// Baseline + bench control surface
// ---------------------------------------------------------------------------

pub async fn ping(Extension(trace): Extension<Trace>) -> Json<Value> {
    trace.handler_start();
    let t = Instant::now();
    let payload = json!({ "ok": true, "svc": "umbral-bench" });
    trace.add_serialize(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Json(payload)
}

pub async fn telemetry_snapshot() -> Json<Value> {
    Json(telemetry::snapshot())
}

pub async fn middleware_inventory() -> Json<Value> {
    Json(telemetry::middleware_inventory())
}

pub async fn reset_telemetry() -> Json<Value> {
    telemetry::reset_aggregates();
    Json(json!({ "ok": true }))
}

/// The layer profile of ONE run (requests tagged with its `x-bench-run`).
pub async fn run_telemetry(Path(id): Path<u64>) -> Result<Json<Value>, ApiError> {
    if let Some(snap) = telemetry::per_run_snapshot(id) {
        return Ok(Json(snap));
    }
    if loadgen::get_run(id).is_none() {
        return Err(ApiError::not_found(format!("run {id} not found")));
    }
    // Known run, but recorded before per-run capture (or zero requests) —
    // say so instead of inventing numbers from the global window.
    Ok(Json(json!({ "captured": false, "run": id })))
}

pub async fn lab_get() -> Json<Value> {
    let (fake_middleware_us, rate_limit_per_sec) = crate::lab::lab_settings();
    Json(json!({
        "fake_middleware_us": fake_middleware_us,
        "rate_limit_per_sec": rate_limit_per_sec,
    }))
}

#[derive(Debug, Deserialize)]
pub struct LabSettings {
    pub fake_middleware_us: Option<u64>,
    pub rate_limit_per_sec: Option<u64>,
}

pub async fn lab_set(Json(input): Json<LabSettings>) -> Json<Value> {
    crate::lab::set_lab_settings(input.fake_middleware_us, input.rate_limit_per_sec);
    let (fake_middleware_us, rate_limit_per_sec) = crate::lab::lab_settings();
    Json(json!({
        "ok": true,
        "fake_middleware_us": fake_middleware_us,
        "rate_limit_per_sec": rate_limit_per_sec,
    }))
}

#[derive(Debug, Deserialize)]
pub struct StartRun {
    pub scenario: String,
    pub vus: u32,
    #[serde(default = "default_duration")]
    pub duration_s: u32,
}

fn default_duration() -> u32 {
    30
}

pub async fn start_run(Json(input): Json<StartRun>) -> Result<Json<Value>, ApiError> {
    let record = loadgen::start_run(&input.scenario, input.vus, input.duration_s)
        .await
        .map_err(ApiError::bad_request)?;
    Ok(Json(json!(record)))
}

pub async fn list_runs() -> Json<Value> {
    let runs: Vec<Value> = loadgen::list_runs()
        .iter()
        .map(|r| {
            json!({
                "id": r.id, "scenario": r.scenario, "vus": r.vus,
                "duration_s": r.duration_s, "backend": r.backend,
                "status": r.status, "started_at": r.started_at,
                "summary": r.summary,
            })
        })
        .collect();
    Json(json!({ "runs": runs }))
}

pub async fn get_run(Path(id): Path<u64>) -> Result<Json<Value>, ApiError> {
    let run = loadgen::get_run(id)
        .ok_or_else(|| ApiError::not_found(format!("run {id} not found")))?;
    Ok(Json(json!(run)))
}

// ---------------------------------------------------------------------------
// SSE — live run samples
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    run: Option<u64>,
}

pub async fn stream(
    Query(q): Query<StreamQuery>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = loadgen::subscribe();
    let want = q.run;
    let stream = BroadcastStream::new(rx).filter_map(move |item| {
        let ev = item.ok()?;
        if let Some(want) = want {
            if ev.run_id != want {
                return None;
            }
        }
        let data = serde_json::to_string(&ev).ok()?;
        Some(Ok(Event::default().event("sample").data(data)))
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10)))
}
