//! The model-exposure contract: the derived views over `ModelMeta` that every
//! serializer plugin (REST, GraphQL, admin, a future gRPC) reads to learn a
//! model's shape, instead of re-deriving "which fields are serializable /
//! writable" by hand.
//!
//! Kept separate from the migration-snapshot code in `migrate.rs`: this module
//! owns runtime exposure, that one owns schema snapshots — even though both
//! `impl ModelMeta`.
//!
//! Facts on the model, policy in the plugin: these methods return declarative
//! facts; each plugin layers its own transport policy (REST `.hide()`,
//! permissions, nesting) on top. Raw per-field facts (`help`, `widget`,
//! `choices`, `secret`, `private`, …) and the declared presentation lists
//! (`list_display`, `search_fields`, …) are read directly off the public
//! `Column`/`ModelMeta` fields — this module adds only DERIVED views.
//!
//! See `docs/decisions/2026-09-15-model-exposure-contract.md`.

use crate::migrate::{Column, ModelMeta};

impl ModelMeta {
    /// Look up a field by name.
    pub fn field(&self, name: &str) -> Option<&Column> {
        self.fields.iter().find(|c| c.name == name)
    }

    /// The display column — the first field flagged `is_string_repr`, if any.
    pub fn display_field(&self) -> Option<&Column> {
        self.fields.iter().find(|c| c.is_string_repr)
    }

    /// Fields safe to serialize with no unlocks: not `secret` and not `private`.
    pub fn public_fields(&self) -> impl Iterator<Item = &Column> {
        self.fields.iter().filter(|c| !c.secret && !c.private)
    }

    /// Serializable set, optionally adding back `private` fields for a caller
    /// that has authorized it: `!secret && (!private || allow_private)`.
    /// `secret` is never included either way.
    pub fn serializable_fields(&self, allow_private: bool) -> impl Iterator<Item = &Column> {
        self.fields
            .iter()
            .filter(move |c| !c.secret && (!c.private || allow_private))
    }

    /// True when the server populates this field, never the client: the primary
    /// key, or any `auto_*` column.
    pub fn is_server_managed(&self, f: &Column) -> bool {
        f.primary_key
            || f.auto_now
            || f.auto_now_add
            || f.auto_user
            || f.auto_user_add
            || f.auto_uuid
    }

    /// The safe client-writable set on create/update: not server-managed, not
    /// `noform`, not `privileged`. (`private`/`secret` are READ facts and do not
    /// affect writability.)
    pub fn writable_fields(&self) -> impl Iterator<Item = &Column> {
        self.fields
            .iter()
            .filter(|c| !self.is_server_managed(c) && !c.noform && !c.privileged)
    }

    /// Foreign-key columns (those carrying a `fk_target`).
    pub fn foreign_keys(&self) -> impl Iterator<Item = &Column> {
        self.fields.iter().filter(|c| c.fk_target.is_some())
    }
}
