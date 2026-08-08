# Compliance workflows and a tamper-evident audit trail: two plugins over umbral's own primitives

Status: draft for ratification (proposes the shape of gaps5 #86 and gaps5 #87)
Date: 2026-08-08
Drafts: planning/gaps5.md #86 (tf#299), planning/gaps5.md #87 (tf#300)
Builds on: docs/decisions/2026-08-08-search-and-data-governance.md (gaps5 #34, the classification metadata this reads), docs/decisions/2026-08-08-cdc-outbox-and-read-replicas.md (gaps5 #31, the outbox `Destination` trait the audit sink reuses)
Relates: plugins/umbral-admin (`AdminAuditLog` + `log()`), plugins/umbral-tasks (the durable queue + beat), docs/decisions/2026-08-08-product-north-star.md (Stage 2 self-hosted platform posture)

## Framing

Both items are Stage-2 platform capabilities, and both obey umbral's one governing rule: they are **plugins over primitives the framework already owns**, not new engines. #86 is a workflow layer over two things that already exist - the gaps5 #34 classification metadata (which columns are PII, which model rows belong to a subject, what retention class each falls under) and the `umbral-tasks` durable queue-and-beat. #87 generalizes the admin's existing `AdminAuditLog` into a framework-level, hash-chained, append-only `AuditEvent`, and reuses the gaps5 #31 outbox `Destination` trait to mirror events to a WORM / S3 / Kafka sink. Neither reimplements a primitive.

The two are one doc because they are the two halves of "prove you handled the data correctly": #86 *does* the compliant thing (finds a subject's data, exports it, erases it, sweeps expired rows, records consent), and #87 makes the record of what was done *impossible to quietly rewrite*. A DSAR workflow whose audit trail can be edited after the fact is not evidence; a tamper-evident log with nothing to record is inert. They ship together.

The scope boundary with gaps5 #34 is deliberate and already written into that doc: #34 defines the **metadata and the read hooks** (`ClassificationRegistry`, `subject_links()`, `pii_columns()`, the export/delete read functions, the retention-class registry, the `governance_legal_hold` table). #34 explicitly defers "the DSAR workflow engine: request intake, identity verification, human approval steps, SLA timers, staged/reversible deletion, delivery, and the audit-trail assembly" to #86. This doc designs exactly that engine.

---

## Part 1 (#86): compliance workflows

### What already exists (the substrate this orchestrates)

#86 writes almost no new *mechanism*. It composes four shipped seams:

1. **The gaps5 #34 classification metadata.** `ClassificationRegistry` is the machine-readable inventory built at boot from every model's `ModelMeta`: `data_map()` (every PII column, its sensitivity tier, its retention class, its residency), `pii_columns(table)`, and `subject_links()` (every model carrying a `data_subject = "<fk_column>"` pointer to a subject). The two read-oriented DSAR operations - **export** (walk `subject_links()`, select the subject's rows through the ORM, emit a JSON bundle of every PII column, revealing `Masked<T>` values via the loudly-named reveal path) and **delete / anonymize** (walk the same graph, consult `governance_legal_hold`, apply each column's retention-class action) - are already specified in #34 as pure functions of the registry plus the ORM. #86 does not re-derive these. It *orchestrates* them: puts a request, an approval, an SLA, and an audit record around the read hooks #34 delivered.

2. **The `umbral-tasks` durable queue and beat.** `enqueue` / `enqueue_task::<T>()` write a durable `pending` row to the app's own pool; the worker claims it, retries on failure with exponential backoff (`retry_backoff_base * 2^(attempts-1)`, capped at `retry_backoff_max`), and abandons to a terminal state after `max_attempts`. `TasksPlugin::periodic` + `run_beat` (the `tasks-beat` process) run recurring work on a schedule. This is the substrate a DSAR's long-running, resumable, retriable steps run on, and the substrate the retention sweep runs on. umbral has **no separate `Workflow` primitive**, and #86 does not invent one: a DSAR is modeled as a small explicit state machine persisted in a request row, advanced by task-backed steps, not by a generic workflow engine we would then have to maintain.

3. **The retention-class registry (#34).** `GovernancePlugin::retention_class("customer_data", Retention::days(365*3).on_delete(RetentionAction::Anonymize))` already names durations and default expiry actions (`HardDelete`, `Anonymize`, `Retain`). #34 states the *enforcement* - "a periodic sweep that finds rows past their class's horizon and applies the action" - is "a scheduled `umbral-tasks` beat job the plugin ships." #86 ships that sweep.

4. **`Masked<T>` crypto-shredding.** Destroying a per-subject key crypto-shreds the value; #34 calls this the fast bulk-erasure path for "right to be forgotten." The DSAR delete and the retention `Anonymize` action both route through it for `Masked` columns.

### The plugin surface: `umbral-compliance`

A built-in plugin under `plugins/umbral-compliance`, depending only on the `umbral` facade plus `umbral-tasks` (durable steps + beat) and the gaps5 #34 governance plugin (the classification registry it reads). It contributes a handful of models (all migrated the normal way via `plugin.migrations()`), a set of task-backed workflow steps, a consent API, and a report generator. It never writes raw SQL at the row level - every read and write goes through the ORM, exactly as CLAUDE.md requires of a plugin.

#### 1. The DSAR workflow (export and erasure with approvals)

A **`SubjectRequest`** model (table `compliance_subject_request`) is the durable state machine, one row per data-subject request:

| Column | Meaning |
|---|---|
| `id` | request id |
| `subject_id` | the data subject (matches the value `subject_links()` keys on) |
| `kind` | `"export"` (access / portability) or `"erasure"` (deletion / anonymization) |
| `status` | `received` -> `identity_verified` -> `pending_approval` -> `approved` -> `running` -> `completed` (or `rejected` / `failed` / `on_hold`) |
| `requested_at` / `due_at` | intake instant and the SLA horizon (e.g. 30 days for GDPR, 45 for CCPA) - the beat surfaces requests nearing `due_at` |
| `verified_by` / `approved_by` | FK to `auth_user` for the identity-verification and approval actors (dual-control) |
| `artifact_ref` | for a completed export, where the bundle landed (a signed, expiring URL into the media store); NULL for erasure |
| `manifest` | JSON summary of what the request touched: models, row counts, columns, actions applied |

The request advances through **task-backed steps**, each a durable `umbral-tasks` job so a long export or a large erasure survives a process crash and resumes rather than restarting:

- **Intake + identity verification.** A request is created (admin action or an authenticated self-service endpoint) in `received`. Identity verification is a required human gate before any data is read or written - the workflow refuses to advance an unverified request, so a spoofed "export everything for user X" cannot exfiltrate data. `verified_by` records who cleared it.
- **Approval (dual-control on erasure).** Export can be configured to auto-approve after verification; **erasure requires an explicit human approval** by default (`approved_by`), because an unapproved automated delete is how a bug becomes an irreversible data-loss incident. Approval is an admin action and an API call; the *policy* of who may approve is enforced by the plugin's permission gate (reuse the admin's existing permission checks, the same ones `AdminPlugin::view` widgets enforce).
- **Execution.** On `approved`, a worker step runs the corresponding #34 read hook:
  - **Export** walks `subject_links()`, selects the subject's rows through the ORM, assembles the JSON bundle (revealing `Masked<T>` via the reveal path, including the subject's own `private`/`secret` columns because the subject is entitled to their own data, never leaking a second subject's data into the bundle), writes it to the media store, and stamps `artifact_ref` with a signed, expiring URL.
  - **Erasure** walks the same graph, **consults `governance_legal_hold` and skips held rows** (logging that they were skipped, per #34), and applies each column's retention-class action - `HardDelete` via the ORM's `delete()`, `Anonymize` via tombstone overwrite or `Masked` crypto-shred, `Retain` left in place with surrounding PII anonymized. FK ordering reuses the migration engine's dependency graph so children erase before parents. `legal_hold_exempt` audit timestamps are untouched by construction.
- **Every state transition and every row-level action writes an `AuditEvent`** (Part 2). This is the join between the two halves of the doc: the workflow's proof-of-work is a hash-chained audit record, so "we deleted subject X's data on this date, approved by this operator, skipping these held rows" is tamper-evident, not a mutable log line.

Because each step is a durable task, the whole request is **resumable and retriable** on the queue's existing backoff, and a partially-completed erasure does not silently lose its place. `status` + `manifest` are the durable checkpoint.

#### 2. Retention automation (the #34 sweep, shipped)

The retention-class *enforcement* #34 defers to #86: a **`retention_sweep` periodic task** registered via `TasksPlugin::periodic` and run by the beat. Each run, per registered retention class, it finds rows past the class's horizon (`created_at`/`updated_at` older than `Retention::days/years`) and applies the class's `on_delete` action through the ORM - `HardDelete`, `Anonymize` (tombstone or `Masked` crypto-shred), or `Retain` (no-op, for legally-mandated records). It **consults `governance_legal_hold` first and skips held rows**, logging the skip, so a litigation hold is never silently swept away. Each action writes an `AuditEvent`. The sweep is idempotent (a row already past its horizon and already anonymized is a no-op) and batched so a large table does not lock. This is the same queue-and-beat substrate the DSAR steps and the #34 external-search-sync use - no second scheduler.

#### 3. Consent ledger + processing-purpose metadata

#34 explicitly listed "consent tracking / lawful-basis records" as *not* part of the classification layer - "a separate governance concern." #86 is where it lands, because lawful basis is the thing a DSAR and a retention decision both need to justify *why* data is held.

- **A `ConsentRecord` model** (table `compliance_consent`), append-only in spirit (a withdrawal is a new row, not an in-place edit, so the history of a subject's consent is preserved): `subject_id`, `purpose` (a processing-purpose key), `basis` (`consent` / `contract` / `legitimate_interest` / `legal_obligation` / ...), `granted` (bool), `source` (where it was captured), `recorded_at`. The current consent state for `(subject, purpose)` is the latest row; the full row history is the audit-grade consent trail.
- **Processing-purpose metadata** declares, per purpose, what it is for and which retention class governs data collected under it, registered on the plugin (`CompliancePlugin::purpose("marketing_email", Purpose::new(...).basis(LawfulBasis::Consent).retention("customer_data"))`). This makes "on what basis, and for how long, do we process this data" a machine-readable, code-declared fact - the same "declare it, get the plumbing" shape as the rest of umbral - rather than a wiki page that drifts.
- **The join to the DSAR and the sweep:** a consent withdrawal for a purpose can trigger (or is surfaced as due for) an erasure of data held solely on that basis; the retention sweep reads the purpose's retention class. Writing a `ConsentRecord` and reading the current state both go through the ORM.

#### 4. Export / delete approvals

Approvals are not a bespoke mechanism - they are the `pending_approval` -> `approved` transition on `SubjectRequest`, gated by the plugin's permission check and recorded in `approved_by` plus an `AuditEvent`. The dual-control default (erasure needs a human approval distinct from the verifier) is the safety posture; an app can relax it per request kind via config, but the framework ships the safe default. Because the approval is a state transition on a durable row, an approval that arrives while the process is down is not lost - the request simply waits in `pending_approval`.

#### 5. Compliance report generator

A report generator that reads the shipped state and emits an operator- and auditor-facing report:

- **The data map (from #34's `ClassificationRegistry::data_map()`)** - the GDPR Art. 30 "record of processing activities," generated from code: every model, its PII columns, sensitivity tiers, retention classes, residency, and (via the consent/purpose registry) lawful basis per purpose.
- **DSAR activity** - requests received / completed / rejected in a window, median and worst-case time-to-completion against `due_at` (the SLA evidence), and per-request manifests.
- **Retention activity** - rows swept per class, held rows skipped, over a window.
- **Consent state** - current grants/withdrawals per purpose, and the withdrawal history.

Output is JSON and Markdown via a `umbral compliance report` CLI command (the same shape as #34's `umbral governance datamap`), plus an admin custom view (the `AdminPlugin::view` seam already exists). The report's integrity claim leans on Part 2: because every DSAR and retention action is an entry in the hash-chained audit log, the report is backed by a trail that can be independently verified, not by mutable counters.

### What #86 defers

- **Automated identity verification** (matching a request to a real person) - the framework provides the *gate* and records `verified_by`; the verification method (email challenge, ID upload, SSO assertion) is app policy.
- **Jurisdiction-specific SLA presets** beyond a configurable `due_at` - v1 ships a default horizon and lets the app set it per request kind; a library of "GDPR 30d / CCPA 45d" presets is a thin follow-up.
- **Reversible / staged erasure** (a soft-delete quarantine window before hard delete) - the state machine has room for it (`status` could gain a `quarantined` state), but v1 ships direct `HardDelete`/`Anonymize` per the retention-class action, with the legal-hold skip as the guard against premature erasure.

---

## Part 2 (#87): a tamper-evident audit trail

### What already exists (`AdminAuditLog`)

The admin ships a working, append-only audit table today (`plugins/umbral-admin/src/models.rs`):

- **`AdminAuditLog`** columns: `id`, `actor_user_id` (i64, FK to `auth_user`), `action` (String, one of `"create"` / `"update"` / `"delete"` / `"action:<key>"`), `model` (the SQL table name touched), `object_id` (`Option<String>` - **text, not i64**, deliberately, so it can name a row whose PK is a `String` or `Uuid`, not only an `i64`; gaps3 #59 fixed the bug where an INTEGER column could not address a non-i64 row), `diff_summary` (a short human description, e.g. `"created Post #42"`), and `created_at`.
- **`log(actor_user_id, action, model, object_id, diff_summary)`** is the fire-and-forget append: it builds an entry and `save()`s it, logging (never surfacing) an insert error so "a CRUD handler that succeeds at its real work isn't undone by an audit-write hiccup."
- Every column is `#[umbral(noedit)]` and the admin surfaces the table read-only, so the form path cannot mutate a row "even if someone navigates directly to the edit URL."

This is honest append-only-by-*convention*: the form path won't edit it, but there is nothing stopping a direct `UPDATE compliance_... ` on the database from silently rewriting or deleting a row, and nothing that would *detect* it if someone did. For an admin activity log that is a reasonable ceiling. For the evidence backing a DSAR erasure or a SOC 2 / HIPAA audit, it is not: the whole value of an audit trail is that it cannot be quietly altered after the fact. #87 raises that ceiling.

### The design: generalize `AdminAuditLog` into a framework `AuditEvent`

#87 lifts the admin's table into a framework-level, hash-chained audit primitive and re-expresses `AdminAuditLog` as one *producer* of it.

#### 1. `AuditEvent`: the generalized, hash-chained model

A framework-level **`AuditEvent`** model (owned by a small `umbral-audit` plugin, migrated the normal way), superset of `AdminAuditLog`'s columns plus the tamper-evidence chain:

| Column | Meaning |
|---|---|
| `id` | monotonic event id (also the chain sequence) |
| `actor_id` | who acted (`Option<String>` - text PK-shape independent, and NULL for a system/beat action like the retention sweep) |
| `action` | `"create"` / `"update"` / `"delete"` / `"action:<key>"` (the existing `AdminAuditLog` vocabulary, unchanged) plus governance actions (`"dsar:export"`, `"dsar:erase"`, `"retention:anonymize"`, `"consent:withdraw"`, ...) |
| `target_table` | the SQL table the action touched (was `model`) |
| `object_id` | `Option<String>` affected row PK, text, exactly as `AdminAuditLog` already stores it |
| `summary` | the human description (was `diff_summary`) |
| `metadata` | JSON for structured context (the DSAR request id, the retention class, row counts) - richer than a single summary string |
| `created_at` | event instant |
| `prev_hash` | the `hash` of the immediately preceding event in the chain (NULL for the genesis row) |
| `hash` | `H(prev_hash ‖ id ‖ actor_id ‖ action ‖ target_table ‖ object_id ‖ summary ‖ metadata ‖ created_at)`, a SHA-256 over a canonical serialization of this row's fields **and** the previous row's hash |

The chain is the tamper-evidence: because each row's `hash` folds in the previous row's `hash`, altering or deleting any historical row breaks every subsequent `hash`, and the break is *detectable* by re-walking the chain. You cannot rewrite one entry without rewriting the entire tail, and you cannot rewrite the tail without also controlling the external sink (below) that already captured the original hashes. This is the standard hash-chain / hash-linked-log construction (the same idea as a Merkle-linked ledger, minus the tree - a linear chain is enough for an append-only log and is cheaper to verify incrementally).

Appending is serialized (each new event must read the current tail's `hash` to compute its `prev_hash`), so the writer takes a short per-append lock (reuse the same advisory-lock / conditional-insert discipline the tasks beat already uses to prevent double-fire) to keep the chain strictly linear under concurrency. Append stays fire-and-forget for the caller's *latency* (the CRUD handler doesn't block on it), but the *ordering* is enforced so the chain never forks.

**A verify operation** (`umbral audit verify` CLI + an admin view) re-walks the chain from genesis (or a checkpoint), recomputes each `hash`, and reports the first index where a recomputed hash diverges from the stored one - i.e. exactly which event was tampered with or removed. Periodic verification runs as a beat task and raises an alert on divergence.

#### 2. `AdminAuditLog` becomes a producer, not a parallel table

The refactor keeps the admin's ergonomics and its call sites intact:

- `AdminAuditLog::log(actor_user_id, action, model, object_id, diff_summary)` stays as a thin wrapper that constructs an `AuditEvent` (`actor_id = actor_user_id.to_string()`, `target_table = model`, `summary = diff_summary`) and appends it to the chain. Existing admin call sites do not change.
- The admin's read-only audit view reads `AuditEvent` filtered to admin-origin actions; `audit_for_object(model, object_id, limit)` becomes a filter over `AuditEvent` (the `object_id` text-match it already does). Because `object_id` was already text (gaps3 #59), no PK-shape regression.
- The `#[umbral(noedit)]` posture carries over and is now backed by real tamper-evidence, not only form-path convention.

This is the "generalize AdminAuditLog into a framework AuditEvent" the gap asks for: one audit primitive, many producers (admin CRUD, DSAR steps, retention sweep, consent changes, auth events), all writing to one verifiable chain.

#### 3. Optional external sink (WORM / S3 / Kafka) via the outbox `Destination` trait

A hash chain proves *internal* consistency (no row was altered relative to its neighbors), but an attacker with full DB write access could in principle recompute the entire chain after an edit. The defense is to get the hashes *out of the database* into append-only external storage the app's DB credentials cannot rewrite. #87 does this by **reusing the gaps5 #31 outbox `Destination` trait** - it does not invent a second delivery mechanism:

```rust,ignore
#[async_trait]
pub trait Destination: Send + Sync {
    fn name(&self) -> &'static str;
    async fn deliver(&self, event: &OutboxEvent) -> Result<(), DeliveryError>;
}
```

- Each `AuditEvent` append also enqueues an outbox event (ideally via the true transactional path `outbox::publish_on(tx, ...)` when the audited write is itself in a transaction, so the audit record and the business change commit atomically). The #31 relay then delivers it at-least-once, with the queue's existing backoff and per-attempt delivery log, to whichever audit `Destination`s the app registered.
- Three audit sinks ship as `Destination` implementations:
  - **`worm_s3`** - writes each event (and periodic chain checkpoints: `(last_id, hash)` tuples) to an S3 bucket configured with **Object Lock in compliance mode**, which S3 itself enforces as write-once-read-many - not even the account root can delete or overwrite a locked object before its retention expires. This is the anchor: once a chain checkpoint is in WORM storage, rewriting the in-DB chain to match it is impossible, so the tamper is detectable by comparing the live chain against the last WORM checkpoint.
  - **`kafka`** - appends events to a Kafka topic (a log-structured, append-only, replayable stream is exactly a Kafka topic's shape), for downstream SIEM / warehouse ingestion.
  - **`syslog` / file** - an append-only local or forwarded log for the simplest deployments.
- Because the sink is the *outbox's* `Destination` trait, an app adds its own audit sink (`AuditPlugin::sink(MySplunkSink)` delegating to `OutboxPlugin::destination`) with no framework change - the same extension shape as any third-party plugin. And the at-least-once + idempotency-key guarantee #31 already documents applies unchanged: the event `id` is the dedupe key, so a redelivered audit event is not double-counted at the sink.

The result is defense in depth: the in-DB hash chain makes tampering *detectable* cheaply and continuously; the WORM/Kafka sink makes it *undeniable* by holding hashes the DB credentials can't reach.

### Why generalize rather than bolt tamper-evidence onto `AdminAuditLog` in place

- `AdminAuditLog` is admin-scoped by name and location (`plugins/umbral-admin`). DSAR, retention, and consent actions in `umbral-compliance` need to write audit records too, and they must not have to depend on the admin plugin to do so. Lifting the model into a small `umbral-audit` plugin that both admin and compliance depend on keeps the arrows pointing inward and the admin from becoming a dependency of governance.
- The hash chain is a cross-cutting concern (any producer's append extends the same chain); making it a shared primitive is the only way one chain covers all producers. Two parallel audit tables with two chains would defeat the point.

### What #87 defers

- **A Merkle tree / inclusion proofs** (proving a single event's membership without re-walking the whole chain) - a linear chain with periodic WORM checkpoints is enough for v1's "detect any tamper on verify"; per-event succinct proofs are a later optimization if an external auditor needs them.
- **Cryptographic signing of each event** (an HMAC or asymmetric signature over the hash, so tamper-evidence survives even an attacker who controls the WORM checkpoint cadence) - the WORM anchor covers the realistic threat model; signing keys are a heavier follow-up.
- **Retrofitting non-admin, non-governance producers** (auth login events, session lifecycle) onto `AuditEvent` - the primitive supports them, but wiring every plugin's events into the chain is incremental adoption, not a v1 gate.

---

## Why these are two items in one doc

They share one spine and one dependency direction. #86 stands on the gaps5 #34 classification metadata and the `umbral-tasks` queue-and-beat; #87 generalizes the admin's shipped `AdminAuditLog` and reuses the gaps5 #31 outbox `Destination` trait. Both refuse to reimplement anything: no workflow engine (a DSAR is a state machine over durable tasks), no scheduler (the beat), no delivery mechanism (the outbox relay), no new crypto beyond a SHA-256 hash chain. And they compose end to end: every action #86 takes is recorded as a #87 `AuditEvent`, so the compliance workflow's proof-of-work is tamper-evident by construction. umbral grows platform breadth by wrapping its own primitives in plugins, never by bolting on a parallel engine - and here the two plugins wrap each other's output into a single auditable whole.
