//! Auth-gated account views. Three pages:
//!
//!   - `dashboard` — login_required; greets the logged-in user.
//!   - `staff_only` — permission_required (ecommerce.view_product).
//!   - `me` — login_required; exercises the cross-crate
//!     reverse-OneToOne accessor so visiting `/me` proves
//!     `auth_user.customer()` works without modifying AuthUser
//!     (a cross-crate type from umbral-auth).
//!
//! The `CustomerUserOneToOneReverse` trait is imported here
//! purely to bring the `.customer()` method into scope.

use ecommerce::models::{Customer, CustomerUserOneToOneReverse, Order, Product};
use umbral::templates::context;
use umbral::web::{ApiError, Html};
use umbral_auth::{AuthUser, LoggedIn, UserModel};


pub async fn dashboard(user: LoggedIn<AuthUser>) -> Result<Html<String>, ApiError> {
    let username = user.0.username().to_string();

    let product_count = Product::objects().count().await?;
    let order_count = Order::objects().count().await?;
    let customer_count = Customer::objects().count().await?;

    let body = umbral::templates::render(
        "dashboard.html",
        &context!(username, product_count, order_count, customer_count),
    )?;
    Ok(Html(body))
}

pub async fn staff_only() -> Result<Html<String>, ApiError> {
    let body = umbral::templates::render("staff_only.html", &context!())?;
    Ok(Html(body))
}

/// Exercises the cross-crate reverse-OneToOne accessor end-to-end.
/// `user.customer().await?` is the trait method emitted by
/// `#[derive(Model)]` on Customer because of its
/// `OneToOne<AuthUser>` field — AuthUser lives in `umbral-auth`,
/// Customer lives in `examples/shop/plugins/ecommerce`, the
/// accessor still resolves thanks to the trait-on-foreign-type
/// emission.
pub async fn me(user: LoggedIn<AuthUser>) -> Result<Html<String>, ApiError> {
    let username = user.0.username().to_string();
    let user_id = user.0.id;

    let customer = user.0.customer().await?;

    let has_customer = customer.is_some();
    let customer_id = customer.as_ref().map(|c| c.id);
    let loyalty_points = customer.as_ref().map(|c| c.loyalty_points).unwrap_or(0);

    let body = umbral::templates::render(
        "me.html",
        &context!(username, user_id, has_customer, customer_id, loyalty_points),
    )?;
    Ok(Html(body))
}
