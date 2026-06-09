//! Shop seed orchestrator — fans out to one file per concern
//! so each step is small, focused, and easy to edit. The order
//! in `all()` matters:
//!
//! 1. `credentials` — shopadmin must exist before anything else
//!    that references user ids.
//! 2. `products`    — catalog rows the orders + reviews link to.
//! 3. `demo_data`   — extra users (alice/bob/carol → ids 2/3/4)
//!    + customers + addresses + orders. Must run BEFORE blogs
//!    because the blog comments reference user ids 2 + 3.
//! 4. `blogs`       — tags + posts + comments tied to the users
//!    created in step 3.

pub mod blogs;
pub mod credentials;
pub mod demo_data;
pub mod products;

/// Run every seed step in the right order. Each step is
/// idempotent (short-circuits on a non-empty table), so calling
/// `all()` on a partially-seeded DB tops up the missing pieces
/// without re-inserting.
pub async fn all() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    credentials::test_credentials().await?;
    products::products().await?;
    demo_data::demo_data().await?;
    blogs::blogs().await?;
    Ok(())
}
