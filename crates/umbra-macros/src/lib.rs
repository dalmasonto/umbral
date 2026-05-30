//! Derive and attribute macros for umbra: `#[derive(Model)]`, `#[task]`, etc.
//!
//! Do not depend on this crate directly. Use the `umbra` facade, which
//! re-exports the derives so user code only ever imports `umbra`.
//!
//! Status: M3 in flight. `#[derive(Model)]` is the only derive shipped;
//! more land as their milestones do. See `docs/specs/04-orm-model-and-
//! fields.md` for the target shape — what M2's hand-written `impl Model
//! for Post` looks like is exactly what this derive emits.

use proc_macro::TokenStream;

/// Generate `impl Model` for a struct.
///
/// Emits the trait impl, the sibling column module, and an inherent
/// `objects()` entry point. Filled in by the M3 fan-out subagent A.
#[proc_macro_derive(Model)]
pub fn derive_model(_input: TokenStream) -> TokenStream {
    // Stub: M3 implementation lands in the fan-out commit. Returning an
    // empty TokenStream lets the workspace compile in the meantime;
    // anything that tries to USE `#[derive(Model)]` won't get the trait
    // impl yet so its tests will fail to compile until A's work lands.
    TokenStream::new()
}
