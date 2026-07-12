//! Per-field clean / validate hooks (features #83).
//!
//! The declarative attributes — `#[umbral(trim, lowercase, max_length, email)]` —
//! cover the rules the framework can name. This is the escape hatch for the ones
//! only your app knows: masking a banned word, normalising a phone number into
//! E.164, rejecting a username that collides with a reserved route.
//!
//! One hook shape covers both jobs, because in practice they are the same job:
//!
//! ```ignore
//! register_cleaner::<Post>("title", |v| {
//!     let s = v.as_str().unwrap_or_default();
//!     if s.contains("<script") {
//!         return Err("HTML is not allowed in a title".into());  // reject
//!     }
//!     Ok(json!(s.replace("damn", "d***")))                       // transform
//! });
//! ```
//!
//! `Ok(value)` rewrites the value before it is written; `Err(message)` fails the
//! write as a [`WriteError::Validator`], which every surface already knows how to
//! render — a REST 400 with a per-field error map, the `Form<T>` extractor's
//! errors, and the admin's inline field errors. You write the rule; you do not
//! wire it anywhere.
//!
//! # Where it runs
//!
//! At the same seam as `trim` / `lowercase`, which means **every** write path:
//! the typed `create` / `bulk_create` / `update_values`, and the dynamic path that
//! REST and the admin run on. A hook that only fired for *some* writers would be
//! worse than none — it would look enforced while a background job walked past it.
//!
//! The framework ships no word lists, no policy, no opinions about content. It
//! ships the hook.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use serde_json::Value as JsonValue;

use crate::orm::Model;
use crate::orm::write::WriteError;

/// A field hook: rewrite the value, or reject the write with a message.
///
/// Sync on purpose. A cleaner runs inside the write path, once per field per row,
/// and an `await` there would put a database round-trip (or an HTTP call to a
/// moderation API) in the middle of every insert. If your rule genuinely needs
/// I/O, do it in the handler and pass the result down — that keeps the cost where
/// the developer can see it.
pub type Cleaner = Arc<dyn Fn(&JsonValue) -> Result<JsonValue, String> + Send + Sync>;

type Registry = RwLock<HashMap<(String, String), Vec<Cleaner>>>;

fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register a clean/validate hook for one field of one model.
///
/// Call it at boot — from `main`, or a plugin's `Plugin::on_ready`. Hooks run in
/// registration order, each seeing the previous one's output, so you can compose
/// a normalise step and a reject step.
///
/// # Panics
///
/// If `field` is not a column on `M`. That is a boot-time programming error, and
/// the alternative — quietly registering a hook against a field that does not
/// exist — means a moderation or sanitisation rule that *looks* installed and
/// never runs. Failing loudly at startup is the whole point.
pub fn register_cleaner<M: Model>(
    field: &str,
    f: impl Fn(&JsonValue) -> Result<JsonValue, String> + Send + Sync + 'static,
) {
    if !M::FIELDS.iter().any(|c| c.name == field) {
        let known: Vec<&str> = M::FIELDS.iter().map(|c| c.name).collect();
        panic!(
            "umbral: register_cleaner::<{}>(\"{field}\") names a field that does not exist on \
             `{}` (columns: {known:?}). A hook on a misspelled field would silently never run.",
            M::TABLE,
            M::TABLE,
        );
    }
    registry()
        .write()
        .expect("cleaner registry poisoned")
        .entry((M::TABLE.to_string(), field.to_string()))
        .or_default()
        .push(Arc::new(f));
}

/// Drop every registered hook. Test-only — the registry is process-global, so a
/// test that registers one would otherwise leak into the next.
#[doc(hidden)]
pub fn clear_for_tests() {
    registry()
        .write()
        .expect("cleaner registry poisoned")
        .clear();
}

/// True when *any* hook is registered — checked before the per-field work so a
/// codebase with no cleaners pays a single atomic read per write, not a hash
/// lookup per column.
pub(crate) fn any_registered() -> bool {
    registry().read().map(|r| !r.is_empty()).unwrap_or(false)
}

/// Run every hook registered for `(table, field)` against `value`.
///
/// `Ok(Some(v))` — a hook rewrote it. `Ok(None)` — no hooks, nothing to do (the
/// caller can skip a clone). `Err` — a hook rejected the write.
pub(crate) fn apply(
    table: &str,
    field: &str,
    value: &JsonValue,
) -> Result<Option<JsonValue>, WriteError> {
    if !any_registered() {
        return Ok(None);
    }
    let hooks = {
        let reg = registry().read().expect("cleaner registry poisoned");
        match reg.get(&(table.to_string(), field.to_string())) {
            Some(h) => h.clone(),
            None => return Ok(None),
        }
    };

    let mut current = value.clone();
    for hook in &hooks {
        // Each hook sees the previous one's output, so a normalise step and a
        // reject step compose in registration order.
        current = hook(&current).map_err(|message| WriteError::Validator {
            field: field.to_string(),
            message,
        })?;
    }
    Ok(Some(current))
}
