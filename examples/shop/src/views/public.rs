//! Public storefront views — anyone can hit these, no auth.
//!
//! Three pages: home (featured products + brand grid), product
//! list (the full active catalog), product detail (one row by
//! id, 404 on miss). Every handler hands a minijinja context
//! to `umbra::templates::render` and wraps DB / template errors
//! with `internal_error`.

use ecommerce::models::{Brand, Product, brand, product};
use umbra::templates::context;
use umbra::web::{Html, Path, StatusCode};

use super::internal_error;

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

    let body = umbra::templates::render("home.html", &context!(featured, brands))
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

    let body = umbra::templates::render("product_list.html", &context!(products))
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
