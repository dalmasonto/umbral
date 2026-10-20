//! Server-rendered HTML pages — the template-rendering arm of the harness.
//!
//! The same data the JSON API serves, rendered through minijinja so the
//! `render` bucket in the layer ledger has a real workload: the feed and
//! profile pages both fetch, assemble a context, and render a template.
//!
//! The fetching and the context assembly live in [`crate::domain`] — the pages
//! must be mountable by `bench-prod`, which constructs no telemetry and so
//! cannot provide the `Trace` extension. What stays here is the `trace.*`
//! stamping and the render.

use std::time::Instant;

use axum::extract::Extension;

use umbral::templates::context;
use umbral::web::{ApiError, Html, Path, Query};

use crate::api::FeedQuery;
use crate::domain;
use crate::telemetry::Trace;

pub async fn feed_page(
    Query(q): Query<FeedQuery>,
    Extension(trace): Extension<Trace>,
) -> Result<Html<String>, ApiError> {
    trace.handler_start();
    let limit = domain::feed_limit(q.limit);

    let t = Instant::now();
    let data = domain::feed_page_data(limit).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let rows = domain::feed_page_payload(&data);

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
    let data = domain::profile_page_data(id).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let vars = domain::profile_page_payload(&data);

    let t = Instant::now();
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
    let data = domain::post_page_data(id).await?;
    trace.add_db_wall(t.elapsed().as_micros() as u64);

    let vars = domain::post_page_payload(&data);

    let t = Instant::now();
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
    trace.add_render(t.elapsed().as_micros() as u64);
    trace.handler_end();
    Ok(Html(body))
}
