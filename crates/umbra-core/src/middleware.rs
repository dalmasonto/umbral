//! A framework-level request/response middleware contract (feature #68).
//!
//! axum/tower already give you `Layer` + `Service`, but writing one
//! correctly means understanding poll-readiness, `BoxFuture`, and the
//! `Service` trait's ownership rules. Most application middleware only
//! wants two things: *look at the request before the handler*, and
//! *look at the response after*. The [`Middleware`] trait is that
//! narrow, ergonomic surface — Django's `process_request` /
//! `process_response`, typed for Rust.
//!
//! Plugins contribute middleware via `Plugin::middleware`; an app adds
//! its own via `AppBuilder::middleware`. `App::build` collects them all
//! into one [`MiddlewareStack`] and installs it as a single axum layer.
//!
//! ## Composition (the onion)
//!
//! `before_request` hooks run in registration order; `after_response`
//! hooks run in the *reverse* order, so each middleware wraps the ones
//! registered after it — the standard onion model that makes
//! composition predictable. A `before_request` may short-circuit by
//! returning `Err(response)`: the handler and every later middleware are
//! skipped, and only the `after_response` hooks of the middleware that
//! already ran (in reverse) get to see the short-circuit response.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

/// A typed request/response middleware. Implement either hook (or both);
/// the defaults pass through untouched.
///
/// ```ignore
/// use umbra::prelude::*;
/// use axum::extract::Request;
/// use axum::response::Response;
///
/// struct RequestId;
///
/// #[umbra::async_trait]
/// impl Middleware for RequestId {
///     async fn before_request(&self, mut req: Request) -> Result<Request, Response> {
///         req.headers_mut().insert("x-request-id", new_id().parse().unwrap());
///         Ok(req)
///     }
/// }
/// ```
#[async_trait]
pub trait Middleware: Send + Sync + 'static {
    /// A short label for diagnostics. Defaults to the type name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Inspect or modify the request before it reaches the handler.
    ///
    /// Return `Ok(req)` to continue (with the possibly-modified request),
    /// or `Err(response)` to short-circuit: the handler and all later
    /// middleware are skipped, and the response unwinds back out through
    /// the `after_response` hooks of the middleware that already ran.
    ///
    /// Default: pass the request through unchanged.
    async fn before_request(&self, req: Request) -> Result<Request, Response> {
        Ok(req)
    }

    /// Inspect or modify the response on the way out.
    ///
    /// Default: pass the response through unchanged.
    async fn after_response(&self, res: Response) -> Response {
        res
    }
}

/// An ordered set of [`Middleware`], collected from the app builder and
/// every plugin, installed as one axum layer by `App::build`.
#[derive(Clone, Default)]
pub struct MiddlewareStack {
    middleware: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareStack {
    /// An empty stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one middleware to the end of the stack. Its `before_request`
    /// runs after every middleware already in the stack; its
    /// `after_response` runs before them (onion order).
    pub fn push(&mut self, mw: Arc<dyn Middleware>) {
        self.middleware.push(mw);
    }

    /// Append every middleware from `other`, preserving order.
    pub fn extend(&mut self, other: impl IntoIterator<Item = Arc<dyn Middleware>>) {
        self.middleware.extend(other);
    }

    /// True when no middleware is registered — `App::build` skips
    /// installing the layer entirely in that case.
    pub fn is_empty(&self) -> bool {
        self.middleware.is_empty()
    }

    /// Number of registered middleware.
    pub fn len(&self) -> usize {
        self.middleware.len()
    }

    /// Wrap `router` with this stack as a single axum middleware layer.
    /// A no-op (returns the router unchanged) when the stack is empty.
    pub fn apply(self, router: axum::Router) -> axum::Router {
        if self.middleware.is_empty() {
            return router;
        }
        let state = Arc::new(self.middleware);
        router.layer(axum::middleware::from_fn_with_state(state, run_stack))
    }
}

/// The axum middleware fn that drives one [`MiddlewareStack`] per request:
/// run the `before_request` hooks in order (short-circuiting on the first
/// `Err`), invoke the handler, then run the `after_response` hooks of the
/// middleware that ran, in reverse.
async fn run_stack(
    State(stack): State<Arc<Vec<Arc<dyn Middleware>>>>,
    req: Request,
    next: Next,
) -> Response {
    // `Option` so the request can be moved into each `before_request` and
    // handed back, without the borrow checker tripping on the short-
    // circuit (`Err`) path where it isn't returned.
    let mut req_opt = Some(req);
    let mut ran = 0usize;
    let mut short_circuit: Option<Response> = None;

    for mw in stack.iter() {
        let req = req_opt
            .take()
            .expect("request present for each before hook");
        match mw.before_request(req).await {
            Ok(modified) => {
                req_opt = Some(modified);
                ran += 1;
            }
            Err(resp) => {
                short_circuit = Some(resp);
                break;
            }
        }
    }

    let mut res = match short_circuit {
        Some(resp) => resp,
        None => {
            next.run(
                req_opt
                    .take()
                    .expect("request present when not short-circuited"),
            )
            .await
        }
    };

    // Only the middleware whose `before_request` ran get an
    // `after_response`, in reverse (onion unwind).
    for mw in stack.iter().take(ran).rev() {
        res = mw.after_response(res).await;
    }
    res
}
