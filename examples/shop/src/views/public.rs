//! Public storefront views - anyone can hit these, no auth.
//!
//! Storefront pages for catalog and content plugin records. Every
//! handler hands a minijinja context to `umbra::templates::render`
//! and wraps DB / template errors with `internal_error`.

use chrono::Utc;
use content::models::{ContactMessage, ContactStatus, Faq, Post, faq, post};
use ecommerce::models::{Brand, Product, Review, brand, product, review};
use serde::{Deserialize, Serialize};
use umbra::forms::{Form, FormErrors};
use umbra::templates::context;
use umbra::web::{Html, IntoResponse, Path, Query, Redirect, Response, StatusCode};

use super::internal_error;

#[derive(Debug, Deserialize)]
pub struct ContactQuery {
    sent: Option<String>,
}

/// gaps2 #19: the contact form's validation is now declared via
/// `#[derive(Form)]` attributes. The ~80 lines of bespoke
/// `ContactErrors` / `validate_contact_form` / `normalize_contact_form`
/// that used to live here are gone — the `Form<ContactForm>`
/// extractor runs validation before the handler, the result lands
/// in a `Result<ContactForm, FormErrors>`, and the template reads
/// `errors.<field>` straight off `FormErrors::as_template_ctx`.
#[derive(Debug, Deserialize, Serialize, Default, umbra::forms::Form)]
#[serde(default)]
#[form(normalize_strings)]
pub struct ContactForm {
    #[form(required, length(min = 1, max = 100))]
    name: String,

    #[form(required, email, max_length = 254)]
    email: String,

    #[form(optional, max_length = 30)]
    phone: String,

    #[form(required, length(min = 1, max = 200))]
    subject: String,

    #[form(required, length(min = 10, max = 5000))]
    message: String,
}

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

pub async fn contact(Query(query): Query<ContactQuery>) -> Result<Response, (StatusCode, String)> {
    let sent = query.sent.as_deref() == Some("1");
    render_contact_page(
        sent,
        &ContactForm::default(),
        serde_json::Map::new(),
        StatusCode::OK,
    )
}

pub async fn submit_contact(form: Form<ContactForm>) -> Result<Response, (StatusCode, String)> {
    let now = Utc::now();
    // gaps2 #19: the extractor already ran validation. On Err we
    // re-render the form with the per-field error map AND the
    // user's original input (axum's deserialize step ran first, so
    // the parsed struct is still accessible via the inner Result —
    // but on failure we don't have the typed T, so re-render with
    // a default ContactForm and rely on the template's `value=""`
    // fallback). For full input-preservation across re-renders the
    // caller would extract `axum::Form` AND `Form<T>` together;
    // the v1 trade-off is to keep the surface simple.
    let valid = match form.into_result() {
        Ok(v) => v,
        Err(errs) => {
            return render_contact_page(
                false,
                &ContactForm::default(),
                ctx_with_form_summary(&errs),
                StatusCode::UNPROCESSABLE_ENTITY,
            );
        }
    };

    let phone = if valid.phone.is_empty() {
        None
    } else {
        Some(valid.phone.clone())
    };

    ContactMessage::objects()
        .create(ContactMessage {
            id: 0,
            name: valid.name.clone(),
            email: valid.email.to_lowercase(),
            phone,
            subject: valid.subject.clone(),
            message: valid.message.clone(),
            status: ContactStatus::New,
            ip_address: None,
            created_at: now,
        })
        .await
        .map_err(internal_error)?;

    Ok(Redirect::to("/contact?sent=1").into_response())
}

fn render_contact_page(
    sent: bool,
    form: &ContactForm,
    errors: serde_json::Map<String, serde_json::Value>,
    status: StatusCode,
) -> Result<Response, (StatusCode, String)> {
    let body = umbra::templates::render("contact.html", &context!(sent, form, errors))
        .map_err(internal_error)?;
    Ok((status, Html(body)).into_response())
}

/// Lift `FormErrors` into the flat template ctx the
/// `contact.html` partial expects (`errors.name`, `errors.email`,
/// ..., `errors.form`), AND add the "Please fix the highlighted
/// fields..." form-level banner the legacy hand-rolled validator
/// used to write. `FormErrors::as_template_ctx` only emits a
/// `form` key when there's a non-field error; per-field-only
/// failures need an explicit banner so the user sees ONE summary
/// at the top of the form.
fn ctx_with_form_summary(errs: &FormErrors) -> serde_json::Map<String, serde_json::Value> {
    let mut ctx = errs.as_template_ctx();
    if !ctx.contains_key("form") {
        ctx.insert(
            "form".to_string(),
            serde_json::Value::String(
                "Please fix the highlighted fields and send again.".to_string(),
            ),
        );
    }
    ctx
}
