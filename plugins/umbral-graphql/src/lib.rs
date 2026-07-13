//! A **real** GraphQL API, derived from your models.
//!
//! ```rust,ignore
//! GraphqlPlugin::default()
//!     .expose("post")
//!     .expose("auth_user")
//! ```
//!
//! ```graphql
//! { post(id: "1") { title author { username } comments { body } } }
//! ```
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
//! # Read-only, for now
//!
//! Queries only. Mutations are the half where a mistake writes to your database, and they
//! want the same validation, permission and CSRF story the REST write path already has —
//! that is a deliberate next slice, not an oversight.

use std::sync::Arc;

use async_graphql::dynamic::Schema;
use async_graphql::http::GraphiQLSource;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::response::{Html, IntoResponse};
use umbral::migrate::ModelMeta;
use umbral::plugin::Plugin;
use umbral::web::Router;

mod loader;
mod schema;

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

/// Test-only: the plural query-field name for a model type name.
#[doc(hidden)]
pub fn plural_for_tests(model_name: &str) -> String {
    schema::plural(model_name)
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

/// Mounts a GraphQL endpoint (and GraphiQL) over the models you expose.
#[derive(Default, Clone)]
pub struct GraphqlPlugin {
    path: Option<String>,
    exposed: Vec<(String, Option<AccessFn>)>,
    graphiql: Option<bool>,
    auth: Option<Arc<dyn umbral::auth::Authentication>>,
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
        let mut out = Vec::new();
        for (table, access) in &self.exposed {
            match registry.iter().find(|m| &m.table == table) {
                Some(meta) => out.push(Exposed {
                    meta: meta.clone(),
                    access: access.clone(),
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
                        GraphQLResponse::from(schema.execute(inner).await)
                    }
                },
            ),
        );

        if show_ide {
            router =
                router.route(
                    &ide_path,
                    axum::routing::get(move || {
                        let p = post_path.clone();
                        async move {
                            Html(GraphiQLSource::build().endpoint(&p).finish()).into_response()
                        }
                    }),
                );
        }

        router
    }
}
