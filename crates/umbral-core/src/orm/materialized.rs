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
///
/// `SourceReg` the *type* is `pub` (it appears in the public field type of
/// `MaterializedSpec::sources`), but both its fields stay `pub(crate)` — it's
/// an opaque handle. Nothing outside umbral-core reads `.table` / `.extract`
/// directly; the erasure-boundary unit test lives in this same module (see
/// `tests` below) precisely so it can.
pub struct SourceReg {
    pub(crate) table: String,
    pub(crate) extract: Arc<dyn Fn(&Value) -> Option<Value> + Send + Sync>,
}

/// The fully type-erased, collectible form of a materialized-field declaration.
///
/// Opaque handle: `.materialize()` (a later task) takes this by value as a
/// token. Fields are `pub(crate)` so the erasure internals never leak into
/// the facade / semver surface — callers only ever see a `MaterializedSpec`
/// come out of `Materialized::recompute_typed`, never construct or inspect
/// one directly.
pub struct MaterializedSpec {
    pub(crate) target_meta: ModelMeta,
    pub(crate) target_col: String,
    pub(crate) sources: Vec<SourceReg>,
    pub(crate) recompute: Arc<dyn Fn(Value) -> BoxFuture<'static, Option<Value>> + Send + Sync>,
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

#[cfg(test)]
mod tests {
    //! gaps6 #7 — the Materialized<M> builder erases M / S / Pk / V into a
    //! non-generic MaterializedSpec. This tests the erasure boundary in
    //! isolation: a source row's JSON maps to the right pk JSON, and the
    //! recompute closure's typed value comes back as JSON.
    //!
    //! In-crate (not `tests/materialized_spec.rs`) because `SourceReg` and
    //! `MaterializedSpec`'s fields are `pub(crate)` — an integration test in
    //! `tests/` is a separate crate and can't see them.
    //!
    //! The fixtures below hand-implement `crate::orm::Model` instead of using
    //! `#[derive(Model)]` (the same pattern `orm::post::Post` uses). The
    //! derive macro always expands to `::umbral::orm::Model` paths, and
    //! `umbral` (the facade) is only reachable here as a dev-dependency that
    //! itself depends on umbral-core — from an in-crate `--lib` test, that
    //! creates two non-unified copies of the `umbral_core` crate in the
    //! graph (one compiled with `--cfg test` as the unit under test, one
    //! pulled in through `umbral` without it), so the trait `S: Model` bound
    //! fails to resolve (confirmed via a real E0277 "multiple different
    //! versions of crate `umbral_core`" build before writing this). Hand-
    //! implementing `Model` directly against `crate::orm::Model` sidesteps
    //! the facade entirely and avoids the diamond.

    use serde::Deserialize;
    use serde_json::json;

    use super::Materialized;
    use crate::orm::{FieldSpec, Model, SqlType};

    #[derive(Debug, Clone, Deserialize)]
    struct MsBooking {
        id: i64,
        #[allow(dead_code)]
        payment_total: i64,
    }

    impl Model for MsBooking {
        type PrimaryKey = i64;
        const NAME: &'static str = "MsBooking";
        const TABLE: &'static str = "ms_booking";
        const FIELDS: &'static [FieldSpec] = &[
            FieldSpec {
                name: "id",
                ty: SqlType::BigInt,
                primary_key: true,
                ..FieldSpec::PLACEHOLDER
            },
            FieldSpec {
                name: "payment_total",
                ty: SqlType::BigInt,
                ..FieldSpec::PLACEHOLDER
            },
        ];
        fn primary_key(&self) -> i64 {
            self.id
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    struct MsRsvp {
        id: i64,
        booking_id: i64,
        #[allow(dead_code)]
        amount: i64,
    }

    impl Model for MsRsvp {
        type PrimaryKey = i64;
        const NAME: &'static str = "MsRsvp";
        const TABLE: &'static str = "ms_rsvp";
        const FIELDS: &'static [FieldSpec] = &[
            FieldSpec {
                name: "id",
                ty: SqlType::BigInt,
                primary_key: true,
                ..FieldSpec::PLACEHOLDER
            },
            FieldSpec {
                name: "booking_id",
                ty: SqlType::BigInt,
                ..FieldSpec::PLACEHOLDER
            },
            FieldSpec {
                name: "amount",
                ty: SqlType::BigInt,
                ..FieldSpec::PLACEHOLDER
            },
        ];
        fn primary_key(&self) -> i64 {
            self.id
        }
    }

    #[tokio::test]
    async fn builder_erases_source_extract_and_recompute_to_json() {
        let spec = Materialized::<MsBooking>::field("payment_total")
            .from::<MsRsvp, _, _>(|r: &MsRsvp| Some(r.booking_id))
            .recompute_typed(|booking_id: i64| async move { booking_id * 10 });

        assert_eq!(spec.target_col, "payment_total");
        assert_eq!(spec.sources.len(), 1);
        assert_eq!(spec.sources[0].table, "ms_rsvp");

        // extract: a source instance JSON → the affected target pk JSON
        let instance = json!({ "id": 5, "booking_id": 42, "amount": 3 });
        assert_eq!((spec.sources[0].extract)(&instance), Some(json!(42)));

        // recompute: pk JSON in → value JSON out
        let out = (spec.recompute)(json!(42)).await;
        assert_eq!(out, Some(json!(420)));
    }
}
