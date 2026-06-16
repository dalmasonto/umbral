//! The swappable `DatabaseRouter` trait and its default implementation.
//! See `docs/superpowers/specs/2026-06-16-database-router-foundation-design.md`.

use std::sync::{Arc, OnceLock};

use crate::db::route_context::RouteContext;
use crate::migrate::ModelMeta;

/// A database alias — the key under which a pool is registered
/// (`App::builder().database(alias, pool)`), e.g. `"default"`, `"replica"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Alias(String);

impl Alias {
    pub fn new(s: impl Into<String>) -> Self {
        Alias(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// The conventional default alias.
    pub fn default_alias() -> Self {
        Alias("default".to_string())
    }
}

impl From<&str> for Alias {
    fn from(s: &str) -> Self {
        Alias(s.to_string())
    }
}
impl From<String> for Alias {
    fn from(s: String) -> Self {
        Alias(s)
    }
}

/// A validated Postgres schema identifier. Constructed only through
/// [`Schema::new`], which rejects anything that isn't a safe identifier,
/// so a schema name can never be a SQL-injection vector — it is always
/// emitted as a quoted identifier regardless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema(String);

impl Schema {
    /// Validate and wrap a schema name: `^[A-Za-z_][A-Za-z0-9_]*$`, 1..=63 chars
    /// (Postgres identifier limit). Returns `None` for anything else.
    pub fn new(s: impl Into<String>) -> Option<Self> {
        let s = s.into();
        let ok = (1..=63).contains(&s.len())
            && s.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        ok.then_some(Schema(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The operation a route is being resolved for. The query terminal knows
/// whether it is reading or writing; this is passed to the seam, not stored
/// in the context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteOp {
    Read,
    Write,
}

/// Swappable routing policy. Every decision umbra makes about *which*
/// database/relation/migration target, plus the optional per-request schema,
/// flows through this trait. The default methods reproduce today's behavior;
/// install a custom impl via `App::builder().router(MyRouter)`.
pub trait DatabaseRouter: Send + Sync {
    /// Alias of the database to read `model` from for this request.
    fn db_for_read(&self, model: &ModelMeta, ctx: &RouteContext) -> Alias {
        let _ = ctx;
        default_alias_for(model)
    }

    /// Alias of the database to write `model` to for this request.
    fn db_for_write(&self, model: &ModelMeta, ctx: &RouteContext) -> Alias {
        let _ = ctx;
        default_alias_for(model)
    }

    /// May a relation (FK) span these two models? Default: same alias only
    /// (the #22 cross-DB FK guard).
    fn allow_relation(&self, a: &ModelMeta, b: &ModelMeta) -> bool {
        default_alias_for(a) == default_alias_for(b)
    }

    /// Should `model` be migrated on database `alias`? Default: yes when
    /// `alias` is the model's assigned alias.
    fn allow_migrate(&self, alias: &str, model: &ModelMeta) -> bool {
        default_alias_for(model).as_str() == alias
    }

    /// The Postgres schema to scope this request's queries to. Default: None
    /// (no qualification — today's behavior). `Some(schema)` makes the SQL
    /// builder schema-qualify table references.
    fn schema_for(&self, ctx: &RouteContext) -> Option<Schema> {
        let _ = ctx;
        None
    }
}

/// Today's static precedence, resolved by name: per-model `Model::DATABASE`
/// then per-plugin `Plugin::database()` (both folded into `MODEL_ALIASES` at
/// build) then `"default"`. This is exactly what the old `resolve_pool` did.
fn default_alias_for(model: &ModelMeta) -> Alias {
    match crate::migrate::model_alias(&model.name) {
        Some(a) => Alias::new(a),
        None => Alias::default_alias(),
    }
}

/// The zero-override router. Every method is the trait default.
#[derive(Debug, Default)]
pub struct DefaultRouter;

impl DatabaseRouter for DefaultRouter {}

static ROUTER: OnceLock<Arc<dyn DatabaseRouter>> = OnceLock::new();
static DEFAULT: OnceLock<Arc<dyn DatabaseRouter>> = OnceLock::new();

/// Install the app's router. Called once during `App::build`. Idempotent
/// no-op on a second call (so tests that build twice don't blow up).
pub(crate) fn install_router(router: Arc<dyn DatabaseRouter>) {
    let _ = ROUTER.set(router);
}

fn default_router_arc() -> Arc<dyn DatabaseRouter> {
    DEFAULT.get_or_init(|| Arc::new(DefaultRouter)).clone()
}

/// The ambient router: the installed one, or `DefaultRouter` before/without
/// `App::build` (boot, CLI, low-level tests).
pub fn router() -> Arc<dyn DatabaseRouter> {
    ROUTER.get().cloned().unwrap_or_else(default_router_arc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_accepts_valid_identifiers_and_rejects_the_rest() {
        assert!(Schema::new("tenant_7").is_some());
        assert!(Schema::new("_private").is_some());
        assert!(Schema::new("public").is_some());
        // rejects injection / malformed
        assert!(Schema::new("").is_none());
        assert!(Schema::new("1tenant").is_none());
        assert!(Schema::new("a b").is_none());
        assert!(Schema::new("drop\";--").is_none());
        assert!(Schema::new("a".repeat(64)).is_none());
    }

    #[test]
    fn alias_roundtrips() {
        assert_eq!(Alias::from("replica").as_str(), "replica");
        assert_eq!(Alias::default_alias().as_str(), "default");
    }
}
