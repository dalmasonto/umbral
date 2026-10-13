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
//!
//! These handlers are thin: the queries and payload assembly live in
//! [`crate::domain`], so `bench-prod` runs the same work under real auth.
//! What stays here is `bench-instrumented`'s identity (`x-bench-user`) and
//! every `trace.*` stamp.

use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::extract::Extension;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use umbral::web::{ApiError, Json, Path, Query};

use crate::domain;
use crate::probes::BenchUser;
use crate::telemetry::Trace;
use crate::{loadgen, telemetry};

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
    let posts = domain::recent_posts(limit).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();

    let t = Instant::now();
    let (like_counts, comment_counts) = domain::engage_counts(&ids).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let t = Instant::now();
    let payload = domain::feed_payload(&posts, &like_counts, &comment_counts);
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

    // The walk is one `domain` call (initial read, the per-post author fetch
    // and two COUNTs, row assembly), so it is one wall window rather than a
    // window per post.
    let t = Instant::now();
    let payload = domain::naive_feed_payload(limit).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);
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

    // Queries and payload assembly are one `domain` call, so one wall window.
    let t = Instant::now();
    let payload = domain::profile_payload(id).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);
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
    let body = domain::clamp_body(&input.body);

    let t = Instant::now();
    let payload = domain::create_post(user.id, &body).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Json(payload))
}

pub async fn like_post(
    Path(id): Path<i64>,
    Extension(user): Extension<Option<BenchUser>>,
    Extension(trace): Extension<Trace>,
) -> Result<Json<Value>, ApiError> {
    trace.handler_start();
    let user = user.ok_or_else(|| ApiError::unauthorized("x-bench-user header required"))?;

    let t = Instant::now();
    let payload = domain::like_post(user.id, id).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Json(payload))
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
    let payload = domain::follow_user(user.id, id).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Json(payload))
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
    let run =
        loadgen::get_run(id).ok_or_else(|| ApiError::not_found(format!("run {id} not found")))?;
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
