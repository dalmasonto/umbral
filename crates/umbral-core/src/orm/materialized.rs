//! gaps6 #7 — materialized (computed) fields: a column kept fresh by the
//! framework whenever a declared source table changes. See the design spec at
//! `docs/superpowers/specs/2026-09-24-materialized-computed-fields-design.md`.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::migrate::ModelMeta;
use crate::orm::Model;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One source table for a materialized field: its table name plus an erased
/// extractor that decodes a source-row payload into the affected target pk.
// Deviation from the task-1 brief's literal `pub(crate) struct SourceReg`
// (and its `pub(crate)` fields): `crates/umbral-core/tests/materialized_spec.rs`
// — the exact test the brief specifies, verbatim — is an integration test
// compiled as its own crate, and both `pub(crate)` fields and a
// `pub(crate)` element type used as `Vec<SourceReg>` are invisible from
// there (confirmed by an E0616 "private field" / E0603 "private type" RED
// run; see task-1-report.md). Widening `SourceReg` and its two fields to
// `pub` is the minimal change that lets the brief's own test compile;
// `SourceReg` is still not part of the intended builder surface (not
// re-exported from `crate::orm`'s `pub use materialized::{...}`, and later
// tasks should keep treating it as framework-internal plumbing).
pub struct SourceReg {
    pub table: String,
    pub extract: Arc<dyn Fn(&Value) -> Option<Value> + Send + Sync>,
}

/// The fully type-erased, collectible form of a materialized-field declaration.
pub struct MaterializedSpec {
    pub(crate) target_meta: ModelMeta,
    // Widened to `pub` for the same reason as `SourceReg`'s fields above:
    // the unit test that proves the erasure boundary lives in `tests/` and
    // needs direct field access from outside the crate.
    pub target_col: String,
    pub sources: Vec<SourceReg>,
    pub recompute: Arc<dyn Fn(Value) -> BoxFuture<'static, Option<Value>> + Send + Sync>,
}

/// Builder for a single computed column on target model `M`.
pub struct Materialized<M: Model> {
    target_meta: ModelMeta,
    target_col: String,
    sources: Vec<SourceReg>,
    _m: PhantomData<fn() -> M>,
}

impl<M: Model> Materialized<M> {
    /// Name the target column (any column token, e.g. `booking::PAYMENT_TOTAL`,
    /// or a `&str`). Fixes the target model + column.
    pub fn field(col: impl AsRef<str>) -> Self {
        Self {
            target_meta: ModelMeta::for_::<M>(),
            target_col: col.as_ref().to_owned(),
            sources: Vec::new(),
            _m: PhantomData,
        }
    }

    /// Register a source model `S` and a map from a changed `S` row to the
    /// affected target pk (`None` to skip, e.g. a null FK). Call more than once
    /// for multiple sources; all share the one `recompute`.
    pub fn from<S, K, Pk>(mut self, key_fn: K) -> Self
    where
        S: Model + DeserializeOwned,
        K: Fn(&S) -> Option<Pk> + Send + Sync + 'static,
        Pk: Serialize,
    {
        let extract = Arc::new(move |instance: &Value| -> Option<Value> {
            let s: S = serde_json::from_value(instance.clone()).ok()?;
            let pk = key_fn(&s)?;
            serde_json::to_value(pk).ok()
        });
        self.sources.push(SourceReg {
            table: S::TABLE.to_owned(),
            extract,
        });
        self
    }

    /// Finalize with the recompute closure `Fn(Pk) -> Future<Output = V>`.
    /// The framework writes the returned `V` back into the target column.
    pub fn recompute_typed<F, Fut, Pk, V>(self, f: F) -> MaterializedSpec
    where
        F: Fn(Pk) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = V> + Send + 'static,
        Pk: DeserializeOwned,
        V: Serialize,
    {
        let f = Arc::new(f);
        let recompute = Arc::new(move |pk_json: Value| -> BoxFuture<'static, Option<Value>> {
            let f = f.clone();
            Box::pin(async move {
                let pk: Pk = serde_json::from_value(pk_json).ok()?;
                let value = f(pk).await;
                serde_json::to_value(value).ok()
            })
        });
        MaterializedSpec {
            target_meta: self.target_meta,
            target_col: self.target_col,
            sources: self.sources,
            recompute,
        }
    }
}
