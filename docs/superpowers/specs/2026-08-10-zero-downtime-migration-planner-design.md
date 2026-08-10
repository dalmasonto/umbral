# Zero-downtime migration planner (`umbral plan`) — design

Status: design draft for approval (gaps5 #25 / tf#238)
Date: 2026-08-10
Scope: a planner that sequences a pending migration's operations into expand/backfill/switch/contract deploy phases, prints the plan, and offers a CI gate. Plus a user-facing guide.

## The problem

umbral already refuses the single worst one-shot mistake (adding a `NOT NULL` column to a populated table) and ships a per-operation zero-downtime classifier, but it never tells the developer *how to sequence* a risky change across releases. gaps5 #25 (tf#238) tracks this: "deployment phases, dual writes, background backfills, contract/expand sequencing, and rollout gates are not orchestrated."

This lands the sequencing + gate half. It does **not** attempt to generate dual-write application code or scaffold the phase migration files (bigger follow-ups), and migration rollback/targeting is a separate gap (#26).

## What already exists (build on it, don't rebuild)

`crates/umbral-core/src/migrate.rs`:

- `enum OpSafety { Safe, Warning(String), Unsafe(String) }` — the three-tier zero-downtime classification, with expand/contract guidance already baked into the `Warning`/`Unsafe` reason strings (e.g. renames say "add `to`, backfill, switch reads, then drop `from`").
- `fn classify_operation(op: &Operation) -> OpSafety` — pure, no I/O.
- `struct ClassifiedOp { plugin, migration, op, safety }`.
- `async fn check_pending_safety_in(dir) -> Vec<ClassifiedOp>` — loads the pending migrations and classifies every op. Powers `checkmigrations`.

The planner is a thin, pure layer over these: assign each op a **phase**, group the pending ops by phase, and decide whether the pending set is a single safe rolling deploy.

## The phase model

Every operation belongs to exactly one deploy phase of an expand/contract rollout:

| Phase | Operations | Rationale |
|---|---|---|
| **1 · Expand** | CreateTable, CreateM2MTable, CreateView, SetColumnComment, AddColumn (nullable or with default), AddIndex `{unique:false}`, DropIndex | additive / removes only a guarantee — old and new code both work |
| **2 · Backfill** | RunSql | populate the new columns/tables the expand phase added |
| **3 · Switch** | RenameColumn, RenameTable, AlterColumn (type change / NOT NULL tighten), AddColumn (NOT NULL, no default), AddIndex `{unique:true}` | requires a code deploy (old code must stop using the old shape) or data pre-cleaning |
| **4 · Contract** | DropColumn, DropTable, DropM2MTable, DropView | safe only after no running code references the surface |

`phase_of` is derived purely from the operation (and, for `AddColumn`, its `nullable`/`default`), and stays consistent with `classify_operation`'s tiers: everything `phase_of` puts in Expand/Backfill is `Safe` or a benign `Warning`; everything in Switch/Contract is a `Warning`/`Unsafe` that needs sequencing.

## Core API (new: `crates/umbral-core/src/migrate/plan.rs`)

```rust
/// The deploy phase an operation belongs to in an expand/contract rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase { Expand, Backfill, Switch, Contract }

impl Phase {
    pub fn label(&self) -> &'static str;   // "expand" | "backfill" | "switch" | "contract"
    pub fn order(&self) -> u8;             // 1..=4, for sorting
}

/// Which phase this operation belongs to. Pure — derived from the op type
/// (and AddColumn's nullable/default). Consistent with classify_operation.
pub fn phase_of(op: &Operation) -> Phase;

/// A classified op with its deploy phase.
pub struct PhasedOp { pub classified: ClassifiedOp, pub phase: Phase }

/// The sequenced plan for the pending migration set.
pub struct ZdmPlan {
    pub ops: Vec<PhasedOp>,          // in (phase, then source) order
    pub single_safe_deploy: bool,    // true iff no Switch/Contract ops present
}

impl ZdmPlan {
    pub fn by_phase(&self) -> [Vec<&PhasedOp>; 4];  // grouped, phase 1..4
    pub fn from_classified(ops: Vec<ClassifiedOp>) -> ZdmPlan;  // pure core
}

/// Build the plan for the pending migrations (reuses check_pending_safety_in).
pub async fn plan_pending_in(dir: &Path) -> Result<ZdmPlan, MigrateError>;
pub async fn plan_pending() -> Result<ZdmPlan, MigrateError>;
```

`from_classified` is the pure heart (unit-testable with no DB); `plan_pending_in` is the thin async wrapper that reuses `check_pending_safety_in`. Re-exported from the facade under `umbral::migrate`.

## CLI: `umbral plan`

A new top-level subcommand in `crates/umbral-cli/src/lib.rs` (sibling of `checkmigrations`; `checkmigrations` stays the flat per-op linter).

- `umbral plan` — prints the pending changes grouped into the four phases, each op with its one-line guidance (the `OpSafety` reason). When `single_safe_deploy` is true, it says so plainly ("this migration is a single safe rolling deploy").
- `umbral plan --check` — **CI gate.** Exit `0` when `single_safe_deploy`, else exit non-zero after printing the phase split. Intended for the expand-phase migration in CI.
- `umbral plan --acknowledge` — with `--check`, exit `0` even with Switch/Contract ops, for the deploy where you *intend* to run a later phase (e.g. the contract migration after the switch shipped). Prints what it's acknowledging. This is the escape hatch for the gate's unavoidable blind spot: a pure op-type gate can't know old code is already gone, so the contract migration needs an explicit ack.
- `umbral plan --json` — machine-readable plan for external CI tooling. Optional; ship if cheap.

### Example output

```
$ umbral plan
Zero-downtime plan for 1 pending migration (blog/0007_rename_slug):

  Deploy 1 · expand
    + add column  post.url_slug (nullable)        safe — additive
  Deploy 2 · backfill
    ~ run SQL     backfill post.url_slug from slug review the row impact; make it idempotent
  Deploy 3 · switch
    ⇄ rename col  post.slug → url_slug            old code references `slug`; switch reads, then drop
  Deploy 4 · contract
    - drop column post.slug                        drop only after no running code reads it

  ✗ Not a single safe deploy — split across the releases above.
```

## Docs: `deployment/zero-downtime-migrations.mdx`

A user-facing MDX page (sidebar_position 2, next to `migrations-in-production`), cross-linked from `migrations/adding-not-null-columns` and `deployment/migrations-in-production`:

- The core rule (a risky change is a sequence of individually-safe migrations across releases).
- The expand → backfill → switch → contract cycle, mapped to umbral primitives (separate migration files per phase, `RunSql` backfills, code deploys between phases).
- A cookbook per unsafe op (add NOT NULL, rename, drop, type change, add UNIQUE with `CREATE INDEX CONCURRENTLY`), each pointing at the phase it belongs to.
- Using `umbral plan` and `umbral plan --check` in CI.
- The honest boundary: you sequence phases yourself today; auto-scaffolding and dual-write codegen are roadmap.

## Testing

- **Core (pure, primary):** `phase_of` returns the right phase for every `Operation` variant, including `AddColumn` nullable-vs-NOT-NULL-no-default; `from_classified` groups and orders correctly and sets `single_safe_deploy` (true for an all-additive set, false when any Switch/Contract op is present). No DB needed.
- **Wrapper:** `plan_pending_in(fixture_dir)` against a fixture migration tree (reuse the pattern `check_pending_safety_in` tests already use), asserting the grouped plan.
- **CLI:** an integration test invoking `umbral plan` / `umbral plan --check` against a fixture, asserting output and exit code (0 vs non-zero, and 0 under `--acknowledge`).

## Deferred (noted, not built)

- Auto-scaffolding the phase migration *files* from a single model change.
- Application-level dual-write codegen.
- Migration rollback / `migrate <target>` (gaps5 #26).
- Making `--check` aware that old code is already gone (needs deploy metadata) — the `--acknowledge` flag stands in.

## Commit plan

1. `umbral-core`: `migrate/plan.rs` (`Phase`, `phase_of`, `ZdmPlan`, `plan_pending*`) + facade re-export + unit tests.
2. `umbral-cli`: the `plan` subcommand (`--check`, `--acknowledge`, `--json`) + integration test.
3. `documentation`: `deployment/zero-downtime-migrations.mdx` + cross-links.
