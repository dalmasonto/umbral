//! A Django-shape web framework for Rust.
//!
//! `umbra` is the stable surface for users and plugin authors. Code in
//! user crates and third-party plugins should `use umbra::prelude::*;`
//! and never depend on `umbra-core` or `umbra-macros` directly. The
//! visibility rule is set out in `arch.md §2.1`; the contract that plugin
//! authors write against lives in `docs/specs/02-plugin-contract.md`.
//!
//! The internal crate split (`umbra-core`, `umbra-macros`, …) can be
//! refactored freely as long as the names exposed here keep their shape.
//!
//! Status: M0. Surfaces `Settings`, the db pool accessors, the web
//! re-exports, and the `App` builder.

pub mod prelude {
    //! Common imports for handlers, models, and plugin authors.
    //!
    //! `use umbra::prelude::*;` pulls in the items most code reaches for:
    //! the `App` builder, `Settings`, the router-construction surface, the
    //! standard extractors, and the response traits. Power-user items live
    //! on the facade itself rather than in the prelude: for example, the
    //! raw pool accessors are reached as `umbra::db::pool()` so they do
    //! not pollute the prelude with bare names like `pool`.

    pub use crate::web::{
        Form, IntoResponse, Json, JsonResponse, Path, Query, Router, delete, get, patch, post, put,
    };
    pub use crate::{App, AppBuilder, Environment, Settings};
}

pub use umbra_core::app::{App, AppBuilder, BuildError};
pub use umbra_core::settings::{Environment, Settings};

pub mod db {
    //! Database pool accessors.
    //!
    //! `connect` opens a new pool; `pool` and `pool_for` return the
    //! ambient pools published by `App::build()`.

    pub use umbra_core::db::{connect, pool, pool_for};
}

pub mod web {
    //! The web layer: router, extractors, and response types.
    //!
    //! Re-exports `umbra-core`'s web surface (currently a curated slice of
    //! axum). Later milestones will add umbra-specific wrappers without
    //! changing the names here.

    pub use umbra_core::web::*;
}
