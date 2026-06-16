//! Public storefront views - anyone can hit these, no auth.
//!
//! Storefront pages for catalog and content plugin records. Every
//! handler hands a minijinja context to `umbra::templates::render`
//! and wraps DB / template errors with `internal_error`.

use std::collections::HashMap;

use content::models::{ContactMessage, Faq, Note, Post, faq, note, post};
use ecommerce::models::{Brand, Product, Review, brand, product, review};
use serde::{Deserialize, Serialize};
use umbra::forms::Form;
use umbra::templates::context;
use umbra::web::{Html, IntoResponse, Json, Path, Query, Redirect, Response, StatusCode};

use super::internal_error;

#[derive(Debug, Deserialize)]
pub struct ContactQuery {
    sent: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BenchJsonResponse {
    ok: bool,
    name: &'static str,
    items: [i32; 4],
}

pub async fn bench_json() -> Json<BenchJsonResponse> {
    Json(BenchJsonResponse {
        ok: true,
        name: "shop",
        items: [1, 2, 3, 4],
    })
}

pub async fn bench_text() -> &'static str {
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\
xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\
xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\
xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\
xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\
xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\
xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\
xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
}

pub async fn bench_note_write() -> Result<Json<Note>, (StatusCode, String)> {
    let note = Note::objects()
        .create(Note {
            id: 0,
            title: "ApacheBench note".to_string(),
            description: "Inserted by the shop benchmark write endpoint.".to_string(),
        })
        .await
        .map_err(internal_error)?;

    Ok(Json(note))
}

pub async fn bench_note_read() -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let notes = Note::objects()
        .order_by(note::ID.desc())
        .limit(25)
        .fetch()
        .await
        .map_err(internal_error)?;

    let total = Note::objects().count().await.map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "total": total,
        "notes": notes,
    })))
}

// gaps2 #19 follow-up: ContactMessage now serves as BOTH the
// persisted Model AND the public form. The `#[derive(Form)]` lives
// on the Model declaration in `content::models`; this view just
// imports it and writes `Form<ContactMessage>` in the handler
// signature. No parallel ContactForm struct, no field-by-field
// duplication.

pub async fn home() -> Result<Html<String>, (StatusCode, String)> {
    let featured = Product::objects()
        .filter(product::IS_FEATURED.eq(true))
        .filter(product::STATUS.eq("active"))
        .order_by(product::CREATED_AT.desc())
        .limit(4)
        .fetch()
        .await
        .map_err(internal_error)?;

    let brands = Brand::objects()
        .order_by(brand::NAME.asc())
        .fetch()
        .await
        .map_err(internal_error)?;

    let reviews = Review::objects()
        .filter(review::IS_APPROVED.eq(true))
        .order_by(review::CREATED_AT.desc())
        .limit(3)
        .fetch()
        .await
        .map_err(internal_error)?;

    let home_faqs = Faq::objects()
        .filter(faq::IS_PUBLISHED.eq(true))
        .order_by(faq::POSITION.asc())
        .limit(3)
        .fetch()
        .await
        .map_err(internal_error)?;

    let product_count = Product::objects()
        .filter(product::STATUS.eq("active"))
        .count()
        .await
        .map_err(internal_error)?;
    let featured_count = featured.len();
    let brand_count = brands.len();

    let body = umbra::templates::render(
        "home.html",
        &context!(
            featured,
            brands,
            reviews,
            home_faqs,
            product_count,
            featured_count,
            brand_count
        ),
    )
    .map_err(internal_error)?;
    Ok(Html(body))
}

pub async fn product_list() -> Result<Html<String>, (StatusCode, String)> {
    let products = Product::objects()
        .filter(product::STATUS.eq("active"))
        .order_by(product::NAME.asc())
        .fetch()
        .await
        .map_err(internal_error)?;

    let product_count = products.len();

    let body = umbra::templates::render("product_list.html", &context!(products, product_count))
        .map_err(internal_error)?;
    Ok(Html(body))
}

pub async fn product_detail(Path(id): Path<i64>) -> Result<Html<String>, (StatusCode, String)> {
    let product = Product::objects()
        .filter(product::ID.eq(id))
        .first()
        .await
        .map_err(internal_error)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Product {id} not found")))?;

    let body = umbra::templates::render("product_detail.html", &context!(product))
        .map_err(internal_error)?;
    Ok(Html(body))
}

pub async fn post_list() -> Result<Html<String>, (StatusCode, String)> {
    let featured_posts = Post::objects()
        .filter(post::STATUS.eq("published"))
        .filter(post::IS_FEATURED.eq(true))
        .order_by(post::PUBLISHED_AT.desc())
        .limit(3)
        .fetch()
        .await
        .map_err(internal_error)?;

    let posts = Post::objects()
        .filter(post::STATUS.eq("published"))
        .order_by(post::PUBLISHED_AT.desc())
        .fetch()
        .await
        .map_err(internal_error)?;

    let post_count = posts.len();

    let body = umbra::templates::render("posts.html", &context!(posts, featured_posts, post_count))
        .map_err(internal_error)?;
    Ok(Html(body))
}

pub async fn post_detail(Path(slug): Path<String>) -> Result<Html<String>, (StatusCode, String)> {
    let post = Post::objects()
        .filter(post::SLUG.eq(slug.as_str()))
        .filter(post::STATUS.eq("published"))
        .first()
        .await
        .map_err(internal_error)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Post `{slug}` not found")))?;

    let body =
        umbra::templates::render("post_detail.html", &context!(post)).map_err(internal_error)?;
    Ok(Html(body))
}

pub async fn faqs() -> Result<Html<String>, (StatusCode, String)> {
    let faqs = Faq::objects()
        .filter(faq::IS_PUBLISHED.eq(true))
        .order_by(faq::POSITION.asc())
        .fetch()
        .await
        .map_err(internal_error)?;

    let faq_count = faqs.len();

    let body = umbra::templates::render("faqs.html", &context!(faqs, faq_count))
        .map_err(internal_error)?;
    Ok(Html(body))
}

/// `GET /contact` — render the empty form. The Django shape:
/// CSRF is ambient (`{{ csrf_input }}` in the template), `errors`
/// is simply absent, and `?sent=1` after a successful redirect
/// shows the thank-you banner.
pub async fn contact(
    Query(query): Query<ContactQuery>,
) -> Result<Html<String>, (StatusCode, String)> {
    let sent = query.sent.as_deref() == Some("1");
    let form = ContactMessage::default();
    let errors: HashMap<String, Vec<String>> = HashMap::new();
    let body = umbra::templates::render("contact.html", &context!(sent, form, errors))
        .map_err(internal_error)?;
    Ok(Html(body))
}

/// `POST /contact` — validate, save, redirect; on validation failure
/// re-render the same template with the user's input and the errors.
/// `errs.render("contact.html")` is the whole failure path: it binds
/// `form` (every keystroke kept) + `errors` (per-field + summary
/// banner) and returns 422.
pub async fn submit_contact(form: Form<ContactMessage>) -> Result<Response, (StatusCode, String)> {
    let mut msg = match form.into_result() {
        Ok(v) => v,
        Err(errs) => return Ok(errs.render("contact.html")),
    };

    // Lowercase post-validation — `normalize_strings` trims but
    // deliberately doesn't change case (it matters for usernames).
    msg.email = msg.email.to_lowercase();

    ContactMessage::objects()
        .create(msg)
        .await
        .map_err(internal_error)?;

    Ok(Redirect::to("/contact?sent=1").into_response())
}
