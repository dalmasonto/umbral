# State machine / workflow plugin (`umbral-workflow`)

Status: draft for ratification (proposes the design for gaps5 #103; the final call is the maintainer's)
Date: 2026-08-10
Decision coverage: planning/gaps5.md #103 (tf#327). Records the accepted design target; implementation and docs remain tracked on the task.
Relates: #31 (CDC/outbox), #52 (transactional outbox / after-commit), #87 (tamper-evident audit), #20 (compliance evidence), the storage moderation-workflow note (#63 area), and `docs/decisions/2026-08-08-unified-authz-dsl-and-webhooks.md` (guards compose with the authz DSL).

## The problem

Almost every real app has an entity with a lifecycle: an order (`placed → paid → shipped → delivered`, or `cancelled`), a document under review, a KYC application, a support ticket, a publishing flow. Today umbral gives you a `status` column via `#[umbral(choices)]` and nothing else. Everything that makes a lifecycle *correct* is left to hand-rolled code scattered across handlers:

- **No enforced transitions.** Nothing stops code from setting `status = shipped` on an order that was never paid. The legal graph lives only in the developer's head.
- **No actor rules.** "Who is allowed to move this, and from which state?" is re-implemented per handler, inconsistently.
- **No audit trail.** Who advanced it, when, from what, and why is not recorded unless you remember to.
- **No multi-party gates.** "Two managers must both approve" or "finance AND legal must sign off" has no home.
- **No request-changes loop.** "Reviewer asks for edits → applicant resubmits" is a backward transition people fake with ad-hoc flags.

The result is that the single most common modelling need in line-of-business apps is the least supported. This proposes `umbral-workflow`: a declarative finite-state-machine attached to a model, enforced through one API, with history, guards, and multi-actor gates built in.

## Prior art

Django's `django-fsm` / `viewflow`, Rails' `AASM` / `state_machines`, and `XState` (statecharts) all model this; Temporal/Cadence model *durable* long-running workflows. We deliberately scope to a **per-entity flat FSM stored in the database** — the 90% case — and leave statecharts and durable orchestration as explicit non-goals for v1 (see Deferred). The bet mirrors umbral's motto: this is a plugin, structurally identical to a third-party one, that other plugins (admin, REST, permissions, outbox) integrate with through existing seams.

## The design

### 1. Declare the machine

A machine binds a model to its state column and declares states + transitions. States are an enum used as the column's `choices`; transitions are declared in a builder so guards, effects, and quorum have somewhere to live (attributes on variants get unreadable fast).

```rust
use umbral_workflow::prelude::*;

#[derive(State, Clone, Copy, PartialEq)]   // stored as the `status` choices column
pub enum OrderState { Placed, Paid, Shipped, Delivered, Cancelled }

WorkflowPlugin::new()
    .machine::<Order>("status")            // model + its state column
        .initial(OrderState::Placed)
        .transition("pay",     from![Placed],        OrderState::Paid)
            .guard(permission("order.pay"))
        .transition("ship",    from![Paid],          OrderState::Shipped)
            .guard(role("fulfilment"))
            .effect(|order, actor, ctx| ctx.emit("order.shipped", order))
        .transition("deliver", from![Shipped],       OrderState::Delivered)
        .transition("cancel",  from![Placed, Paid],  OrderState::Cancelled)
            .guard(owner_or(role("support")))
```

- `from![...]` lists the legal source states; a transition from any other state is rejected.
- `guard(...)` decides *who* may fire it — a predicate over `(entity, actor)` that composes ownership, roles, and the authz DSL. Guards are the same concept object-level REST scoping and RLS use, so they stay consistent.
- `effect(...)` runs on a successful transition (after the state write, inside the same transaction) and can `ctx.emit(...)` a domain event.

### 2. Fire a transition — the one enforced path

```rust
order.transition("ship", &actor, None).await?;   // note: Option<&str>
```

`transition` is the *only* way state changes. It:

1. loads the entity's current state,
2. finds the transition registered for `(event, current_state)` — else `Err(IllegalTransition)`,
3. runs the guards — else `Err(GuardDenied)`,
4. (multi-actor only) records the actor's approval and checks quorum; if not yet met, returns `Err(QuorumPending { needed, have })` **without** moving state,
5. writes the new state to the model's column via the ORM,
6. runs `effect`,
7. appends a **transition-history** row,
8. emits the domain event after commit,

all inside one transaction so state, history, and effects commit together or not at all. Direct writes to the `status` column bypass this and are discouraged; a system check can warn when a model has a machine but its column is written elsewhere.

### 3. Multi-actor transitions (quorum / M-of-N)

A transition can require **N approvals from distinct actors** before it fires:

```rust
.transition("approve", from![PendingReview], OrderState::Approved)
    .quorum(2, role("manager"))          // two different managers
```

Calling `transition("approve", &manager_a, ..)` records an approval and returns `QuorumPending { needed: 2, have: 1 }`; a *different* manager's call reaches quorum and the transition actually fires. Approvals are rows in `workflow_approval` with a unique `(entity, event, actor)` so one actor cannot count twice, and are cleared once the transition fires or the entity leaves the state. This is how "finance AND legal sign off" and "two admins to delete" are expressed. Distinct-role quorums (one finance **and** one legal) are a `quorum_of([role("finance"), role("legal")])` variant.

### 4. Resubmit / request-changes loops — first class

Backward transitions are just transitions; the review loop is:

```rust
.transition("request_changes", from![PendingReview],     OrderState::ChangesRequested)
    .guard(role("reviewer"))
.transition("resubmit",        from![ChangesRequested],  OrderState::PendingReview)
    .guard(owner())               // only the submitter resubmits
```

`ChangesRequested` is a real state the UI can render ("Action needed"); the owner edits the entity (including re-uploading files — ties to storage) and fires `resubmit`, which sends it back for another review pass. The history table makes the round-trips auditable.

### 5. History / audit trail

Every transition appends to `workflow_transition`: `(entity_type, entity_id, machine, event, from_state, to_state, actor_id, note, created_at)`. This is the built-in answer to "who moved it, when, and why." Optionally each row carries `prev_hash` / `hash` for a hash-chained, tamper-evident log (reuses the #87 mechanism), and the emitted event can be mirrored to the outbox (#52) for external sinks.

### 6. Integration surfaces

- **Permissions / RLS:** guards call the same permission/ownership machinery, so authorization stays in one place; RLS still scopes which entities an actor can even see.
- **Admin:** a workflow widget on the object page shows the current state, the **allowed** transitions *for this actor* as buttons (guarded ones the actor can't fire are hidden or shown disabled), a pending-quorum indicator ("1 of 2 approvals"), and the transition history as a timeline. Firing a button calls `transition`.
- **REST / GraphQL:** `GET …/{id}/transitions` returns the events the caller may fire from the current state; `POST …/{id}/transition { event, note }` fires one; GraphQL exposes an equivalent `transition` mutation. This composes with REST object-scoping (#101) and GraphQL `owned_by`.
- **Events:** a transition emits `workflow.transition` (and any `effect` events) through the after-commit/outbox path, so webhooks, tasks, email, and realtime react to state changes without the machine knowing about them.
- **Diagram:** the CLI generates a Mermaid/DOT diagram of a machine (states + transitions + guards) for docs and the admin — the same codegen seam as `umbral typegen`.

### 7. Schema (plugin-owned migrations)

The plugin owns its tables the normal way (walked by `migrate`):

- `workflow_transition` — the history/audit trail (+ optional `prev_hash`/`hash`).
- `workflow_approval` — in-flight approvals for quorum transitions, unique on `(entity_type, entity_id, event, actor_id)`.

The **state itself** lives on the model's own `status` choices column (no extra join for the common read); the plugin reads/writes it through the ORM, never raw SQL.

## Complexity tiers this must cover

- **Simple** — a 2-state toggle (`draft ⇄ published`): one transition each way, a guard on publish.
- **Standard** — a linear order lifecycle with per-transition role guards and an emitted event on ship.
- **Complex** — a review flow with `request_changes`/`resubmit` loops, a multi-actor `approve` quorum, and an **SLA/timeout** transition (e.g. `pending_review → escalated` after N days) driven by a periodic `umbral-tasks` job that fires the transition on the system's behalf.

## Naming

`umbral-workflow` (chosen). "Workflow" reads to product people and matches the admin surface; the implementation is a flat FSM. Rejected: `umbral-fsm` (jargon), `umbral-states` (collides with the `state` column noun).

## Open questions / deferred (noted, not built in v1)

- **Statecharts** — parallel regions, nested/hierarchical states, internal transitions. v1 is a flat FSM; revisit if real demand appears.
- **Timed / SLA transitions** — need `umbral-tasks`; ship the hook (a `.timeout(dur, event)` declaration) but the scanning job is a fast-follow.
- **Sagas / compensation** — cross-model, distributed, compensating transactions are out of scope; this is a per-entity FSM, not an orchestrator.
- **Retroactive machines on existing data** — attaching a machine to a populated table needs a backfill/adoption story (map existing `status` values to declared states, and a system check for values with no state).
- **Guard/authz overlap** — guards must compose with, not duplicate, the unified authz DSL (2026-08-08 decision); the exact shared predicate type is an implementation detail to settle first.
