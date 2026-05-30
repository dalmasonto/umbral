# Examples

Standalone test apps that exercise umbra as a consumer would.

Each subdirectory is its own Cargo project, **not** a member of the umbra workspace. They depend on the local umbra via a relative path:

```toml
# examples/<name>/Cargo.toml
[dependencies]
umbra = { path = "../../crates/umbra" }
```

Running this way preserves the experience of `cargo add umbra` from a real downstream project: the example only sees what the facade re-exports, and missing prelude entries fail loudly at the example boundary rather than silently inside the workspace.

## Adding a new example

```bash
cd examples
cargo new <name>
# edit examples/<name>/Cargo.toml to add the umbra path dep
```

The example then runs standalone:

```bash
cd examples/<name>
cargo run
```

Workspace-level commands (`cargo build`, `cargo test` at the repo root) do not touch the examples. That's intentional. To verify every example builds, walk them explicitly.

## What belongs here

- Minimal "hello world" apps exercising one feature at a time.
- A canonical blog app matching the example schema in `docs/specs/00-overview.md` (`User → Author → Post ←M2M→ Tag via PostTag`). Built up incrementally as M0–M6 land.
- Porting validation apps: an existing Postgres schema fed through `inspectdb` to confirm the porting story works end-to-end.
- Plugin author smoke tests: small third-party-plugin crates that exercise `08-authoring-plugins.md`.

## What does NOT belong here

- Documentation-illustration snippets. Those live in user-facing MDX under `documentation/docs/v0.0.1/`.
- Code that's part of the framework itself. That goes in `crates/` (or `plugins/` from M9+).
- Internal integration tests. Those live in each crate's `tests/` directory.
