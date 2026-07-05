//! `Routes::require_permission` — the ergonomic, audit-visible way to gate a
//! route on a permission (audit_2 H19).
//!
//! A route gated by hand with `.layer(permission_required("perm"))` is enforced
//! correctly, but the framework can't SEE the gate: `RouteSpec` (path + methods)
//! has no idea a tower layer is attached, so the boot audit of ungated mutating
//! routes would flag it as a false positive. These builders do both halves in
//! one call: they apply the `permission_required` layer for enforcement AND
//! record the permission in the `RouteSpec` (via core's `Routes::route_gated`)
//! so the boot audit sees the route as gated.
//!
//! ```ignore
//! use umbral::routes::Routes;
//! use umbral_permissions::RoutesPermExt;
//!
//! let routes = Routes::new()
//!     .get("/", home)                                       // public
//!     .post_gated("/posts", create_post, "blog.add_post")   // enforced + recorded
//!     .delete_gated("/posts/{id}", delete_post, "blog.delete_post");
//! ```

use axum::handler::Handler;
use axum::routing::{delete, get, patch, post, put};
use umbral::routes::Routes;

use crate::permission_required;

/// Extension trait adding permission-gated route builders to [`Routes`]. Each
/// method registers the route with `permission_required(perm)` applied AND the
/// permission recorded in the `RouteSpec`, so the audit_2 H19 boot audit can
/// tell the route is gated.
pub trait RoutesPermExt {
    /// `GET` gated on `perm`.
    fn get_gated<H, T>(self, path: &str, handler: H, perm: &str) -> Self
    where
        H: Handler<T, ()>,
        T: 'static;
    /// `POST` gated on `perm`.
    fn post_gated<H, T>(self, path: &str, handler: H, perm: &str) -> Self
    where
        H: Handler<T, ()>,
        T: 'static;
    /// `PUT` gated on `perm`.
    fn put_gated<H, T>(self, path: &str, handler: H, perm: &str) -> Self
    where
        H: Handler<T, ()>,
        T: 'static;
    /// `PATCH` gated on `perm`.
    fn patch_gated<H, T>(self, path: &str, handler: H, perm: &str) -> Self
    where
        H: Handler<T, ()>,
        T: 'static;
    /// `DELETE` gated on `perm`.
    fn delete_gated<H, T>(self, path: &str, handler: H, perm: &str) -> Self
    where
        H: Handler<T, ()>,
        T: 'static;
}

impl RoutesPermExt for Routes {
    fn get_gated<H, T>(self, path: &str, handler: H, perm: &str) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.route_gated(
            &["GET"],
            path,
            get(handler).layer(permission_required(perm)),
            perm,
        )
    }

    fn post_gated<H, T>(self, path: &str, handler: H, perm: &str) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.route_gated(
            &["POST"],
            path,
            post(handler).layer(permission_required(perm)),
            perm,
        )
    }

    fn put_gated<H, T>(self, path: &str, handler: H, perm: &str) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.route_gated(
            &["PUT"],
            path,
            put(handler).layer(permission_required(perm)),
            perm,
        )
    }

    fn patch_gated<H, T>(self, path: &str, handler: H, perm: &str) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.route_gated(
            &["PATCH"],
            path,
            patch(handler).layer(permission_required(perm)),
            perm,
        )
    }

    fn delete_gated<H, T>(self, path: &str, handler: H, perm: &str) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.route_gated(
            &["DELETE"],
            path,
            delete(handler).layer(permission_required(perm)),
            perm,
        )
    }
}
