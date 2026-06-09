//! Auth-gated account views. Three pages:
//!
//!   - `dashboard` — login_required; greets the logged-in user.
//!   - `staff_only` — permission_required (ecommerce.view_product).
//!   - `me` — login_required; exercises the cross-crate
//!     reverse-OneToOne accessor so visiting `/me` proves
//!     `auth_user.customer()` works without modifying AuthUser
//!     (a cross-crate type from umbra-auth).
//!
//! The `CustomerUserOneToOneReverse` trait is imported here
//! purely to bring the `.customer()` method into scope.

use ecommerce::models::CustomerUserOneToOneReverse;
use umbra::templates::context;
use umbra::web::{Html, StatusCode};
use umbra_auth::{AuthUser, LoggedIn, UserModel};

use super::internal_error;

pub async fn dashboard(user: LoggedIn<AuthUser>) -> Result<Html<String>, (StatusCode, String)> {
    let username = user.0.username().to_string();
    let body =
        umbra::templates::render("dashboard.html", &context!(username)).map_err(internal_error)?;
    Ok(Html(body))
}

pub async fn staff_only() -> Result<Html<String>, (StatusCode, String)> {
    let body = umbra::templates::render("staff_only.html", &context!()).map_err(internal_error)?;
    Ok(Html(body))
}

/// Exercises the cross-crate reverse-OneToOne accessor end-to-end.
/// `user.customer().await?` is the trait method emitted by
/// `#[derive(Model)]` on Customer because of its
/// `OneToOne<AuthUser>` field — AuthUser lives in `umbra-auth`,
/// Customer lives in `examples/shop/plugins/ecommerce`, the
/// accessor still resolves thanks to the trait-on-foreign-type
/// emission.
pub async fn me(user: LoggedIn<AuthUser>) -> Result<Html<String>, (StatusCode, String)> {
    let username = user.0.username().to_string();
    let user_id = user.0.id;

    let customer = user.0.customer().await.map_err(internal_error)?;

    let body = match customer {
        Some(c) => format!(
            "<!doctype html><h1>{username}</h1>\
             <p><strong>AuthUser id</strong>: {user_id} (umbra-auth crate)</p>\
             <p><strong>Customer id</strong>: {} (ecommerce crate)</p>\
             <p><strong>Loyalty points</strong>: {}</p>\
             <p>Loaded via <code>user.customer().await?</code> — the cross-crate \
             reverse-O2O accessor.</p>\
             <p><a href='/admin'>Admin</a></p>",
            c.id, c.loyalty_points,
        ),
        None => format!(
            "<!doctype html><h1>{username}</h1>\
             <p>AuthUser id {user_id} has no Customer row — \
             <code>user.customer().await?</code> returned <code>None</code>.</p>\
             <p><a href='/admin'>Admin</a></p>"
        ),
    };
    Ok(Html(body))
}
