# umbra — motto & hero copy

Date: 2026-06-10. Companion to `competitive-positioning.md`. The reasoning here is grounded in the audit in this folder (the modularity claim is verified, not aspirational) and the competitor sweep.

## The strategic core

Django, cot, and loco all compete on the same axis — **developer ease**:
- Django: *"The web framework for perfectionists with deadlines."*
- cot.rs: *"The Rust web framework for lazy developers."*
- loco.rs: *"Productivity-first… the one-person framework."*

That axis is crowded, and on it umbra is the fourth voice saying a version of *fast and pleasant* — and it would lose the fight on maturity (cot ships, loco is mature).

umbra's genuinely unique, audit-verified truth lives on a **different axis: radical modularity.** Auth, sessions, admin, tasks, and REST are all plugins, registered through the *same trait a third party uses*, enforced by Cargo's ban on circular crate dependencies. No competitor can claim this without re-architecting. **The motto should plant a flag on the axis umbra already owns — not fight on ease.**

## Recommended motto

> ## Every feature is a plugin. Even ours.

Why this is the line:

1. **It's true and verified.** The audit confirmed the framework cannot structurally distinguish its own auth plugin from yours. The motto is a fact, not a promise — the opposite of a claim like "secure by default" that the audit showed umbra can't yet substantiate.
2. **"Even ours" does enormous work in two words** — it carries the no-privileged-core bet, the dogfooding discipline, and a note of confident humility all at once.
3. **It survives a rename.** "umbra" is a placeholder (per CLAUDE.md); a motto built on shadow/Latin wordplay dies the day the tree is `sed`-renamed. This one is anchored to the architecture, not the word.
4. **It competes on the open lane.** Ease is taken; modularity is not. It's also the one wedge umbra's code actually backs up.

**The caveat that makes it the right line:** a motto is a promise you must keep shipping. "Every feature is a plugin, even ours" raises the bar — the day a built-in quietly bypasses the Plugin trait for a shortcut, the motto becomes a lie the next developer notices. It's the right line *because* it's demanding: it keeps the architecture honest.

## Alternatives by flavor

| Flavor | Line | When to use it |
|---|---|---|
| Provocative / minimal | **"There is no core."** | Maximum intrigue; makes people click to find out what you mean. Riskier — needs the subline to land. |
| The contract angle | **"One trait away from anything."** | Leans technical; speaks to Rust devs who'll appreciate the Plugin-trait seam. |
| Identity / who-it's-for | **"For builders who refuse to inherit someone else's monolith."** | If you'd rather name the audience (Django/Rails refugees who hit the wall of an un-swappable core). |

## Why not the others

- **Ease/speed lines** ("fast", "productive", "for X developers") are taken, and umbra loses that fight on maturity today.
- **"Django for Rust"** is literally cot's slot.
- **Shadow/umbra puns** are clever but fragile (placeholder name) and inward-looking.

Modularity is the open lane, and the only one the code can survive being fact-checked on.

---

## Hero section variations

Each is headline + subline + three proof-points. The proof-points are grounded in real, in-tree capabilities (some still need the round-one fixes before they're marketing-ready — flagged where relevant).

### Variation 1 — Modularity (recommended)

> # Every feature is a plugin. Even ours.
> A Rust web framework with no privileged core. Auth, the admin, the API layer — all plugins through the same contract your code uses. Swap, extend, or replace any of it.

- **No special-cased internals.** `AuthPlugin` registers exactly like the plugin you'll write tomorrow — the framework can't tell them apart.
- **Enforced by the compiler.** The core crate cannot depend on a plugin; Cargo's circular-dependency ban makes "serializers are a plugin" a structural fact, not a guideline.
- **Batteries when you want them, gone when you don't.** A REST-free app compiles with zero serializer code in the binary.

### Variation 2 — Provocative / minimal

> # There is no core.
> umbra is a Rust web framework that dissolved its own center. Every capability — auth, sessions, admin, background tasks, REST — is a plugin you could have written, registered through one trait. Replace any piece without forking.

- **One seam, infinite surface.** A single `Plugin` trait contributes models, routes, middleware, commands, settings, and admin registrations.
- **Your code and ours, indistinguishable.** Built-ins earn no privileges yours can't.
- **Declare a model, get the rest.** Migrations, CRUD, an admin, and an optional API follow from the type — with Rust's compile-time guarantees.

### Variation 3 — The contract angle (technical audience)

> # One trait away from anything.
> A batteries-included Rust web framework where every batterie is a plugin. Implement one trait and contribute models, routes, middleware, commands, settings, and admin pages — the same way auth, sessions, and the admin already do.

- **Dependency inversion all the way down.** Dependencies point inward to the core; control flows outward through the trait object. The framework names no concrete plugin.
- **Managed migrations from day one.** Change a type → autodetected migration → `migrate`. The declare-and-migrate loop *is* the product.
- **Postgres-first, type-checked.** Nullable columns are `Option<T>`; errors are `Result`; SQL is always parameterized.

### Variation 4 — Identity / who-it's-for

> # For builders who refuse to inherit someone else's monolith.
> umbra gives you Django's shape — models, migrations, an admin, a real REST framework, a task queue — on Rust, with one difference that changes everything: nothing is built-in. Everything is a plugin, including the parts we wrote.

- **Outgrow nothing.** When you need to replace the admin, the auth, or the API layer, there's no privileged core fighting you — just another plugin.
- **More batteries than a starter kit.** Auth, permissions, sessions, admin with dashboards, DRF-style REST, OpenAPI, a background task queue, email, cache, media. *(Harden tasks-queue correctness before leading with it — see broken-features.md.)*
- **Safety the compiler enforces, not the docs.** argon2 hashing, template autoescaping, always-parameterized SQL.

### Variation 5 — Hybrid for Django refugees (leads familiar, pivots to the wedge)

> # The framework that feels like Django — and gets out of your way completely.
> Declare your data and get migrations, an admin, CRUD, and an optional API almost for free. Then discover the part Django never gave you: every one of those features is a plugin you can swap, on a core that holds no special privileges.

- **The loop you already know.** Declare → migrate → change → migrate, generated and reversible, from the first model.
- **The extensibility you always wanted.** No `contrib` tier above your code — auth and admin are peers of your plugins.
- **Rust underneath.** Compile-time guarantees, Postgres-first, axum-fast, secure primitives by default.

---

## Notes for using these

- **Pick one motto and commit** — fragmenting across taglines weakens the flag-plant. Variation 1's headline is the recommendation; the others are A/B candidates.
- **Two proof-points in every variation depend on closing round-one gaps** to be honest in public: the security-defaults theme (so "secure primitives by default" reads as true end-to-end) and the umbra-tasks correctness bugs (before the task queue headlines a bullet). The motto raises the bar; the backlog is how you clear it. See `competitive-positioning.md` → "the engineering backlog *is* the marketing strategy."
