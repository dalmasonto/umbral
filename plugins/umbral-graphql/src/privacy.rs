//! Which `#[umbral(private)]` columns THIS caller may see.
//!
//! `private` means "confidential, but some callers legitimately see it". The unlock lives on
//! the ORM (`DynQuerySet::allow_private`) — and until now nothing in GraphQL called it, so a
//! private column was hidden with no way to reveal it, which is what `secret` means. A
//! two-tier policy with one usable tier.
//!
//! # Why this is computed per request and not per schema
//!
//! The schema is built once, at boot. Whether *you* may see `cost` is a fact about the
//! request. So the schema carries the *possibility* (the field exists, and is nullable
//! because it may not arrive), and the resolved set of unlocked columns rides in the request
//! context — put there by the handler, which is the only place the caller's identity exists.

use std::collections::HashMap;
use std::sync::Arc;

use crate::schema::Exposed;

/// Table → the private columns this caller has unlocked on it.
#[derive(Clone, Debug, Default)]
pub struct PrivateUnlocks(HashMap<String, Vec<String>>);

impl PrivateUnlocks {
    /// Evaluate every configured unlock against this caller.
    pub(crate) fn resolve(exposed: &[Exposed], identity: Option<&umbral::auth::Identity>) -> Self {
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for e in exposed {
            for (field, check) in &e.private_unlocks {
                if check(identity) {
                    out.entry(e.meta.table.clone())
                        .or_default()
                        .push(field.clone());
                }
            }
        }
        Self(out)
    }

    pub(crate) fn for_table(&self, table: &str) -> &[String] {
        self.0.get(table).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// The caller's unlocks, out of the resolver context.
///
/// Absent context => nothing unlocked, which is the safe direction to be wrong in.
pub(crate) fn from_ctx(ctx: &async_graphql::dynamic::ResolverContext<'_>) -> Arc<PrivateUnlocks> {
    ctx.data_opt::<Arc<PrivateUnlocks>>()
        .cloned()
        .unwrap_or_default()
}

/// Is this column private *and* unlockable — i.e. present in the schema, but only sometimes
/// populated?
///
/// Such a field is emitted as **nullable** even when the column is NOT NULL, because for a
/// caller without the unlock it genuinely does not arrive. A non-null field that resolves to
/// nothing is a schema error at execution time, and would turn a permission decision into a
/// broken query.
pub(crate) fn is_conditional(e: &Exposed, col_name: &str) -> bool {
    e.private_unlocks.iter().any(|(f, _)| f == col_name)
}
