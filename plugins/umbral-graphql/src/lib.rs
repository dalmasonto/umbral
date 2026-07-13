//! A **real** GraphQL API, derived from your models.
//!
//! ```rust,ignore
//! GraphqlPlugin::default()
//!     .expose("post")
//!     .expose("auth_user")
//!     .hide("auth_user", "email")   // exposing a model exposes EVERY column of it
//! ```
//!
//! ```graphql
//! { post(id: "1") { title author { username } comments { body } } }
//! ```
//!
//! `password_hash` needs no `hide`: it is denied in core and no builder call can bring it
//! back ([`umbral::orm::HARD_DENIED_FIELDS`]). Everything else on a model you expose is
//! public the moment you name it, so read the model before you expose it.
//!
//! # Why not convert the OpenAPI spec
//!
//! Because that produces `getPost` / `listPosts` — GraphQL in name only. Nobody adopts
//! GraphQL to make the same call with different syntax; they adopt it to traverse a graph.
//! And umbral already has the graph: `Column::fk_target` names the table a foreign key
//! points at, inverting those edges gives every reverse relation, and it is the same model
//! registry `typegen` and OpenAPI already read. See `schema.rs`.
//!
//! # Deny by default
//!
//! Nothing is exposed until you name it. That is not politeness — a GraphQL endpoint is
//! the most efficient data-exfiltration tool you can hand an attacker, because *they*
//! choose the query shape. An auto-exposed schema would put every column of every model
//! (password hashes, session tokens, internal costs) one query away, and the framework
//! would have done it on your behalf.
//!
//! A relation is only traversable when BOTH ends are exposed. Otherwise `post.author`
//! would be a side door into a model you deliberately withheld.
//!
//! # Writes are a second opt-in
//!
//! `expose` makes a model readable. `mutable` makes it writable. Two calls, because a read
//! you got wrong leaks data and a write you got wrong destroys it.
//!
//! Mutations go through the ORM's dynamic write path, so they inherit the mass-assignment
//! guard, validators, cleaners, defaults and signals — see `mutation.rs`. And because the
//! endpoint now accepts writes over a `POST` that CSRF middleware is typically told to
//! exempt, it enforces its own CSRF defence: see [`GraphqlPlugin`] and `is_csrf_safe`.

use std::sync::Arc;

use async_graphql::dynamic::Schema;
use async_graphql::http::GraphiQLSource;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse, GraphQLSubscription};
use axum::response::{Html, IntoResponse};
use futures_util::StreamExt;
use umbral::migrate::ModelMeta;
use umbral::plugin::Plugin;
use umbral::web::Router;

mod connection;
mod loader;
mod mutation;
mod schema;
mod subscription;

pub use schema::{AccessFn, Exposed};

/// Default page size for a list query when the client does not say.
pub const DEFAULT_LIMIT: u64 = 50;
/// Hard cap. The client picks the query shape, so the client picks your query cost —
/// which is exactly why they do not get to pick it without a ceiling.
pub const MAX_LIMIT: u64 = 200;

pub(crate) use schema::id_string as schema_id_string;

/// Test-only: the number of database reads this plugin has performed.
#[doc(hidden)]
pub use loader::DB_READS;

/// Test-only: build a schema from an explicit `Exposed` list.
#[doc(hidden)]
pub fn build_schema_for_tests(
    exposed: &[Exposed],
) -> Result<Schema, async_graphql::dynamic::SchemaError> {
    schema::build(exposed)
}

/// Test-only: a fresh per-request loader set.
#[doc(hidden)]
pub fn new_loaders_for_tests() -> loader::Loaders {
    loader::Loaders::new()
}

/// Test-only: publish a change event without performing a write.
#[doc(hidden)]
pub use subscription::publish_for_tests;

/// Test-only: subscribe to the ORM write signals for a table (what `on_ready` does).
#[doc(hidden)]
pub fn wire_signals_for_tests(table: &str) {
    subscription::wire_signals(table);
}

/// Test-only: the plural query-field name for a model type name.
#[doc(hidden)]
pub fn plural_for_tests(model_name: &str) -> String {
    schema::plural(model_name)
}

/// Would a browser have been *forced* to preflight this request?
///
/// # The hole mutations open
///
/// GraphQL speaks POST for reads, so every umbral app puts `/graphql` in
/// `csrf_exempt_paths` — otherwise every query 403s. That was defensible while the endpoint
/// was read-only. It stops being defensible the moment a mutation exists: a cross-site page
/// can submit a plain HTML `<form>` at your endpoint, the browser attaches the victim's
/// session cookie, and the write happens. Classic CSRF, with the token check turned off by
/// the very exemption that made queries work.
///
/// # Why a content-type check is a real defence and not a fig leaf
///
/// An HTML form can only send three content types: `application/x-www-form-urlencoded`,
/// `multipart/form-data`, `text/plain`. None of them is `application/json`. And a `fetch()`
/// with `Content-Type: application/json` is NOT a CORS "simple request", so the browser must
/// preflight it with `OPTIONS` — which an attacker's origin fails, because we never answer it
/// with permissive CORS headers.
///
/// So: require `application/json`, and the set of requests a hostile page can *forge* becomes
/// exactly the set we reject. This is the same defence Apollo Server ships as its built-in
/// CSRF prevention, for the same reason.
///
/// Note what this does NOT depend on: a token, a session, or any state. It is a property of
/// what browsers are willing to send cross-origin.
pub(crate) fn is_csrf_safe(headers: &umbral::web::HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| {
            let ct = ct.split(';').next().unwrap_or("").trim();
            ct.eq_ignore_ascii_case("application/json")
                || ct.eq_ignore_ascii_case("application/graphql+json")
        })
}

/// Reject any POST that a cross-site HTML form could have produced. See [`is_csrf_safe`].
async fn csrf_content_type_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if req.method() == axum::http::Method::POST && !is_csrf_safe(req.headers()) {
        return (
            axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "graphql requires `Content-Type: application/json`. This is a CSRF defence, not a \
             formality: `/graphql` must be CSRF-exempt for queries to work at all (GraphQL \
             reads are POSTs), so the content type is what stands between a hostile page's \
             <form> and a mutation carrying your user's session cookie. A form cannot send \
             this content type, and a cross-origin fetch that does gets preflighted.",
        )
            .into_response();
    }
    next.run(req).await
}

/// Guard a top-level query field.
pub(crate) fn guard(
    ctx: &async_graphql::dynamic::ResolverContext<'_>,
    access: Option<&AccessFn>,
    meta: &ModelMeta,
) -> async_graphql::Result<()> {
    let Some(check) = access else {
        return Ok(());
    };
    let identity = ctx.data_opt::<Option<umbral::auth::Identity>>();
    let id = identity.and_then(|o| o.as_ref());
    if check(id) {
        Ok(())
    } else {
        // Deliberately not "you are not allowed to read Post" — that confirms Post exists
        // and is exposed. The client learns nothing it did not already know.
        Err(async_graphql::Error::new(format!(
            "not authorized to read `{}`",
            meta.name
        )))
    }
}

/// Guard a mutation field.
///
/// Separate from [`guard`] because the failure has to be separate: a read denial can afford
/// to be vague, but a write denial the caller cannot distinguish from "it worked" is how
/// data silently does not get saved.
pub(crate) fn guard_write(
    ctx: &async_graphql::dynamic::ResolverContext<'_>,
    e: &Exposed,
) -> async_graphql::Result<()> {
    let Some(check) = e.writable.as_ref() else {
        // Unreachable in a well-formed schema (the field would not exist), but a mutation
        // that falls through to "allowed" because of a future refactor is not a failure mode
        // worth leaving open.
        return Err(async_graphql::Error::new("not writable"));
    };
    let identity = ctx.data_opt::<Option<umbral::auth::Identity>>();
    if check(identity.and_then(|o| o.as_ref())) {
        Ok(())
    } else {
        Err(async_graphql::Error::new(format!(
            "not authorized to write `{}`",
            mutation::meta_name(&e.meta)
        )))
    }
}

/// Mounts a GraphQL endpoint (and GraphiQL) over the models you expose.
#[derive(Default, Clone)]
pub struct GraphqlPlugin {
    path: Option<String>,
    exposed: Vec<(String, Option<AccessFn>)>,
    writable: Vec<(String, AccessFn)>,
    subscribable: Vec<String>,
    hidden: Vec<(String, String)>,
    graphiql: Option<bool>,
    auth: Option<Arc<dyn umbral::auth::Authentication>>,
}

/// Accepts `"cost"`, `["cost", "supplier"]`, or a `Vec<String>` — the same shape
/// `RestPlugin::hide` takes, so the two plugins read alike.
pub trait HideFields {
    fn into_fields(self) -> Vec<String>;
}

impl HideFields for &str {
    fn into_fields(self) -> Vec<String> {
        vec![self.to_string()]
    }
}

impl HideFields for String {
    fn into_fields(self) -> Vec<String> {
        vec![self]
    }
}

impl<T: Into<String>, const N: usize> HideFields for [T; N] {
    fn into_fields(self) -> Vec<String> {
        self.into_iter().map(Into::into).collect()
    }
}

impl<T: Into<String>> HideFields for Vec<T> {
    fn into_fields(self) -> Vec<String> {
        self.into_iter().map(Into::into).collect()
    }
}

impl GraphqlPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount at a different path. Default `/graphql`.
    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Expose a model, readable by anyone.
    ///
    /// Nothing is exposed until you call this. Take the same care you would with
    /// `RestPlugin::resource` — more, in fact: a REST endpoint returns the shape you
    /// designed, and a GraphQL endpoint returns the shape the *caller* designed.
    pub fn expose(mut self, table: impl Into<String>) -> Self {
        self.exposed.push((table.into(), None));
        self
    }

    /// Expose a model only to callers your closure approves.
    ///
    /// ```rust,ignore
    /// .expose_if("order", |id| id.is_some_and(|i| i.is_staff))
    /// ```
    pub fn expose_if(
        mut self,
        table: impl Into<String>,
        access: impl Fn(Option<&umbral::auth::Identity>) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.exposed.push((table.into(), Some(Arc::new(access))));
        self
    }

    /// Make a model **writable**: `createProduct`, `updateProduct`, `deleteProduct`.
    ///
    /// A second opt-in, on top of `expose`. A read you got wrong leaks data; a write you got
    /// wrong destroys it, so exposing a model does not make it writable — you say so again.
    ///
    /// ```rust,ignore
    /// .expose("review").mutable("review")     // anyone may post a review
    /// ```
    ///
    /// The model must also be exposed: a mutation returns the row it wrote, and returning a
    /// type that is not in the schema is not a thing that can be done.
    ///
    /// Writes go through the ORM's dynamic write path, so they inherit the mass-assignment
    /// guard (`#[umbral(privileged)]` columns cannot be set by a client that did not
    /// authorize them), validators, cleaners, defaults and signals.
    pub fn mutable(mut self, table: impl Into<String>) -> Self {
        self.writable.push((table.into(), Arc::new(|_| true)));
        self
    }

    /// Make a model writable only by callers your closure approves.
    ///
    /// ```rust,ignore
    /// .mutable_if("product", |id| id.is_some_and(|i| i.is_staff))
    /// ```
    ///
    /// Needs [`Self::authenticate`], or every caller is anonymous and the gate can only deny.
    pub fn mutable_if(
        mut self,
        table: impl Into<String>,
        access: impl Fn(Option<&umbral::auth::Identity>) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.writable.push((table.into(), Arc::new(access)));
        self
    }

    /// Stream live changes to this model over WebSocket or SSE.
    ///
    /// ```graphql
    /// subscription { productChanged(id: "1") { name price } }
    /// subscription { productDeleted }
    /// ```
    ///
    /// Events come from the ORM's `post_save` / `post_delete` signals, so a row changed by
    /// ANY write path — a mutation, a REST call, an admin form, a background task — reaches
    /// subscribers. No write path can forget to publish, because publishing was never its job.
    ///
    /// The row a subscriber receives is **re-read through the ORM**, not lifted from the
    /// signal payload. The payload is a serde dump of the model and knows nothing about
    /// `private` / `secret` / `hide`; forwarding it would defeat the entire field policy
    /// merely because the data left over a socket instead of a response body.
    ///
    /// Gated by the same `expose_if` closure as reads — a subscription IS a read, just a long
    /// one. Note the gate is checked when the subscription is ESTABLISHED, not per event: a
    /// caller who is demoted mid-stream keeps receiving until they reconnect. If that matters,
    /// keep the streams short-lived.
    pub fn subscribable(mut self, table: impl Into<String>) -> Self {
        self.subscribable.push(table.into());
        self
    }

    /// Omit columns from the schema.
    ///
    /// ```rust,ignore
    /// .expose("product")
    /// .hide("product", "cost")                 // wholesale cost is not the public's business
    /// .hide("product", ["cost", "supplier"])
    /// ```
    ///
    /// The column is absent from the schema — not present-but-null. A field that exists and
    /// always returns null still confirms the column to anyone reading introspection.
    ///
    /// Hiding a foreign-key column also removes the relation it forms, in BOTH directions.
    /// Otherwise hiding `product.category` would be decorative: the client could still ask
    /// for `product { category { id } }` and get the id you hid.
    ///
    /// This is a *presentation* choice, and it is per-plugin on purpose — `RestPlugin::hide`
    /// is a separate list, because REST and GraphQL are swappable and each owns its own
    /// surface. What is NOT per-plugin is data that must never ship at all: `password_hash`
    /// is denied in core (`umbral::orm::HARD_DENIED_FIELDS`) and no `expose` call here can
    /// bring it back.
    pub fn hide(mut self, table: impl Into<String>, fields: impl HideFields) -> Self {
        let table = table.into();
        for f in fields.into_fields() {
            self.hidden.push((table.clone(), f));
        }
        self
    }

    /// How to identify the caller — the same `Authentication` backends `RestPlugin` takes
    /// (session cookie, bearer token, a chain of both).
    ///
    /// Without this, EVERY request is anonymous, and `expose_if` can only ever deny — a
    /// gate that cannot be opened is not a gate, it is a wall with a lock painted on it.
    /// The endpoint still works; only the guarded models become unreachable.
    pub fn authenticate<A: umbral::auth::Authentication>(mut self, auth: A) -> Self {
        self.auth = Some(Arc::new(auth));
        self
    }

    /// Serve the GraphiQL IDE on `GET <path>`. Defaults to ON in `Dev`, OFF otherwise —
    /// an interactive schema explorer on a production endpoint is a gift to whoever is
    /// enumerating your API.
    pub fn graphiql(mut self, on: bool) -> Self {
        self.graphiql = Some(on);
        self
    }

    fn resolve_exposed(&self) -> Vec<Exposed> {
        let registry = umbral::migrate::registered_models();

        // A typo in `.hide(...)` is a SECURITY typo: `.hide("product", "costt")` silently
        // leaves `cost` in the schema, and the operator believes they hid it. Say so.
        for (table, field) in &self.hidden {
            let known = registry
                .iter()
                .find(|m| &m.table == table)
                .is_some_and(|m| m.fields.iter().any(|c| &c.name == field));
            if !known {
                tracing::error!(
                    table = %table, field = %field,
                    "umbral-graphql: .hide(\"{table}\", \"{field}\") names a column that does not \
                     exist on that model — NOTHING is being hidden. Check the column name."
                );
            }
        }

        let mut out = Vec::new();
        for (table, access) in &self.exposed {
            match registry.iter().find(|m| &m.table == table) {
                Some(meta) => out.push(Exposed {
                    meta: meta.clone(),
                    access: access.clone(),
                    writable: self
                        .writable
                        .iter()
                        .find(|(t, _)| t == table)
                        .map(|(_, f)| f.clone()),
                    subscribable: self.subscribable.iter().any(|t| t == table),
                    hidden: self
                        .hidden
                        .iter()
                        .filter(|(t, _)| t == table)
                        .map(|(_, f)| f.clone())
                        .collect(),
                }),
                None => {
                    // A typo here silently produces a schema missing the type you thought
                    // you exposed, and you find out from a client. Say so at boot.
                    tracing::error!(
                        table = %table,
                        "umbral-graphql: .expose(\"{table}\") names a table that is not a registered \
                         model — it will be MISSING from the schema. Check the table name (it is \
                         `Model::TABLE`, not the struct name)."
                    );
                }
            }
        }
        out
    }
}

impl Plugin for GraphqlPlugin {
    fn name(&self) -> &'static str {
        "graphql"
    }

    /// Subscribe to the ORM's write signals for every subscribable model.
    ///
    /// In `on_ready`, not `routes()`: the model registry has to be populated first, and the
    /// signal subscription is a runtime side effect rather than part of building the router.
    fn on_ready(
        &self,
        _ctx: &umbral::plugin::AppContext,
    ) -> Result<(), umbral::plugin::PluginError> {
        for table in &self.subscribable {
            subscription::wire_signals(table);
        }
        Ok(())
    }

    fn routes(&self) -> Router {
        let path = self.path.clone().unwrap_or_else(|| "/graphql".to_string());
        let exposed = self.resolve_exposed();

        if exposed.is_empty() {
            tracing::warn!(
                "umbral-graphql: no models exposed — the schema is empty. Call \
                 `.expose(\"<table>\")` for each model the API should serve."
            );
        }

        let schema: Schema = match schema::build(&exposed) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "umbral-graphql: failed to build the schema");
                return Router::new();
            }
        };

        // `get_opt`, not `get`: settings are not published when `routes()` runs during
        // `App::build`, and `get()` panics. Absent settings => not Dev => no IDE, which is
        // the safe direction to be wrong in.
        let dev = umbral::settings::get_opt()
            .is_some_and(|s| matches!(s.environment, umbral::Environment::Dev));
        let show_ide = self.graphiql.unwrap_or(dev);

        let post_schema = schema.clone();
        let post_path = path.clone();
        let ide_path = path.clone();
        let auth = self.auth.clone();

        // A gated model with no way to authenticate is a permanent 403 the operator will
        // debug for an hour. Say it at boot instead.
        if auth.is_none() && exposed.iter().any(|e| e.access.is_some()) {
            tracing::warn!(
                "umbral-graphql: a model is exposed with `expose_if(...)` but no \
                 `.authenticate(...)` backend is configured — every request is anonymous, so \
                 those models will ALWAYS be denied. Pass the same Authentication you give \
                 RestPlugin (e.g. SessionAuthentication)."
            );
        }

        let mut router = Router::new().route(
            &path,
            axum::routing::post(
                move |headers: umbral::web::HeaderMap, req: GraphQLRequest| {
                    let schema = post_schema.clone();
                    let auth = auth.clone();
                    async move {
                        let identity: Option<umbral::auth::Identity> = match &auth {
                            Some(a) => a.authenticate(&headers).await,
                            None => None,
                        };
                        // Fresh loaders PER REQUEST. A shared cache would serve one caller's
                        // rows to another — a data leak wearing a performance costume.
                        let inner = req.into_inner().data(loader::Loaders::new()).data::<Option<
                            umbral::auth::Identity,
                        >>(
                            identity
                        );
                        GraphQLResponse::from(schema.execute(inner).await).into_response()
                    }
                },
            ),
        );

        // As a LAYER, not a check inside the handler: the handler's `GraphQLRequest` extractor
        // runs first and would reject a multipart body with its own 400 before we ever looked
        // at the content type — which means the CSRF defence would have been decided by an
        // extractor that knows nothing about CSRF. A route layer runs before extraction, so
        // the rule is enforced by the code that states it.
        router = router.route_layer(axum::middleware::from_fn(csrf_content_type_guard));

        // ---- subscriptions -------------------------------------------------
        //
        // Two transports, because they are good at different things.
        //
        // WebSocket (`<path>/ws`) is what Apollo and Relay reach for by default, and it is
        // bidirectional — the client can start and stop individual subscriptions over one
        // connection.
        //
        // SSE (`<path>/sse`) is server→client only, which is all a subscription actually
        // needs. It is plain HTTP: it survives proxies that mangle upgrades, it reconnects on
        // its own, and it needs no protocol negotiation. For "push me updates" it is the
        // cheaper half, and most apps never need more.
        let ws_endpoint: Option<String> = exposed
            .iter()
            .any(|e| e.subscribable)
            .then(|| format!("{path}/ws"));

        if exposed.iter().any(|e| e.subscribable) {
            let ws_schema = schema.clone();
            router = router.route(
                &format!("{path}/ws"),
                axum::routing::get_service(GraphQLSubscription::new(ws_schema)),
            );

            let sse_schema = schema.clone();
            router = router.route(
                &format!("{path}/sse"),
                axum::routing::post(move |req: GraphQLRequest| {
                    let schema = sse_schema.clone();
                    async move {
                        let stream =
                            schema.execute_stream(req.into_inner().data(loader::Loaders::new()));
                        axum::response::Sse::new(stream.map(|res| {
                            axum::response::sse::Event::default()
                                .json_data(res)
                                .map_err(|e| e.to_string())
                        }))
                        .keep_alive(axum::response::sse::KeepAlive::default())
                    }
                }),
            );
        }

        if show_ide {
            router = router.route(
                &ide_path,
                axum::routing::get(move || {
                    let p = post_path.clone();
                    let ws = ws_endpoint.clone();
                    async move {
                        let mut ide = GraphiQLSource::build().endpoint(&p);
                        // Without this, GraphiQL knows the query endpoint but not the
                        // socket, so a `subscription { ... }` typed into the IDE silently
                        // does nothing — the one place a developer will first try to use
                        // the feature is the one place it would not work.
                        if let Some(ws) = ws.as_deref() {
                            ide = ide.subscription_endpoint(ws);
                        }
                        Html(ide.finish()).into_response()
                    }
                }),
            );
        }

        router
    }
}
