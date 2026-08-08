# umbral stability and support policy

Status: draft (planning/gaps5.md #2, tf #215). Current release: 0.0.11, pre-1.0.

umbral is pre-1.0 and its APIs still move (see the README). This document says what "moves" means in practice, so a team can decide what they are adopting. It pairs with the product north star (`docs/decisions/2026-08-08-product-north-star.md`): the policy below governs the Stage 1 surface, the declarative framework.

## API tiers

Not every public item carries the same promise.

- **Stable.** The surface a normal app or plugin author uses: the `umbral` facade prelude (`umbral::prelude::*`), the `Plugin` trait, `#[derive(Model)]` and its `#[umbral(...)]` field attributes, the QuerySet builder and its terminal methods, the extractors, and the `umbral` CLI subcommands. Changes here follow the deprecation policy below.
- **Evolving.** Power-user surfaces re-exported from the facade under a module rather than the prelude: raw query builders, the database backend trait, the migration operation enum, `umbral::codegen`. These may change in a minor (0.Y) release with a changelog note, without a deprecation cycle.
- **Internal.** Anything in `umbral-core` or another crate that the facade does not re-export, plus items marked `#[doc(hidden)]`. No guarantees. Do not depend on it; if you need something here, open an issue so it can be promoted to a supported tier.

## Versioning

- **One lockstep version across all crates.** Every published `umbral-*` crate shares a single version (release-plz `version_group = "umbral"`); they release together. So "umbral 0.0.11" is unambiguous across the facade, core, macros, CLI, and every plugin.
- **Semver, pre-1.0 reading.** While 0.x: breaking changes to any tier may land in a MINOR bump (0.Y.0); additive changes and fixes land in a PATCH (0.y.Z). We will not knowingly break the **Stable** tier in a PATCH. Every breaking change to the Stable tier ships with a migration note in the changelog.

## Deprecation windows

- A **Stable**-tier item is marked `#[deprecated]` with a replacement and a changelog entry for **at least one minor release** before it is removed.
- After 1.0 this extends to **at least two minor releases**.
- **Evolving** and **Internal** tiers carry no deprecation guarantee.

## MSRV (Minimum Supported Rust Version)

- MSRV is **Rust 1.85** (Rust edition 2024), declared as `rust-version` in the workspace and enforced in CI.
- MSRV is raised only in a MINOR release, with a changelog note. A PATCH never raises MSRV.

## Plugin compatibility

- Third-party plugins declare the umbral version range they support (via the plugin manifest, gaps5 #6). A boot-time system check warns when an installed plugin falls outside its declared range.
- Because versions are lockstep, "compatible with umbral 0.0.x" is a single range, not a per-crate matrix.

## Security

- Security fixes ship as a PATCH against the current minor and are announced in `SECURITY.md` (gaps5 #20) and, where applicable, filed as a RUSTSEC advisory.
- Secure-by-default posture (CSRF, signed CSRF, security headers, argon2, autoescaping, parameterized SQL) is part of the Stable contract and will not silently regress.

## 0.x to 1.0 roadmap

1.0 is cut when all of the following hold:

1. The Stage 1 framework surface (product north star) is stable and the **Stable** tier above is frozen.
2. The declare, migrate, change, migrate loop and the `Plugin` contract are frozen.
3. Secure-by-default is on (done as of 2026-08-08) and covered by an always-on security regression suite (gaps5 #98).
4. The tiers, deprecation windows, and MSRV policy here are enforced in CI, and an upgrade-compatibility suite (gaps5 #96) guards prior-release projects.

Stage 2 (self-hosted platform posture) may land across 1.x minors; it is not a 1.0 gate.

## Open decisions for the maintainer

- Confirm MSRV policy cadence (how many releases behind current stable Rust to support).
- Confirm whether the Evolving tier should get a one-release deprecation courtesy too, or stay change-at-will.
