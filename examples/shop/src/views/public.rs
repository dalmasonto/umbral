//! Public storefront views - anyone can hit these, no auth.
//!
//! Storefront pages for catalog and content plugin records. Every
//! handler hands a minijinja context to `umbra::templates::render`
//! and wraps DB / template errors with `internal_error`.

use chrono::Utc;
use content::models::{ContactMessage, ContactStatus, Faq, Post, faq, post};
use ecommerce::models::{Brand, Product, Review, brand, product, review};
use serde::{Deserialize, Serialize};
use umbra::templates::context;
use umbra::web::{Form, Html, IntoResponse, Path, Query, Redirect, Response, StatusCode};

use super::internal_error;

#[derive(Debug, Deserialize)]
pub struct ContactQuery {
    sent: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct ContactForm {
    name: String,
    email: String,
    phone: String,
    subject: String,
    message: String,
}

#[derive(Debug, Serialize, Default)]
struct ContactErrors {
    form: Option<String>,
    name: Option<String>,
    email: Option<String>,
    subject: Option<String>,
    message: Option<String>,
}

impl ContactErrors {
    fn has_any(&self) -> bool {
        self.form.is_some()
            || self.name.is_some()
            || self.email.is_some()
            || self.subject.is_some()
            || self.message.is_some()
    }
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
        &ContactErrors::default(),
        StatusCode::OK,
    )
}

pub async fn submit_contact(
    Form(form): Form<ContactForm>,
) -> Result<Response, (StatusCode, String)> {
    let now = Utc::now();
    let form = normalize_contact_form(form);
    let errors = validate_contact_form(&form);

    if errors.has_any() {
        return render_contact_page(false, &form, &errors, StatusCode::UNPROCESSABLE_ENTITY);
    }

    let phone = if form.phone.is_empty() {
        None
    } else {
        Some(form.phone.clone())
    };

    ContactMessage::objects()
        .create(ContactMessage {
            id: 0,
            name: form.name.clone(),
            email: form.email.clone(),
            phone,
            subject: form.subject.clone(),
            message: form.message.clone(),
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
    errors: &ContactErrors,
    status: StatusCode,
) -> Result<Response, (StatusCode, String)> {
    let body = umbra::templates::render("contact.html", &context!(sent, form, errors))
        .map_err(internal_error)?;
    Ok((status, Html(body)).into_response())
}

fn normalize_contact_form(mut form: ContactForm) -> ContactForm {
    form.name = form.name.trim().to_string();
    form.email = form.email.trim().to_lowercase();
    form.phone = form.phone.trim().to_string();
    form.subject = form.subject.trim().to_string();
    form.message = form.message.trim().to_string();
    form
}

fn validate_contact_form(form: &ContactForm) -> ContactErrors {
    let mut errors = ContactErrors::default();

    if form.name.is_empty() {
        errors.name = Some("Enter your name.".to_string());
    }

    if form.email.is_empty() {
        errors.email = Some("Enter your email address.".to_string());
    } else if !looks_like_email(&form.email) {
        errors.email = Some("Enter a valid email address.".to_string());
    }

    if form.subject.is_empty() {
        errors.subject = Some("Add a subject.".to_string());
    }

    if form.message.is_empty() {
        errors.message = Some("Write a message.".to_string());
    }

    if errors.has_any() {
        errors.form = Some("Please fix the highlighted fields and send again.".to_string());
    }

    errors
}

fn looks_like_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };

    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || email.chars().any(char::is_whitespace)
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return false;
    }

    domain.split('.').all(|part| !part.is_empty()) && domain.contains('.')
}
