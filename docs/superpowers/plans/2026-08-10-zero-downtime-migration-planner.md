# Zero-downtime migration planner (`umbral plan`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `umbral plan`, a command that sequences a pending migration's operations into expand/backfill/switch/contract deploy phases, prints the plan, and offers a CI gate — plus a user-facing guide.

**Architecture:** A pure core module (`migrate_plan.rs`) assigns each `Operation` a `Phase` (reusing the existing `classify_operation`/`ClassifiedOp`/`check_pending_safety_in` machinery in `migrate.rs`), groups the pending ops into an ordered `ZdmPlan`, and flags whether the set is a single safe rolling deploy. The CLI renders that plan and gates on it. No new analysis — it sequences what the engine already classifies.

**Tech Stack:** Rust, `clap` (CLI), `tokio` (async wrappers), `sqlx` (only via existing helpers). Docs in MDX (Specra).

## Global Constraints

- Core planner types live in `crates/umbral-core/src/migrate_plan.rs` (a sibling `pub mod`, like `migrate` and `check`); do NOT convert `migrate.rs` into a directory.
- Everything a plugin author needs is re-exported through the facade `umbral::migrate` (crates/umbral/src/lib.rs) — new public types get a re-export there.
- `phase_of` MUST stay consistent with `classify_operation`: anything it puts in Expand/Backfill is `OpSafety::Safe` or a benign `Warning`; Switch/Contract are `Warning`/`Unsafe`.
- The planner is advisory. It never gates `migrate`; only `umbral plan --check` returns non-zero.
- Before each commit: `cargo fmt`, `cargo clippy --all-targets`, `cargo build`, `cargo test` (workspace) must pass.
- Commit message form: `<type>(<scope>): <summary>`, imperative, ≤72 chars; end body with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.

---

## File Structure

- **Create** `crates/umbral-core/src/migrate_plan.rs` — `Phase`, `phase_of`, `PhasedOp`, `ZdmPlan` (with `from_classified`, `by_phase`), and async `plan_pending`/`plan_pending_in`. Pure logic + one thin async wrapper. Owns its unit tests.
- **Modify** `crates/umbral-core/src/lib.rs` — add `pub mod migrate_plan;` (alphabetically near `pub mod migrate;`).
- **Modify** `crates/umbral/src/lib.rs` — re-export the planner types inside `pub mod migrate { ... }`.
- **Modify** `crates/umbral-cli/src/lib.rs` — add the `Plan` subcommand, the pure `render_plan`/`gate_blocks` helpers, the async `plan()`, dispatch, and CLI unit tests.
- **Create** `documentation/docs/v0.0.1/deployment/zero-downtime-migrations.mdx` — the guide.
- **Modify** `documentation/docs/v0.0.1/migrations/adding-not-null-columns.mdx` and `.../deployment/migrations-in-production.mdx` — one cross-link line each.

---

## Task 1: Core planner module

**Files:**
- Create: `crates/umbral-core/src/migrate_plan.rs`
- Modify: `crates/umbral-core/src/lib.rs` (add `pub mod migrate_plan;`)
- Modify: `crates/umbral/src/lib.rs` (facade re-export)
- Test: inline `#[cfg(test)] mod tests` in `migrate_plan.rs`

**Interfaces:**
- Consumes (from `crate::migrate`, all already `pub`): `Operation`, `Column` (impl `Default`), `ClassifiedOp { plugin: String, migration: String, op: Operation, safety: OpSafety }`, `OpSafety`, `MigrateError`, `MIGRATIONS_DIR: &str`, `async fn check_pending_safety_in(dir: &Path) -> Result<Vec<ClassifiedOp>, MigrateError>`.
- Produces: `enum Phase { Expand, Backfill, Switch, Contract }` (+ `.order() -> u8`, `.label() -> &'static str`); `fn phase_of(op: &Operation) -> Phase`; `struct PhasedOp { classified: ClassifiedOp, phase: Phase }`; `struct ZdmPlan { ops: Vec<PhasedOp>, single_safe_deploy: bool }` (+ `fn from_classified(Vec<ClassifiedOp>) -> ZdmPlan`, `fn by_phase(&self) -> [Vec<&PhasedOp>; 4]`); `async fn plan_pending_in(dir: &Path) -> Result<ZdmPlan, MigrateError>`; `async fn plan_pending() -> Result<ZdmPlan, MigrateError>`.

- [ ] **Step 1: Declare the module and create the file with a stub + first test**

Add to `crates/umbral-core/src/lib.rs` next to `pub mod migrate;`:

```rust
pub mod migrate_plan;
```

Create `crates/umbral-core/src/migrate_plan.rs`:

```rust
//! Zero-downtime migration planner. Sequences a pending migration's
//! operations into expand → backfill → switch → contract deploy phases so a
//! developer can roll a risky schema change out without a maintenance window.
//!
//! Pure layer over `migrate`: it reuses `classify_operation`'s tiers via
//! `check_pending_safety_in` and only assigns each op a deploy phase. Advisory
//! — nothing here applies or gates a migration.

use std::path::Path;

use crate::migrate::{
    check_pending_safety_in, ClassifiedOp, MigrateError, Operation, MIGRATIONS_DIR,
};

/// The deploy phase an operation belongs to in an expand/contract rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Additive; safe while old code still runs.
    Expand,
    /// Data migrations that populate what Expand added.
    Backfill,
    /// Needs a code deploy (old code must stop using the old shape) or data pre-cleaning.
    Switch,
    /// Destructive; safe only after no running code references the surface.
    Contract,
}

impl Phase {
    /// 1..=4 — deploy order.
    pub fn order(&self) -> u8 {
        match self {
            Phase::Expand => 1,
            Phase::Backfill => 2,
            Phase::Switch => 3,
            Phase::Contract => 4,
        }
    }

    /// Lowercase name for display.
    pub fn label(&self) -> &'static str {
        match self {
            Phase::Expand => "expand",
            Phase::Backfill => "backfill",
            Phase::Switch => "switch",
            Phase::Contract => "contract",
        }
    }
}

/// Which deploy phase an operation belongs to. Pure — derived from the op
/// type (and, for `AddColumn`, its nullability/default). Stays consistent
/// with `classify_operation`.
pub fn phase_of(_op: &Operation) -> Phase {
    // Stub — replaced in Step 3.
    Phase::Expand
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate::Column;

    fn addcol(nullable: bool, default: &str) -> Operation {
        Operation::AddColumn {
            table: "post".into(),
            column: Column {
                name: "slug".into(),
                nullable,
                default: default.into(),
                ..Column::default()
            },
        }
    }

    #[test]
    fn phase_of_maps_each_op_kind() {
        // Expand: additive.
        assert_eq!(phase_of(&addcol(true, "")), Phase::Expand);
        assert_eq!(phase_of(&addcol(false, "'draft'")), Phase::Expand);
        assert_eq!(
            phase_of(&Operation::AddIndex {
                table: "post".into(),
                columns: vec!["slug".into()],
                unique: false,
            }),
            Phase::Expand
        );
        // Backfill: data migration.
        assert_eq!(
            phase_of(&Operation::RunSql {
                sql: "UPDATE post SET url_slug = slug".into(),
                reverse_sql: None,
                shared: false,
            }),
            Phase::Backfill
        );
        // Switch: needs a code deploy / pre-clean.
        assert_eq!(phase_of(&addcol(false, "")), Phase::Switch); // NOT NULL, no default
        assert_eq!(
            phase_of(&Operation::RenameTable { from: "post".into(), to: "article".into() }),
            Phase::Switch
        );
        assert_eq!(
            phase_of(&Operation::AddIndex {
                table: "post".into(),
                columns: vec!["email".into()],
                unique: true,
            }),
            Phase::Switch
        );
        // Contract: destructive.
        assert_eq!(
            phase_of(&Operation::DropColumn { table: "post".into(), column: "slug".into() }),
            Phase::Contract
        );
        assert_eq!(
            phase_of(&Operation::DropTable { table: "post".into() }),
            Phase::Contract
        );
    }
}
```

> Note: match the exact `RunSql` / `AddIndex` field names by glancing at `Operation` in `crates/umbral-core/src/migrate.rs` (search `RunSql {` and `AddIndex {`); the shapes above mirror the current definitions. Fix field names if they differ.

- [ ] **Step 2: Run the test — expect failure**

Run: `cargo test -p umbral-core migrate_plan::tests::phase_of_maps_each_op_kind`
Expected: FAIL — the stub returns `Phase::Expand` for every op, so the Backfill/Switch/Contract asserts fail.

- [ ] **Step 3: Implement `phase_of`**

Replace the stub body:

```rust
pub fn phase_of(op: &Operation) -> Phase {
    match op {
        Operation::CreateTable { .. }
        | Operation::CreateM2MTable { .. }
        | Operation::CreateView { .. }
        | Operation::SetColumnComment { .. }
        | Operation::AddIndex { unique: false, .. }
        | Operation::DropIndex { .. } => Phase::Expand,

        // Additive unless it's NOT NULL with no default — then old code
        // inserting without it fails, so it needs the phased add/backfill/tighten.
        Operation::AddColumn { column, .. } => {
            if !column.nullable && column.default.is_empty() {
                Phase::Switch
            } else {
                Phase::Expand
            }
        }

        Operation::RunSql { .. } => Phase::Backfill,

        Operation::RenameColumn { .. }
        | Operation::RenameTable { .. }
        | Operation::AlterColumn { .. }
        | Operation::AddIndex { unique: true, .. } => Phase::Switch,

        Operation::DropColumn { .. }
        | Operation::DropTable { .. }
        | Operation::DropM2MTable { .. }
        | Operation::DropView { .. } => Phase::Contract,
    }
}
```

- [ ] **Step 4: Run the test — expect pass**

Run: `cargo test -p umbral-core migrate_plan::tests::phase_of_maps_each_op_kind`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -p umbral-core
git add crates/umbral-core/src/migrate_plan.rs crates/umbral-core/src/lib.rs
git commit -m "feat(migrate): phase_of classifies ops into deploy phases

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

- [ ] **Step 6: Write the failing test for `ZdmPlan::from_classified`**

Append to the `tests` module:

```rust
fn classified(op: Operation, safety: crate::migrate::OpSafety) -> ClassifiedOp {
    ClassifiedOp { plugin: "blog".into(), migration: "0007".into(), op, safety }
}

#[test]
fn plan_groups_orders_and_flags_single_safe_deploy() {
    use crate::migrate::OpSafety;
    // A drop (contract) declared BEFORE an add (expand) in source order.
    let plan = ZdmPlan::from_classified(vec![
        classified(
            Operation::DropColumn { table: "post".into(), column: "slug".into() },
            OpSafety::Unsafe("drops".into()),
        ),
        classified(addcol(true, ""), OpSafety::Safe),
    ]);
    // Ordered by phase: expand(1) before contract(4).
    assert_eq!(plan.ops[0].phase, Phase::Expand);
    assert_eq!(plan.ops[1].phase, Phase::Contract);
    // A contract op is present → not a single safe deploy.
    assert!(!plan.single_safe_deploy);
    // by_phase buckets: expand has 1, contract has 1, others empty.
    let groups = plan.by_phase();
    assert_eq!(groups[0].len(), 1); // expand
    assert_eq!(groups[3].len(), 1); // contract

    // All-additive set → single safe deploy.
    let safe = ZdmPlan::from_classified(vec![classified(addcol(true, ""), OpSafety::Safe)]);
    assert!(safe.single_safe_deploy);
}
```

- [ ] **Step 7: Run it — expect failure**

Run: `cargo test -p umbral-core migrate_plan::tests::plan_groups_orders_and_flags_single_safe_deploy`
Expected: FAIL to compile — `PhasedOp`/`ZdmPlan` don't exist yet.

- [ ] **Step 8: Implement `PhasedOp` and `ZdmPlan`**

Add above the `tests` module:

```rust
/// A classified operation with the deploy phase it belongs to.
#[derive(Debug, Clone)]
pub struct PhasedOp {
    pub classified: ClassifiedOp,
    pub phase: Phase,
}

/// The sequenced plan for a set of pending operations.
#[derive(Debug, Clone)]
pub struct ZdmPlan {
    /// Ops in (phase, then source) order.
    pub ops: Vec<PhasedOp>,
    /// True iff every op is Expand/Backfill — the set is one safe rolling deploy.
    pub single_safe_deploy: bool,
}

impl ZdmPlan {
    /// Assign each classified op a phase, order by phase (stable within a
    /// phase, preserving source order), and compute `single_safe_deploy`.
    pub fn from_classified(classified: Vec<ClassifiedOp>) -> ZdmPlan {
        let mut ops: Vec<PhasedOp> = classified
            .into_iter()
            .map(|c| {
                let phase = phase_of(&c.op);
                PhasedOp { classified: c, phase }
            })
            .collect();
        ops.sort_by_key(|p| p.phase.order()); // Vec::sort_by_key is stable
        let single_safe_deploy = ops
            .iter()
            .all(|p| matches!(p.phase, Phase::Expand | Phase::Backfill));
        ZdmPlan { ops, single_safe_deploy }
    }

    /// Group the ops into the four phase buckets, index 0..=3 = phase 1..=4.
    pub fn by_phase(&self) -> [Vec<&PhasedOp>; 4] {
        let mut out: [Vec<&PhasedOp>; 4] = Default::default();
        for p in &self.ops {
            out[(p.phase.order() - 1) as usize].push(p);
        }
        out
    }
}
```

- [ ] **Step 9: Run it — expect pass**

Run: `cargo test -p umbral-core migrate_plan`
Expected: PASS (both tests).

- [ ] **Step 10: Add the async wrappers**

Add below the `impl ZdmPlan` block:

```rust
/// Build the zero-downtime plan for the pending migrations under `dir`.
/// Reuses `check_pending_safety_in` (same applied-set + drift the CLI uses).
pub async fn plan_pending_in(dir: &Path) -> Result<ZdmPlan, MigrateError> {
    let classified = check_pending_safety_in(dir).await?;
    Ok(ZdmPlan::from_classified(classified))
}

/// [`plan_pending_in`] against the default migrations directory.
pub async fn plan_pending() -> Result<ZdmPlan, MigrateError> {
    plan_pending_in(Path::new(MIGRATIONS_DIR)).await
}
```

(The pure `from_classified` carries the logic; these two-line wrappers are exercised end-to-end by the CLI unit tests in Task 2, so they need no dedicated DB-fixture test here.)

- [ ] **Step 11: Re-export through the facade**

In `crates/umbral/src/lib.rs`, inside `pub mod migrate { ... }`, add after the existing `pub use umbral_core::migrate::{ ... };` block:

```rust
    pub use umbral_core::migrate_plan::{
        phase_of, plan_pending, plan_pending_in, Phase, PhasedOp, ZdmPlan,
    };
```

- [ ] **Step 12: Build, test, commit**

Run: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test -p umbral-core migrate_plan`
Expected: all PASS.

```bash
git add crates/umbral-core/src/migrate_plan.rs crates/umbral/src/lib.rs
git commit -m "feat(migrate): ZdmPlan sequences pending ops into deploy phases

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: `umbral plan` CLI command

**Files:**
- Modify: `crates/umbral-cli/src/lib.rs` (add `Plan` variant to `enum Command`, pure `render_plan`/`gate_blocks`, async `plan()`, dispatch arm, tests)
- Test: inline `#[cfg(test)]` in `crates/umbral-cli/src/lib.rs`

**Interfaces:**
- Consumes: `umbral::migrate::{plan_pending, ZdmPlan, Phase, PhasedOp}`; the existing `op_kind(&Operation) -> &str` helper in this file (search `fn op_kind`).
- Produces: `async fn plan(check: bool, acknowledge: bool, json: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>>`; pure `fn render_plan(plan: &ZdmPlan) -> String`; pure `fn gate_blocks(plan: &ZdmPlan, check: bool, acknowledge: bool) -> bool`.

- [ ] **Step 1: Write the failing test for the pure gate + render helpers**

Add to the CLI test module (near the existing `WorkerCmd` test, or a new `#[cfg(test)] mod plan_tests`):

```rust
#[cfg(test)]
mod plan_tests {
    use super::*;
    use umbral::migrate::{ClassifiedOp, OpSafety, Operation, ZdmPlan};

    fn plan_with_contract() -> ZdmPlan {
        ZdmPlan::from_classified(vec![ClassifiedOp {
            plugin: "blog".into(),
            migration: "0007".into(),
            op: Operation::DropColumn { table: "post".into(), column: "slug".into() },
            safety: OpSafety::Unsafe("drops column".into()),
        }])
    }

    #[test]
    fn gate_blocks_on_phased_unless_acknowledged() {
        let p = plan_with_contract();
        assert!(gate_blocks(&p, true, false)); // --check, not acknowledged → block
        assert!(!gate_blocks(&p, true, true)); // --check --acknowledge → pass
        assert!(!gate_blocks(&p, false, false)); // no --check → never blocks
    }

    #[test]
    fn render_names_the_phase_and_op() {
        let out = render_plan(&plan_with_contract());
        assert!(out.contains("contract"));
        assert!(out.contains("post"));
        assert!(out.contains("Not a single safe deploy"));
    }
}
```

- [ ] **Step 2: Run it — expect failure**

Run: `cargo test -p umbral-cli plan_tests`
Expected: FAIL to compile — `gate_blocks` / `render_plan` don't exist.

- [ ] **Step 3: Implement the pure helpers**

Add near the other free functions in `crates/umbral-cli/src/lib.rs`:

```rust
const PHASE_LABELS: [&str; 4] = ["expand", "backfill", "switch", "contract"];

/// Render the phased plan as human-readable text.
fn render_plan(plan: &umbral::migrate::ZdmPlan) -> String {
    use std::collections::BTreeSet;
    let mut s = String::new();
    let migrations: BTreeSet<_> = plan
        .ops
        .iter()
        .map(|p| (&p.classified.plugin, &p.classified.migration))
        .collect();
    s.push_str(&format!(
        "Zero-downtime plan for {} pending migration(s):\n\n",
        migrations.len()
    ));
    for (i, group) in plan.by_phase().iter().enumerate() {
        if group.is_empty() {
            continue;
        }
        s.push_str(&format!("  Deploy {} · {}\n", i + 1, PHASE_LABELS[i]));
        for p in group {
            let reason = p.classified.safety.reason();
            let note = if reason.is_empty() { "safe — additive" } else { reason };
            s.push_str(&format!(
                "    [{}] {}/{} — {}\n",
                op_kind(&p.classified.op),
                p.classified.plugin,
                p.classified.migration,
                note
            ));
        }
    }
    s.push('\n');
    if plan.single_safe_deploy {
        s.push_str("✓ Single safe rolling deploy — no phasing needed.\n");
    } else {
        s.push_str("✗ Not a single safe deploy — split across the releases above.\n");
    }
    s
}

/// CI gate: blocks when `--check` is set, the plan isn't a single safe deploy,
/// and the operator hasn't `--acknowledge`d a phased deploy.
fn gate_blocks(plan: &umbral::migrate::ZdmPlan, check: bool, acknowledge: bool) -> bool {
    check && !plan.single_safe_deploy && !acknowledge
}
```

- [ ] **Step 4: Run it — expect pass**

Run: `cargo test -p umbral-cli plan_tests`
Expected: PASS.

- [ ] **Step 5: Add the async `plan()` entry point**

Add near `async fn checkmigrations`:

```rust
async fn plan(
    check: bool,
    acknowledge: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let plan = umbral::migrate::plan_pending().await?;
    if plan.ops.is_empty() {
        println!("No pending migrations — nothing to plan.");
        return Ok(());
    }
    if json {
        let phases: Vec<_> = plan
            .ops
            .iter()
            .map(|p| {
                serde_json::json!({
                    "phase": p.phase.label(),
                    "op": op_kind(&p.classified.op),
                    "plugin": p.classified.plugin,
                    "migration": p.classified.migration,
                    "note": p.classified.safety.reason(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "single_safe_deploy": plan.single_safe_deploy,
                "ops": phases,
            })
        );
    } else {
        print!("{}", render_plan(&plan));
    }
    if gate_blocks(&plan, check, acknowledge) {
        return Err("umbral plan --check: pending migrations need a phased rollout (see the plan above). Split them across releases, or pass --acknowledge for the deploy that intends a later phase.".into());
    }
    if check && acknowledge && !plan.single_safe_deploy {
        println!("(acknowledged — proceeding despite phased ops)");
    }
    Ok(())
}
```

- [ ] **Step 6: Add the `Plan` subcommand and dispatch**

In `enum Command`, add (near `Checkmigrations`):

```rust
    /// Sequence pending migrations into expand/backfill/switch/contract deploy
    /// phases for a zero-downtime rollout. Advisory — never gates `migrate`.
    Plan {
        /// Exit non-zero when the pending set isn't a single safe rolling
        /// deploy (contains switch/contract ops). A CI gate.
        #[arg(long, default_value_t = false)]
        check: bool,
        /// With --check, exit zero anyway — for the deploy that intends a
        /// later phase (e.g. the contract migration after the switch shipped).
        #[arg(long, default_value_t = false)]
        acknowledge: bool,
        /// Emit the plan as JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
```

In the `match` that dispatches commands (near `Command::Checkmigrations { strict } => checkmigrations(strict).await,`):

```rust
        Command::Plan { check, acknowledge, json } => plan(check, acknowledge, json).await,
```

- [ ] **Step 7: Build, test, commit**

Run: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test -p umbral-cli`
Expected: all PASS. Sanity check the surface: `cargo run -p umbral-cli -- plan --help` shows the flags.

```bash
git add crates/umbral-cli/src/lib.rs
git commit -m "feat(cli): add umbral plan (zero-downtime phase plan + --check gate)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: User-facing guide

**Files:**
- Create: `documentation/docs/v0.0.1/deployment/zero-downtime-migrations.mdx`
- Modify: `documentation/docs/v0.0.1/migrations/adding-not-null-columns.mdx` (one cross-link line)
- Modify: `documentation/docs/v0.0.1/deployment/migrations-in-production.mdx` (one cross-link line)

**Interfaces:** none (docs). Depends on Tasks 1–2 for the `umbral plan` surface it documents.

- [ ] **Step 1: Write the guide**

Create `documentation/docs/v0.0.1/deployment/zero-downtime-migrations.mdx` (no hard-wrapping of prose per repo convention):

````mdx
---
title: Zero-downtime migrations
description: Roll risky schema changes out without a maintenance window using the expand/contract pattern, and let `umbral plan` sequence the phases for you.
sidebar_position: 2
tags: [deployment, migrations, zero-downtime, expand-contract]
---

# Zero-downtime migrations

A schema change is safe under a rolling deploy only when old and new code can both run against the database at once. Anything else — dropping a column, renaming one, tightening a type — needs to become a *sequence* of individually-safe migrations shipped across releases. That sequence is the **expand → backfill → switch → contract** pattern, and `umbral plan` sorts your pending changes into it.

## The four phases

<Steps>
<Step title="Expand — add the new shape">
Additive changes only: new tables, new **nullable** (or defaulted) columns, non-unique indexes. Old code ignores them; new code can start writing them.
</Step>
<Step title="Backfill — fill the new shape">
A `RunSql` data migration copies existing data into the new columns/tables. Make it idempotent so a re-run mid-rollout is harmless.
</Step>
<Step title="Switch — move reads/writes over">
Deploy the code that reads the new shape and stops using the old one. Renames, type changes, and NOT NULL tightening happen here — each needs old code gone first.
</Step>
<Step title="Contract — drop the old shape">
Only after no running instance references it: drop the old column/table. Irreversible, so it's always the last, separate release.
</Step>
</Steps>

## Let `umbral plan` sequence it

```bash
umbral plan
```

```text
Zero-downtime plan for 1 pending migration(s):

  Deploy 1 · expand
    [add_column] blog/0007 — safe — additive
  Deploy 3 · switch
    [rename_column] blog/0007 — old code references `slug`; add `url_slug`, backfill, switch reads, then drop
  Deploy 4 · contract
    [drop_column] blog/0007 — drops column and its data; stop writing it, deploy, then drop

✗ Not a single safe deploy — split across the releases above.
```

Gate it in CI so a change that can't ship in one rolling deploy fails the pipeline:

```bash
umbral plan --check
```

`--check` exits non-zero whenever the pending set contains switch/contract work. For the release that *intends* a later phase (the contract migration, after the switch has shipped), acknowledge it:

```bash
umbral plan --check --acknowledge
```

<Callout type="info">
`umbral plan` is advisory — it never blocks `migrate`. It classifies; you sequence. Auto-scaffolding the phase migration files and generating dual-write code are on the roadmap (see `arch.md` and gaps5 #25).
</Callout>

## Cookbook

<Accordion title="Add a NOT NULL column">
Add it nullable (or with a default), backfill, then tighten to NOT NULL in a later migration. `makemigrations` refuses the unsafe one-shot shape — see [Adding NOT NULL columns](/docs/v0.0.1/migrations/adding-not-null-columns).
</Accordion>

<Accordion title="Rename a column">
Expand: add the new column. Backfill: copy `old → new`. Switch: deploy code reading the new column. Contract: drop the old column. (A raw `RENAME` is atomic in the DB but not with your deploy, so old code breaks in the gap.)
</Accordion>

<Accordion title="Drop a column or table">
Stop referencing it in code and deploy first; drop it in a *later* migration once no instance reads it.
</Accordion>

<Accordion title="Change a column type">
Add a new column of the target type, backfill, switch reads, then drop the old one — same shape as a rename.
</Accordion>

<Accordion title="Add a UNIQUE constraint">
De-duplicate existing rows first (a `RunSql` cleanup), then add the constraint. On Postgres, build the index with `CREATE INDEX CONCURRENTLY` in a `RunSql` op to avoid locking writes.
</Accordion>

## See also

- [Migrations in production](/docs/v0.0.1/deployment/migrations-in-production) — one-shot vs boot migrate, and the advisory lock.
- [Adding NOT NULL columns](/docs/v0.0.1/migrations/adding-not-null-columns) — the safe shapes for the most common case.
- [checkmigrations](/docs/v0.0.1/migrations/checkmigrations) — the per-operation safety linter `umbral plan` builds on.
````

- [ ] **Step 2: Add cross-links**

In `documentation/docs/v0.0.1/migrations/adding-not-null-columns.mdx`, add a line near the top (after the intro paragraph):

```mdx
> For the full multi-release pattern (rename, drop, type change), see [Zero-downtime migrations](/docs/v0.0.1/deployment/zero-downtime-migrations).
```

In `documentation/docs/v0.0.1/deployment/migrations-in-production.mdx`, add a line near the top:

```mdx
> To shape a risky change so it survives a rolling deploy, see [Zero-downtime migrations](/docs/v0.0.1/deployment/zero-downtime-migrations).
```

- [ ] **Step 3: Commit**

```bash
git add documentation/docs/v0.0.1/deployment/zero-downtime-migrations.mdx \
        documentation/docs/v0.0.1/migrations/adding-not-null-columns.mdx \
        documentation/docs/v0.0.1/deployment/migrations-in-production.mdx
git commit -m "docs: zero-downtime migrations guide + umbral plan

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: Close the loop

- [ ] **Step 1: Update gaps5 #25 to reflect the shipped planner**

In `planning/gaps5.md`, entry #25, note that the phase planner + CI gate + guide shipped (`umbral plan`), and that what remains deferred is auto-scaffolding phase migration files, dual-write codegen, and rollback/targeting (#26). Keep it a one-line-per-fact update; do not renumber.

- [ ] **Step 2: Full workspace verification**

Run: `cargo fmt && cargo clippy --all-targets && cargo build && cargo test`
Expected: all PASS.

- [ ] **Step 3: Commit**

```bash
git add planning/gaps5.md
git commit -m "docs: mark gaps5 #25 planner half shipped (umbral plan)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- Phase model → Task 1 `phase_of` (table maps 1:1 to the match arms). ✓
- Core API (`Phase`, `phase_of`, `PhasedOp`, `ZdmPlan`, `plan_pending*`) → Task 1. ✓
- Facade re-export → Task 1 Step 11. ✓
- CLI `umbral plan` + `--check` + `--acknowledge` + `--json` → Task 2. ✓
- Gate semantics (blocks on Switch/Contract unless acknowledged) → Task 2 `gate_blocks` + test. ✓
- Docs page + cross-links → Task 3. ✓
- Deferred items noted → Task 3 Callout + Task 4 gaps5 update. ✓

**Placeholder scan:** No TODO/TBD; every code step has real code. The one soft note (Step 1 "match exact field names for RunSql/AddIndex") is a verification instruction, not missing content — the shapes given mirror the current enum.

**Type consistency:** `phase_of`, `from_classified`, `by_phase`, `plan_pending`, `render_plan`, `gate_blocks` names and signatures match between where they're defined (Tasks 1–2) and used (CLI, tests). `ZdmPlan { ops, single_safe_deploy }` field names are consistent across core, tests, and CLI render.
