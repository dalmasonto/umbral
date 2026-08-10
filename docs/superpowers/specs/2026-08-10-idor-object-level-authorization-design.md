# Unified object-level authorization (IDOR) — design

Status: design draft for approval (gaps5 #101 / tf#322)
Date: 2026-08-10
Scope: a cross-cutting IDOR guide plus a boot-time system check that flags write surfaces left without object-level scope, across REST, GraphQL, and storage.

## The problem

Object-level authorization already ships across four subsystems, but as isolated tools with no shared mental model and no signal when one is missing:

- REST: `ResourceConfig::owned_by(owner_column)` / `scope(..)` / `owner_field(..)` — object-level row scoping (audit_2 H1/P2), `plugins/umbral-rest/src/resource.rs`.
- GraphQL: `GraphqlPlugin::owned_by(table, owner_column)` — row-level mutation scope (gaps4 #9), `plugins/umbral-graphql/src/lib.rs`.
- RLS: `umbral-rls` DB-level row filtering via GUC/tenant var, `plugins/umbral-rls/src/lib.rs`.
- Storage: file→owner schema + owner gate + signed URLs (gaps4 #56-58), `plugins/umbral-storage/src/lib.rs`.

A developer who wires REST `owned_by` but forgets the matching RLS policy or storage owner gate has an Insecure Direct Object Reference (IDOR) hole and nothing tells them. This design closes that with (1) a single documented contract and (2) a boot-time check that warns on an unscoped write surface.

## Decisions taken (from brainstorming)

1. **Coverage:** REST + GraphQL + storage in this first version.
2. **RLS interaction:** an explicit acknowledgement marker on the resource, not cross-plugin auto-detection. `umbral-rest` cannot depend on `umbral-rls` (crate-dependency ban), so a REST check cannot read RLS policies directly. The marker makes "row security here is handled by RLS" a declared fact instead of an inferred one.
3. **Severity:** `Warning` by default (boot continues, matching `plugin_security_missing` and the host-validation checks); escalates to boot-blocking `Error` under a strict flag (the future `EnterprisePreset`, or an explicit toggle).

## Architecture

### Enabling core change: `SystemCheck` carries an owned closure

Today `SystemCheck.run` is a bare function pointer:

```rust
pub run: fn(&CheckContext<'_>) -> Vec<SystemCheckFinding>,
```

A bare `fn` cannot read a plugin instance's configured state — which is precisely why no plugin can currently validate whether it was wired safely. The one existing cross-plugin check (`plugin_security_missing`) works only off plugin *names* in `CheckContext.registered_plugin_names`. Change `run` to an owned closure:

```rust
pub run: Box<dyn Fn(&CheckContext<'_>) -> Vec<SystemCheckFinding> + Send + Sync>,
```

- Existing built-in checks become `run: Box::new(settings_required)` — mechanical, ~15 sites in `framework_checks()`.
- `run_all` is unchanged: `Box<dyn Fn>` is callable exactly like the fn pointer was.
- `Plugin::system_checks(&self)` already takes `&self`, so each plugin snapshots its own resource/policy list (owned clones) into the closure it returns.

This is the keystone: it unblocks all three plugin checks below *and* every future "is this plugin configured safely?" check. Blast radius is medium and contained (core `check.rs`, the built-in check literals, and any test that constructs a `SystemCheck` by hand). Run `gitnexus_impact` on `SystemCheck` / `run_all` before editing.

### Strict-mode flag

Add `CheckContext.strict_object_scope: bool`, populated by `App::build` from a new `Settings.strict_object_scope` (env `UMBRAL_STRICT_OBJECT_SCOPE`, default `false`). `EnterprisePreset` flips it to `true` when that preset lands; until then it is an explicit opt-in. Each object-scope check reads `ctx.strict_object_scope` to choose `Severity::Warning` vs `Severity::Error`.

### The three plugin checks

All share the check id `security.object_scope` (users grep the family). Each is contributed via `Plugin::system_checks(&self)`.

**REST (`umbral-rest`).** For every registered `ResourceConfig`, determine:
- `write_enabled` — the resource exposes any of create / update / delete (via `view_scope`, or the back-compat default that exposes every action).
- `scoped` — a `scope` / `owned_by` hook is registered for the table.
- `acked` — an acknowledgement marker is set (see below).

Emit a finding when `write_enabled && !scoped && !acked`.

**GraphQL (`umbral-graphql`).** For every `mutable` model (mutations are opt-in via `mutable`, separate from read-only `expose`), emit a finding when the table is not in `owned_by` and not acked. Read-only exposed models are unaffected.

**Storage (`umbral-storage`).** When a media route is mounted with no access gate configured (no `media_access_identity` / `media_access_owner` closure, no signed-URL requirement) and no explicit public opt-in, emit a finding: any object key is then a direct, unauthenticated fetch — the storage form of IDOR. This subsumes and upgrades the existing partial warning at `plugins/umbral-storage/src/lib.rs:245`.

### Acknowledgement markers (the RLS / intentional-public escape hatch)

- REST: `ResourceConfig::rls_backed()` and a general `ResourceConfig::unscoped_ok(reason: &str)`. `rls_backed()` is sugar for `unscoped_ok("row security enforced by RLS")`.
- GraphQL: `GraphqlPlugin::unscoped_ok(table)` (and `rls_backed(table)` sugar).
- Storage: `StoragePlugin::media_public()` — the legitimate public-asset opt-out.

A marker silences the warning for exactly that surface without disabling the whole check, and leaves a declared, greppable record of the decision.

## The guide

New user-facing docs area `documentation/docs/v0.0.1/security/`:

- `_category_.json` (sidebar label "Security", ordered).
- `object-level-authorization.mdx` (IDOR): the four layers presented as one defense-in-depth story; a "which layer when" decision table (REST/GraphQL `owned_by` for app-level scoping, RLS for DB-enforced defense-in-depth, storage owner gate for media); the boot check, how to silence it (`owned_by` / `scope` / `rls_backed` / `media_public`), and strict mode.

Minimal per "ship a feature, ship its doc page" — purpose, one example per layer, links to the specs and this design.

## Testing

Behavioral, real `App` builds (per the repo convention — real rows / real public path, assert findings, not just SQL):

- REST: build an App with a write-enabled unscoped resource → assert a `security.object_scope` Warning is present; add `.owned_by(..)` → assert it is gone; add `.rls_backed()` → assert it is gone; set `strict_object_scope` → assert `BuildError::SystemCheckFailed`.
- GraphQL: a `mutable` model with no `owned_by` warns; adding `owned_by` or `unscoped_ok` clears it.
- Storage: a mounted media route with no gate warns; `media_access_owner()` or `media_public()` clears it.
- Core: the closure change keeps every existing built-in check green (`cargo test -p umbral-core`).

## Explicitly deferred

- **Auto-detecting RLS coverage (option B2).** A core-owned coverage registry that both `umbral-rest` and `umbral-rls` publish into, so an RLS write-policy on a table auto-silences the warning with no marker. The acknowledgement marker stands in for now.
- **`EnterprisePreset` auto-enabling strict mode.** The `strict_object_scope` flag ships now; the preset wiring lands with the preset.

## Commit plan

One logical feature, split for reviewability:

1. `umbral-core`: the `SystemCheck` closure change + `strict_object_scope` context/setting (the keystone).
2. The three plugin checks + acknowledgement markers (rest / graphql / storage).
3. The `security/object-level-authorization.mdx` guide + `_category_.json`.
