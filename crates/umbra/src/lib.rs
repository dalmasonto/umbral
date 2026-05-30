//! A Django-shape web framework for Rust.
//!
//! `umbra` is the stable surface for users and plugin authors. Code in
//! user crates and third-party plugins should `use umbra::prelude::*;`
//! and never depend on `umbra-core` or `umbra-macros` directly. See
//! `docs/specs/02-plugin-contract.md` and `docs/specs/08-authoring-plugins.md`.
//!
//! Status: M0 scaffold. The facade compiles but re-exports nothing useful
//! yet; populated as M0 lands.

pub mod prelude {
    //! The umbra prelude.
    //!
    //! Will re-export the ORM (`Model`, `QuerySet`, `Manager`), the Plugin
    //! trait, routing types (`Router`, `Request`, `Response`), and common
    //! extractors (`Json`, `Path`, `Query`, `Form`, `Auth`, `Session`).
    //! Currently empty; populated as the relevant subsystems land.
}
