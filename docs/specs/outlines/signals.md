# Outline — Signals (decoupled events)

| | |
|---|---|
| **Status** | Outline. Promotes to a deep spec when built-in plugins (M8) start using signals. |
| **Maps to milestone** | M8 (entry) |
| **Companions** | `02-plugin-contract.md`, `04-orm-model-and-fields.md`, `03-orm-querysets.md`, outline `auth-and-sessions.md`, outline `tasks.md`, outline `admin.md` |

## Purpose

Signals are umbral's decoupled event bus for model lifecycle and arbitrary plugin-published events: a publisher fires a typed signal, zero-or-more subscribers react, and neither side imports the other. The motivating use case is the one Django leans on hardest — `umbral-auth` fires a `post_login` event that `umbral-sessions` rotates a session for and `umbral-admin` logs to its audit trail, with auth knowing about neither. Dispatch is **async**: signals fire mid-database-operation (a `pre_save` runs after validation and before the INSERT; a `post_save` runs immediately after the row is back), and forcing handlers to be sync would either block the executor or push every handler onto a thread pool the framework can't introspect. This closes spec-set-design open question #4 — sync-vs-async — in favor of async, and locates that decision *here* rather than in `02-plugin-contract.md` (which keeps `on_ready` sync to avoid making the trait async-flavored). The trade-off is the Django one: loose coupling at the cost of "where does this run?" being less obvious than an explicit method call. The mitigation is the same one Django landed on — keep the catalog of built-in signals small and well-named, and recommend explicit method calls when the publisher and subscriber are in the same plugin.

## Key concepts

**Built-in lifecycle signals.** Every model gets four signals fired by the QuerySet terminals from `03-orm-querysets.md`: `pre_save` (before INSERT or UPDATE, after validators), `post_save` (after the row is written and the PK populated, before the transaction commits), `pre_delete` (before DELETE), `post_delete` (after DELETE, before commit). Each carries the model instance, a `created: bool`, and the active transaction handle so a handler can read or write inside the same atomic block.

**Custom signals as typed plugin API.** A plugin declares a signal by exporting a `Signal<Payload>` constant from its public API. `umbral-auth` exports `pub static POST_LOGIN: Signal<LoginEvent>`; any plugin that wants to react does `use umbral_auth::POST_LOGIN;` and connects a handler. The payload is a plugin-owned struct, so the signal type *is* the contract — no string-keyed lookups, no dynamic dispatch on payload shape.

**Async dispatch.** A handler is an `async fn(&Payload) -> Result<()>`. The dispatcher awaits handlers in registration order; under `dispatch_concurrent` (custom signals only, opt-in) it `join_all`s them. Lifecycle signals are always sequential because they share the transaction handle.

**Connection lifecycle.** Handlers are connected inside `Plugin::on_ready(&ctx)` (the sync hook from `02-plugin-contract.md`), which keeps wiring static — every signal/handler edge exists at boot, none can appear mid-request. A connection lives for the lifetime of the process; there is no `disconnect` in the public API (the test helper has one).

```rust
fn on_ready(&self, ctx: &AppContext) -> Result<()> {
    ctx.signals().connect(POST_LOGIN, rotate_session);
    Ok(())
}

async fn rotate_session(event: &LoginEvent) -> Result<()> {
    Session::for_user(event.user_id).rotate().await
}
```

**Ordering guarantees.** Lifecycle signals dispatch in plugin topological order (same order as `on_ready`), so a handler in a dependent plugin always sees its dependency's handlers run first. Within a single plugin, handlers fire in connection (FIFO) order. No priority knob in v1; if real conflicts surface, add one.

**Error handling and transaction binding.** A failing handler on a `pre_*` or `post_*` signal aborts the originating transaction by default — the signal sits inside the same atomic block as the SQL operation. `post_save` after-commit semantics (Django's `transaction.on_commit`) is available via a `connect_after_commit` variant for handlers that should only fire if the write actually persists, which is the right hook for "enqueue a task" patterns (cross-link `tasks.md`).

## Promote-to-deep trigger

Promote at M8 entry, when `umbral-auth` needs to fire `post_login` for `umbral-sessions` and `umbral-admin` to react to — that is the first place the abstract design has to survive a real cross-plugin event. Promote earlier if any M5–M7 work needs lifecycle hooks before plugins exist.

## Open questions

- **Ordering guarantees beyond FIFO.** Plugin-topological-then-FIFO is the default; whether to expose a `priority: i32` on `connect()` for intra-plugin ordering, or to leave it implicit, is open. Decide when two handlers in the same plugin actually need a defined order.
- **Error handling default — abort vs log+continue.** Aborting the transaction on handler failure is the safe default but surprises users porting from Django (where signal errors propagate but don't roll back). Resolve by surveying which built-in handlers we ship and whether any are "advisory."
- **Transaction binding for `post_save`.** Fire inside the transaction (handler sees uncommitted state, can write atomically, but a rollback erases the side effect's trigger) or after commit (handler sees only committed state, side effects survive, but no rollback safety). Likely both, with `connect` = inside, `connect_after_commit` = after — confirm at promotion.
- **Sender filtering.** Django's `sender=Post` argument scopes a handler to one model class. Lifecycle signals in umbral are typed (`Signal<SaveEvent<Post>>`), so per-model scoping falls out of the type system; whether custom signals need a runtime `filter: Fn(&Payload) -> bool` parameter is open.
- **Concurrent dispatch for custom signals.** Sequential dispatch is the safe default; `dispatch_concurrent` (`join_all`) is appealing for fan-out custom signals but complicates error reporting. Decide once a custom signal exists with more than two handlers.

## Cross-links

- Deep specs that constrain this: `02-plugin-contract.md` (handlers connect inside the sync `on_ready` hook; signals are explicitly delegated here), `04-orm-model-and-fields.md` (lifecycle hooks fire from the `Model` save/delete path), `03-orm-querysets.md` (QuerySet terminals — `create`, `save`, `delete`, `bulk_*` — are the firing sites).
- Sibling outlines: `auth-and-sessions.md` (post-login signal, the canonical cross-plugin example), `tasks.md` (the `post_save` → `connect_after_commit` → `enqueue` pattern), `admin.md` (admin actions firing custom signals; audit-log handlers subscribing to lifecycle signals).
