//! Thin handlers for `bench-prod`.
//!
//! Identical work to `crate::api`'s handlers (both delegate to
//! `crate::domain`), with two differences: no `trace.*` calls, and the acting
//! user comes from the REAL session rather than an `x-bench-user` header.
//!
//! `crate::views` cannot be mounted here — its three page handlers take
//! `Extension<Trace>`, and a missing axum extension is a runtime 500 on every
//! HTML route, not a compile error. Each page's data-gathering lives in
//! `crate::domain`; the page handlers below are prod's own thin render
//! wrappers, mirroring `views.rs`'s context construction key for key.

use serde::Deserialize;
use serde_json::{Value, json};

use umbral::templates::context;
use umbral::web::{ApiError, Html, Json, Path, Query};
use umbral_auth::OptionalIdentity;

use crate::domain;

#[derive(Debug, Deserialize)]
pub struct FeedQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePost {
    pub body: String,
}

// ---------------------------------------------------------------------------
// HTML pages
// ---------------------------------------------------------------------------

pub async fn feed_page(Query(q): Query<FeedQuery>) -> Result<Html<String>, ApiError> {
    let limit = domain::feed_limit(q.limit);

    let data = domain::feed_page_data(limit).await?;
    let rows = domain::feed_page_payload(&data);

    let body = umbral::templates::render("feed.html", &context!(posts => rows))?;
    Ok(Html(body))
}

pub async fn profile_page(Path(id): Path<i64>) -> Result<Html<String>, ApiError> {
    let data = domain::profile_page_data(id).await?;
    let vars = domain::profile_page_payload(&data);

    let body = umbral::templates::render(
        "profile.html",
        &context!(
            username => vars.username,
            user_id => vars.user_id,
            followers => vars.followers,
            following => vars.following,
            post_count => vars.post_count,
            posts => vars.posts
        ),
    )?;
    Ok(Html(body))
}

pub async fn post_page(Path(id): Path<i64>) -> Result<Html<String>, ApiError> {
    let data = domain::post_page_data(id).await?;
    let vars = domain::post_page_payload(&data);

    let body = umbral::templates::render(
        "post.html",
        &context!(
            post_id => vars.post_id,
            author_id => vars.author_id,
            author => vars.author,
            body => vars.body,
            created_at => vars.created_at,
            likes => vars.likes,
            comments => vars.comments
        ),
    )?;
    Ok(Html(body))
}

// ---------------------------------------------------------------------------
// JSON API
// ---------------------------------------------------------------------------

pub async fn ping() -> Json<Value> {
    Json(json!({ "ok": true }))
}

pub async fn feed(Query(q): Query<FeedQuery>) -> Result<Json<Value>, ApiError> {
    let limit = domain::feed_limit(q.limit);

    let posts = domain::recent_posts(limit).await?;
    let ids: Vec<i64> = posts.iter().map(|p| p.id).collect();
    let (likes, comments) = domain::engage_rows(&ids).await?;

    Ok(Json(domain::feed_payload(&posts, &likes, &comments)))
}

pub async fn feed_naive(Query(q): Query<FeedQuery>) -> Result<Json<Value>, ApiError> {
    let limit = domain::feed_limit(q.limit);
    Ok(Json(domain::naive_feed_payload(
        &domain::naive_feed_query(limit).await?,
    )))
}

pub async fn profile(Path(id): Path<i64>) -> Result<Json<Value>, ApiError> {
    Ok(Json(domain::profile_payload(
        &domain::profile_query(id).await?,
    )))
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// The acting user's id, or the error that says why there isn't one.
///
/// `OptionalIdentity` is a tuple struct over `Option<Identity>`, and
/// `Identity::pk` is the framework's typed accessor for the stringified key
/// (its own docs say to use `pk`, not `.parse()`). The 400 for a pk that will
/// not convert mirrors the framework's `RequireAuth` / `RequireStaff`
/// extractors, which 400 with `bad_identity` on the same impossible-in-a-
/// correct-app branch — this app's user pks are `i64`, so it cannot fire.
fn acting_user(user: OptionalIdentity) -> Result<i64, ApiError> {
    let id = user
        .0
        .ok_or_else(|| ApiError::unauthorized("authentication required"))?;
    id.pk::<i64>()
        .map_err(|e| ApiError::bad_request(e.to_string()))
}

pub async fn create_post(
    user: OptionalIdentity,
    Json(input): Json<CreatePost>,
) -> Result<Json<Value>, ApiError> {
    let user_id = acting_user(user)?;
    let body = domain::clamp_body(&input.body);
    Ok(Json(domain::create_post(user_id, &body).await?))
}

pub async fn like_post(
    user: OptionalIdentity,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let user_id = acting_user(user)?;
    Ok(Json(domain::like_post(user_id, id).await?))
}

pub async fn follow_user(
    user: OptionalIdentity,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let user_id = acting_user(user)?;
    // The guard lives here, not in `domain` — same as `crate::api`, and for
    // the same reason: it must stay before any measurement window so a 400
    // never carries a stamped interval.
    if user_id == id {
        return Err(ApiError::bad_request("cannot follow yourself"));
    }
    Ok(Json(domain::follow_user(user_id, id).await?))
}
