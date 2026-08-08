# Authorization policy model: named ABAC policies + typed RLS builders (design)

Status: draft (planning/gaps5.md #16 tf#229, and #17 tf#230)
Date: 2026-08-08
Realizes Stage 2 (self-hosted platform posture) from `docs/decisions/2026-08-08-product-north-star.md`. Ties to gaps5 #38 (tf#251, unify security rules across REST, storage, realtime, permissions, and RLS).

## Problem

umbral already ships two authorization layers, and they work, but they sit at two different altitudes and neither is enough for an organization-grade deployment.

1. **`umbral-permissions`** (`plugins/umbral-permissions/src/lib.rs`) is RBAC: `ContentType`, `Permission` (keyed by a composite `"<app_label>.<codename>"` string since gap #60), `Group`, and the `UserGroup` / `UserPermission` / `ObjectPermission` join tables. Checks are free functions: `has_perm(user_id, "blog.publish_post")`, `has_perm_scoped(user_id, app_label, codename)`, `has_object_perm(...)`, `objects_with_perm(...)`, `user_perms(...)`. Enforcement in handlers goes through `permission_required("blog.change_post")` (a `tower::Layer`) or the `PermissionRequired` extractor. This is coarse: a permission is a global boolean grant on a model verb. It cannot say "a manager may edit posts only inside their own tenant, only during business hours, only while the post is in `draft`."

2. **`umbral-rls`** (`plugins/umbral-rls/src/lib.rs`) is Postgres row-level security. A policy today is `RlsPlugin::new().policy(table, name, Action, using_sql)` where `using_sql` is a **raw SQL string interpolated verbatim** into `CREATE POLICY ... USING (...)`. The doc-comment (lines 71 to 90) is blunt about it: DDL has no placeholder syntax, so there is no parameter binding, and any user-sourced fragment is `EXECUTE` on the server. It is correct but sharp. It also only enforces at the database, so it says nothing about REST list scoping, storage object gates, or realtime channel authorization, which each re-implement their own checks.

What organizations need on top of this:

- **Attribute-based rules (ABAC), not just role booleans.** Decisions over subject attributes (roles, tenant, department), resource attributes (owner, tenant, status), the action, and request context (time, IP, MFA level).
- **Named, versioned, auditable policies.** A policy has a name, a version, an author, and an audit trail of who changed it and when.
- **One decision, many enforcement points.** The same rule should drive a handler guard, a REST list scope, an RLS predicate, and a storage gate, instead of being hand-copied into each.
- **Dry-run / simulation.** "Would `alice` be allowed to `change` post 42 right now, and why?" answerable without performing the action.
- **Tenant role templates.** A tenant admin picks from a curated set of roles, not raw permission codenames.
- **Admin-editable assignment with guardrails.** Delegating role assignment to non-engineers without letting them grant themselves superuser or break tenant isolation.

This document proposes two composable plugins that layer over the existing two, changing neither of their public contracts:

- **`umbral-policy`** (gaps5 #16): the named-policy / ABAC engine, plus tenant role templates, admin editing with guardrails, dry-run, and the audit trail.
- The **typed RLS builder API** inside `umbral-rls` (gaps5 #17): `RlsPolicy::owner(...)`, `::team(...)`, `::tenant(...)` that emit the correct Postgres SQL, plus a lint for raw policies and a simulation harness.

Both are opt-in plugins. A REST-free, RLS-free app compiles and runs with zero policy code, per the thin-core rule.

---

# Part 1 (gaps5 #16): the named policy model (`umbral-policy`)

## The Policy value

A policy is a named, typed predicate over the tuple `(subject, resource, action, context)`. It is data, not raw SQL, so it can be evaluated in Rust for a handler decision AND compiled down to an RLS predicate for the database. The core type:

```rust
/// One named authorization policy. Typed predicate, not SQL.
pub struct Policy {
    pub name: String,          // stable identifier, e.g. "post.edit.tenant_manager"
    pub version: u32,          // bumped on every change; the audit trail keys on it
    pub effect: Effect,        // Allow | Deny (Deny wins on conflict)
    pub action: ActionMatch,   // Read | Create | Update | Delete | Custom(&str) | Any
    pub resource: ResourceMatch, // which model/table this applies to (by ModelMeta name)
    pub predicate: Expr,       // the typed condition tree (see below)
    pub description: String,
}
```

`Effect::Deny` beats `Effect::Allow` when both match a request, which is the standard deny-override rule and the safe default. The absence of any matching `Allow` is itself a deny (default-deny), consistent with the REST `ReadOnly` posture already shipped (see the "REST is safe-by-default" memory: no matching allow means 403, not 200).

### The typed predicate tree

`Expr` is a small, closed, serializable enum. It never contains raw SQL. This is the load-bearing design choice: because it is a typed tree over named attributes, the same value evaluates in Rust and lowers to SQL.

```rust
pub enum Expr {
    True,
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),

    // Subject attributes: resolved from the authenticated Identity + permissions.
    HasPerm(String),              // delegates to umbral_permissions::has_perm
    InGroup(String),              // delegates to is_in_group
    IsSuperuser,
    SubjectAttr(String, Op, Value), // e.g. subject.department == "eng"

    // Resource attributes: columns on the target model.
    ResourceAttr(String, Op, Value),        // resource.status == "draft"
    ResourceOwnedBy(String),                // resource.<col> == subject.id
    ResourceTenantMatches(String),          // resource.<col> == context.tenant

    // Context attributes: request-time facts.
    ContextAttr(String, Op, Value),         // context.mfa_level >= 2, context.hour in 9..18
}

pub enum Op { Eq, Ne, Lt, Le, Gt, Ge, In }
pub enum Value { Int(i64), Text(String), Bool(bool), List(Vec<Value>), SubjectId, ContextTenant }
```

The three attribute families map cleanly onto what umbral already resolves per request:

- **subject** comes from `umbral::auth::Identity` (resolved by the `OptionalIdentity` / session / bearer extractors in `umbral-auth`) plus `umbral-permissions` group/permission membership.
- **resource** is a row of a registered model, addressed by column name against its `ModelMeta`.
- **context** comes from the `RouteContext` (the same place `AuthPlugin::with_db_session_var` sets `app.user_id` and a tenant key can be parsed off the host header, per `umbral-rls`'s doc-comment) plus request metadata (time, IP, MFA assurance).

## Registration: a plugin, like everything else

```rust
App::builder()
    .plugin(SessionsPlugin::default())
    .plugin(AuthPlugin::<AuthUser>::default().with_db_session_var("app.user_id"))
    .plugin(PermissionsPlugin::default())
    .plugin(
        PolicyPlugin::new()
            .policy(Policy::allow("post.read.same_tenant")
                .on::<Post>()
                .action(ActionMatch::Read)
                .when(Expr::ResourceTenantMatches("tenant_id".into())))
            .policy(Policy::allow("post.edit.tenant_manager")
                .on::<Post>()
                .action(ActionMatch::Update)
                .when(Expr::and([
                    Expr::InGroup("tenant_manager".into()),
                    Expr::ResourceTenantMatches("tenant_id".into()),
                    Expr::ResourceAttr("status".into(), Op::Ne, Value::Text("archived".into())),
                ]))),
    )
    .build()?;
```

`PolicyPlugin` contributes models (the policy store and audit tables, below), no routes by default, and an `on_ready` that validates every registered policy against the `ModelMeta` registry (`umbral::migrate::registered_models()`), failing boot if a policy names a column or model that does not exist. This is the same "fail at boot, not in prod" rule the field/backend system check already follows.

## One policy graph, three enforcement points (gaps5 #38)

The point of a typed predicate is that it lowers to more than one target. `umbral-policy` exposes one decision function and three compilers.

### 1. In-process decision (handlers, storage gates, realtime)

```rust
pub async fn decide(subject: &Subject, action: Action, resource: &ResourceRef) -> Decision;
// Decision { allowed: bool, matched: Vec<PolicyRef>, explanation: Explanation }
```

`decide` evaluates the matching policies' `Expr` trees in Rust. `HasPerm` / `InGroup` delegate straight to the existing `umbral_permissions::has_perm` / `is_in_group` free functions, so RBAC is not re-implemented, it is a leaf of the ABAC tree. This is what a custom handler, a storage-object gate, and a realtime channel-subscribe check all call. It is the single seam gaps5 #38 asks for: REST, storage, and realtime stop hand-rolling their own checks and route through `decide`.

A thin adapter re-expresses the current `permission_required(...)` layer on top of `decide`, so existing `.permission_required("blog.change_post")` call sites keep working unchanged; internally the layer becomes "policy named after this codename, action derived from the verb."

### 2. REST list scoping

For a list endpoint, evaluating per-row in Rust after fetching every row is wrong (it leaks counts and wastes work). Instead the read policy lowers to a `Predicate<T>` (the ORM's typed filter) that the REST viewset ANDs into its base queryset:

```rust
pub fn as_queryset_filter<T: Model>(policies: &[Policy], subject: &Subject) -> Predicate<T>;
```

`ResourceOwnedBy("author_id")` becomes `author::AUTHOR_ID.eq(subject.id)`; `ResourceTenantMatches("tenant_id")` becomes `post::TENANT_ID.eq(subject.tenant)`. Only the ORM-expressible subset of `Expr` lowers here (owner / tenant / resource-attribute comparisons); a predicate that depends on non-column context (for example `context.hour`) is enforced as a whole-endpoint gate instead, and the compiler reports which policies could not be pushed into the query. This uses the ORM, not raw SQL, honoring the "plugins use the ORM" rule.

### 3. RLS predicate (Postgres, defense in depth)

The same `Expr` lowers to a Postgres `USING` / `WITH CHECK` clause via the typed RLS builder in Part 2. This is the critical property: a read policy expressed once produces both the REST filter AND the database-enforced RLS predicate, so an app-layer bug cannot leak rows the database itself refuses to return. `ResourceOwnedBy("author_id")` lowers to `RlsPolicy::owner("author_id")`, which emits `author_id = NULLIF(current_setting('app.user_id'), '')::int`. The lowering is only attempted for the column/owner/tenant subset that maps to safe generated SQL; anything outside it stays an app-layer decision and is reported, never silently dropped.

The honest limit: RLS lowering is **Postgres-only**. On SQLite the policy still enforces at the app layer via `decide` and `as_queryset_filter`; it just loses the database-level backstop, exactly as `umbral-rls` already degrades (fail-closed in `Environment::Prod`, warn in dev). The framework should surface this: a policy marked `rls_enforced` on a SQLite prod boot is the same misconfiguration class `RlsPlugin::on_ready` already refuses.

## Dry-run / simulation

`decide` returns an `Explanation` even on the happy path, and a dedicated entry point runs it without side effects:

```rust
pub async fn explain(subject: &Subject, action: Action, resource: &ResourceRef) -> Explanation;
```

`Explanation` is a tree mirroring the `Expr` that was evaluated, annotated with the concrete value each leaf saw and whether it passed:

```
DENY  post.edit.tenant_manager v3
  AND
    InGroup("tenant_manager")            subject alice groups = {editor}        => false  <-- failed here
    ResourceTenantMatches("tenant_id")   resource.tenant_id=7  context.tenant=7 => true
    ResourceAttr(status != "archived")   resource.status="draft"                => true
Result: no Allow matched  =>  DENY (default-deny)
```

Exposed three ways:

- **CLI:** `umbral policy explain --subject alice --action update --resource post:42`, for debugging in a shell.
- **Admin panel:** a "why?" button on any object, using the `AdminView` custom-view seam (see the admin-custom-views work), that renders the explanation tree.
- **Test harness:** `PolicySim::new().as_user("alice").on(post).expect_denied()` so policies get behavioral tests (real subject, real resource, assert the decision AND the matched policy), matching the "behavioral tests, not random asserts" rule. This is the "policy tests" the gaps5 #16 fix recommends.

## Tenant role templates

A role template is a named bundle of policies (and/or permission codenames) that a tenant admin can assign as one unit, without seeing raw codenames:

```rust
RoleTemplate::new("tenant_manager")
    .grants_permissions(["blog.change_post", "blog.view_post"])
    .grants_policies(["post.edit.tenant_manager", "post.read.same_tenant"])
    .scoped_to_tenant();   // assignment is always tenant-bounded
```

Templates are stored, versioned, and surfaced in the admin as a short pick-list. Assigning a template to a user materializes into the existing `UserGroup` / `UserPermission` rows plus a policy-assignment row, so the RBAC layer and its `has_perm` fast path stay authoritative; templates are a curation layer on top, not a parallel store.

## Admin editing with guardrails

Delegated administration is where authorization systems get dangerous. Guardrails (all enforced server-side in the policy plugin, never only in the UI):

- **No privilege escalation past your own grants.** An admin can only assign roles/policies that are a subset of what they themselves hold (or have been explicitly delegated the right to grant). You cannot grant `IsSuperuser` unless you are a superuser. This is checked in `decide` against the acting admin, using the same engine.
- **Tenant containment.** A tenant admin's edits are auto-scoped to their tenant; the assignment write is itself gated by a `ResourceTenantMatches` policy, so isolation cannot be edited away from inside a tenant.
- **Protected policies are code-only.** Policies registered in `main.rs` via `PolicyPlugin::new().policy(...)` are immutable from the admin; only policies explicitly marked `admin_editable()` can be changed at runtime. This keeps the security-critical baseline in version control and reviewable, while letting non-engineers tune the editable surface.
- **Every write is simulated and audited.** Saving an edited policy runs the simulation harness against a configurable set of canary requests and refuses a change that would, for example, grant anonymous write. The refusal and the diff both land in the audit trail.

These are the "guardrails" gaps5 #16 asks for, and they reuse the same `decide` engine rather than being a special case.

## Audit trail

Two model-backed tables (contributed by the plugin, migrated like any other, per the "each plugin owns its migrations" rule):

- **`policy_revision`**: append-only. One row per policy version: `(name, version, effect, action, resource, predicate_json, author_id, created_at, note)`. Editing a policy never mutates a row; it writes a new version and bumps `Policy::version`. This is the audit-of-the-rules.
- **`authz_decision_log`** (optional, sampled): who asked to do what to which resource, the decision, and the matched policy versions. This is the audit-of-the-decisions. Off by default (it is high-volume); enabled via `PolicyPlugin::with_decision_log()` with a sampling rate, and always-on for `Deny` on sensitive actions. Writes go through the ORM (`objects().create(...)`), never raw SQL.

Both are queryable in the admin, giving "who changed this rule, when, and why" and "why was this request allowed / denied" from one place.

## What this deliberately does not do

- It does not replace `umbral-permissions`. RBAC stays the fast, cacheable substrate; ABAC policies reference it via `HasPerm` / `InGroup` leaves.
- It does not put raw SQL anywhere. `Expr` is a closed enum; the only SQL that exists is what the typed RLS builder generates (Part 2).
- It is not a general rules DSL with user-authored code. Predicates are chosen from typed constructors, which is what makes them safe to admin-edit and simulatable.

---

# Part 2 (gaps5 #17): typed RLS builders, lint, and simulation

Today an RLS policy body is a raw string (`plugins/umbral-rls/src/lib.rs`, the `using` / `with_check` fields, interpolated verbatim in `render_policy_sql`). That is fine for developer-authored SQL and the doc-comment is honest about the injection surface, but it is too sharp for anything admin-managed, and it makes the common cases (owner, team, tenant) error-prone to hand-write correctly (the `NULLIF(current_setting('app.user_id'), '')` dance is easy to get wrong, and a wrong `WITH CHECK` silently under- or over-permits).

## The typed builder

Add `RlsPolicy`, a typed constructor set that produces the exact same `Policy` struct `RlsPlugin` already consumes, so nothing downstream (`apply_policies`, `drop_undeclared_policies`, the reconcile logic, FORCE emission) changes. The builders generate correct, escaped SQL for the ownership / team / tenant shapes:

```rust
// Owner: the row's <column> equals the current session user.
RlsPlugin::new()
    .rls("post", "own_rows", Action::All, RlsPolicy::owner("author_id"))
// emits USING (author_id = NULLIF(current_setting('app.user_id'), '')::int)
//   and, for INSERT/UPDATE, the same as WITH CHECK.

// Tenant: the row's <column> equals the current tenant key.
    .rls("invoice", "tenant_isolation", Action::All,
         RlsPolicy::tenant("tenant_id").session_var("app.tenant_id"))

// Team: the user is a member of the row's team, via a membership table.
    .rls("doc", "team_read", Action::Select,
         RlsPolicy::team("team_id")
             .via_membership("team_member", "team_id", "user_id"))
// emits USING (EXISTS (SELECT 1 FROM team_member m
//                      WHERE m.team_id = doc.team_id
//                        AND m.user_id = NULLIF(current_setting('app.user_id'), '')::int))
```

`RlsPolicy` design points:

- It is a builder that resolves to `{ using: String, with_check: Option<String> }`, the two fields `Policy` already has. The session variable defaults to `app.user_id` (matching `AuthPlugin::with_db_session_var`) and is overridable per builder.
- It always wraps the GUC read in `NULLIF(current_setting(var), '')` so anonymous requests (empty string) yield a clean empty result rather than the `unrecognized configuration parameter` 500 the current doc-comment warns about. The PK type cast (`::int`, `::uuid`, `::text`) is chosen from the target column's `ModelMeta` field type, so the String / Uuid / i64 PK work (from the PrimaryKey refactor) carries through to RLS.
- Column and table identifiers passed to the builder are validated against the model's `ModelMeta` at build time and double-quote-escaped through the existing `escape_ident`, so a builder policy can never emit an injectable identifier. The generated expression is constructed from typed parts, not string concatenation of caller input.
- `with_check` is derived automatically for the common cases (owner/tenant insert must land in your own partition), removing the most common hand-written footgun.

The raw `.policy(...)` / `.policy_with_check(...)` methods stay for genuinely bespoke SQL (Postgres RLS can express things no builder will, per the "backend-specific features the ORM doesn't model" exception in CLAUDE.md). The builders are the paved path; raw is the escape hatch.

## Lint for raw policies

A boot-time and CLI lint over every raw (string) policy body, since those keep the verbatim-interpolation risk:

- **`umbral rls lint`** parses each raw `using` / `with_check` with a Postgres-grammar-aware check (sqlparser) and flags:
  - a body that references `current_setting(...)` without the `NULLIF(..., '')` guard (the anonymous-request 500 footgun);
  - a `FOR INSERT` / `FOR UPDATE` policy with no `WITH CHECK` where the `USING` clause references the owner column (likely under-constrained writes);
  - identifiers that do not resolve against any `ModelMeta` (typo'd column, or a column renamed by a migration without the policy following);
  - any body containing a comment, semicolon, or statement terminator, which is a shape a user-sourced string would take (the injection tripwire).
- Lints are warnings by default and can be promoted to boot-fail under `EnterprisePreset` (gaps5 #3), consistent with the "fail boot, not prod" posture.
- Builder-generated policies are exempt from the injection lints (they are constructed, not parsed from strings) but still get the column-existence check.

The lint is honest about scope: it is a linter, not a proof. It catches the shapes that have actually bitten (unguarded `current_setting`, missing `WITH CHECK`, stale columns), not arbitrary semantic errors.

## Simulation harness

RLS bugs are hard to catch because the DB silently returns fewer or more rows. A simulation harness makes RLS decisions testable and dry-runnable **without applying policies to a shared database**:

- **`RlsSim`** spins the declared policies into a scratch schema (Postgres test container, or a transaction rolled back at the end), seeds a handful of rows across tenants/owners, sets `app.user_id` / `app.tenant_id` to a simulated subject, and asserts which rows are visible and which writes are accepted:

```rust
RlsSim::for_plugin(&rls_plugin)
    .seed("post", [row(id=1, author_id=7), row(id=2, author_id=9)])
    .as_session_var("app.user_id", "7")
    .expect_visible("post", [1])          // owner policy hides row 2
    .expect_insert_rejected("post", row(author_id=9));  // WITH CHECK blocks cross-owner write
```

- **`umbral rls simulate --as user:7`** runs the same thing from the CLI against an ephemeral schema and prints the visible-row / accepted-write matrix, plus a diff versus the previous policy set (so a policy edit's effect is visible before it ships). This is the RLS analogue of Part 1's `explain`.
- Because RLS is Postgres-only, the harness requires a Postgres test backend and says so loudly on SQLite (skip-with-warn), rather than pretending to simulate isolation SQLite does not provide.

The simulation harness is also what Part 1's admin guardrail calls when it "simulates every policy write against canary requests": for RLS-lowered policies, the canaries run through `RlsSim`.

---

## Phasing (honest about what lands when)

This is a large surface. Sequenced so each phase is independently useful and nothing later is a rewrite of something earlier:

- **Phase 0 (small, ships first): typed RLS builders + lint** (`RlsPolicy::owner/team/tenant`, `umbral rls lint`). Pure addition to `umbral-rls`, no new plugin, no schema. Immediately removes the most common hand-written-SQL footguns. This is the concrete half of gaps5 #17.
- **Phase 1: the RLS simulation harness** (`RlsSim`, `umbral rls simulate`). Postgres-test-backed; the rest of #17.
- **Phase 2: `umbral-policy` core**: the `Policy` / `Expr` types, `decide`, `explain`, the policy store + `policy_revision` audit table, boot-time validation against `ModelMeta`. In-process enforcement only (handler guards + storage/realtime gates via `decide`). This is the usable core of gaps5 #16 and the gaps5 #38 seam.
- **Phase 3: the compilers**: `as_queryset_filter` (REST list scoping) and RLS lowering (reusing Phase 0's builders). This is where "one policy graph drives REST scopes, RLS, and storage gates" becomes real.
- **Phase 4: tenant role templates + admin editing with guardrails + `authz_decision_log`**. The organization-delegation surface, gated on the admin custom-view work already shipped.

Phases 0 and 1 are worth doing regardless of whether the full policy engine is ever built, which is why they lead. Phase 2 is the point of no return on the ABAC bet; it should not start until the north-star Stage 2 posture (EnterprisePreset, gaps5 #3) is landing, since that is who this is for.

## Everything composes as plugins

Consistent with the one idea that matters most: `umbral-policy` is a plugin structurally identical to a third-party one. It depends only on the `umbral` facade (and, at runtime, calls `umbral-permissions`' free functions and `umbral-rls`' builders through their public surfaces). `umbral-core` names none of it. An app that wants none of this compiles and runs with zero policy code; an app that wants only typed RLS builders takes Phase 0 without the policy engine; an app that wants the full ABAC layer adds `PolicyPlugin` and gets REST scoping, RLS lowering, dry-run, templates, and audit from one registration. If any of this cannot be expressed as a plugin, the plugin contract is wrong, not this design.

## Open questions for the maintainer

1. **Where does `Expr` live?** It has to be nameable by `umbral-policy` and lowerable by `umbral-rls`. Either `umbral-core` owns a minimal predicate-tree type both depend on, or `umbral-rls` grows the builders and `umbral-policy` depends on `umbral-rls`. The latter keeps core clean and is preferred, at the cost of `umbral-policy` depending on a sibling plugin (allowed: plugins may depend on plugins, only core may not depend on plugins).
2. **Decision-log volume.** Even sampled, `authz_decision_log` can be heavy. Default off, always-on-for-deny, or off entirely with an opt-in hook to the app's own observability (gaps5 #64 metrics)?
3. **How much of `Expr` must lower to RLS?** The owner/tenant/team/resource-attr subset is clearly lowerable. Context-dependent predicates (time, IP, MFA) cannot be RLS and stay app-layer. The design reports the split rather than silently dropping; is a boot-time error on a "must be RLS-enforced" policy that cannot lower the right strictness?
