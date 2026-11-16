---
name: umbral-review
description: Use when reviewing or auditing umbral — a single crate ("review umbral-auth"), the whole framework (review_N sweeps under planning/), or any correctness/perf/security/ergonomics assessment of crates/ or plugins/ code. Triggers on "review", "audit", "sweep", "review_4", "review_5".
---

# Reviewing umbral

## Context
Review passes over umbral happen periodically (review_1 2026-06, review_3 2026-08, review_4 2026-10 — all under `planning/`) and per-crate on request. A review's goal: find what makes the framework less acceptable, fast, easy, performant, secure, or complete than it claims — and write findings the maintainer can triage into the gap trackers. This skill is the rubric. Findings live in `planning/<review_N>/<crate>.md` using that directory's `_TEMPLATE.md`.

## Approach

### 1. Evidence and constraints (non-negotiable)

- **Read the real code.** Every finding cites `file:line` plus a quoted snippet. Grep locates; reading justifies. A claim without a quoted line is a rumor, not a finding.
- **Point-in-time, not a tracker.** Dedupe against everything already tracked before writing:
  `grep -rn "<crate-name>" planning/gaps*.md planning/features.md planning/REAL-GAPS.md planning/improvements.md planning/security.md planning/performance.md planning/orm_fixes.md 2>/dev/null` and
  `grep -rln "<crate-name>" planning/review planning/reviews-2 planning/review_3 planning/audit_2 planning/hardening planning/optimizations planning/archive 2>/dev/null`.
  The tracker set grows over time — if a path errors or looks stale, `ls planning/*.md` and glob what is actually there. For a crate whose name is a substring of everything (e.g. `umbral`), grep by crate **path** (`crates/umbral/`) instead of name. **Tracker stamps go stale**: an entry's own `[open]`/fixed tag is a claim, not a fact — verify against the code at the sweep commit and annotate the Known entry with which side is true ("stamp says partial; all three parts ship at this commit"). The crate-name grep catches crate-level overlap; for concept-level overlap — a behavior buried in a long `gaps5/6` entry that never names the crate — grep the concept instead: the check name, the error string, the constant. Anything already tracked is listed under "known (ref)" and does not get a numbered finding or count toward severity totals. Missing tracker paths in the grep recipe are fine — tolerate and move on.
- **Class-level trackers don't absorb specific defects.** When a tracker entry covers a finding's *class* without recording the specific defect (an open batch like "stale doc-comments", or a closed write-up that restates the claim as a feature), keep the finding numbered and cross-reference the tracker in Known with an explicit note about the relationship. One hard boundary: **a shipped tracker claim contradicted by the code is a numbered finding, never Known-only** ("gaps4 #80 says object-scope covers this; it does not" → full finding + cross-ref).
- **Numbered findings are always new.** There is no "known" status on a numbered finding; known items live only in the Known section. The grep above decides status, not memory.
- **Attribution across crates.** File a finding against the crate whose documented contract is violated, even when the trigger lives in a consumer; cite the consumer site and leave a one-line routing note in your Out-of-scope section (with `file:line`) so the sibling reviewer cross-checks instead of duplicating.
- **Check the load-bearing claims.** When the crate's docs assert an invariant or canonicality ("byte-identical to X", "every writer fires this"), verify it once against the tree. It calibrates everything else you find.
- **Counts and sizes.** `Size` = all `.rs` files under the crate (src + tests, excluding `target/`). The banner's finding count includes every numbered finding, INFO included.
- **Static analysis only.** No `cargo build` / `test` / `clippy` during a sweep — the disk runs hot (see the disk-constrained-test-sweep skill) and builds perturb a tree that may have live demo consumers. Cheap reads (`cargo metadata`, Cargo.toml/lock) are fine, and **reading vendored dependency sources under `~/.cargo/registry/src/` is fair game — often decisive** for claims that hinge on a dependency's semantics (sqlx decode rules, axum upgrade behavior).
- **Review-only.** A sweep never edits product code. Findings are the deliverable; fixes are a separate, planned cycle.
- **One file per crate**, your agent writes only that file. (Project-wide sweeps may use per-slice `_scratch/` files merged by an assembler — see §5. Slice scratch files report slice LOC and may adapt the H1/banner; the final merged file reports crate-wide LOC. The published file has one voice.)

### 2. The axes

**Correctness & soundness.** Logic bugs on reachable paths; edge cases (empty/null/overflow/boundary); error paths — swallowed errors are findings (CLAUDE.md "Fix, don't patch": `.ok()` discarding a secondary error, `unwrap_or_default()` on a never-`None` production value, `_`-prefixed unused variables that hide forgotten wiring, `cfg(not(test))` hiding broken-in-test behavior). Panics in library code (`unwrap`/`expect`/indexing) rank by reachability. `unsafe` audit: count it; each block gets read. Race conditions and async correctness: locks held across `await`, cancellation safety, spawn lifetimes.

**Security.** Check against the known drift class *first*: **a visibility/authorization rule enforced on one surface and forgotten on a sibling** (read vs filter vs sort vs search vs OpenAPI params vs client-gen vs admin vs GraphQL — this exact class has recurred; grep for the rule's sibling implementations and parity-check them). Then: injection (values bound, identifiers aliased — verify, don't assume), authz default-deny vs fail-open, CSRF, template autoescaping and `safe_string` misuse, cookie flags, session/token entropy + expiry + revocation, argon2 params, constant-time compares, path normalization vs authorization (percent-encoding!), upload sniff/allow-list/decode limits, SSRF in outbound features, rate-limit key quality, prod-vs-dev error-text leakage, RLS + session-variable reset on pooled connections, secrets in logs/errors.

**Performance.** Four sub-lenses:
- *layout*: `repr(Rust)` structs are auto-reordered by rustc — do **not** write padding nits for plain structs. Audit instead: `#[repr(C)]` structs (no reordering; check padding and element order), enums whose largest variant bloats every value (Box the fat variants), hot/cold field grouping, per-request struct size (what is cloned/copied on the hot path), false sharing in shared counters.
- *alloc*: per-request `format!`/`to_string`/`clone()`/`Vec` growth without `with_capacity`; `serde_json::Value` churn; `Cow`, `Arc<str>`, `Arc<[T]>` opportunities for shared immutables; boxing in query construction.
- *arc-borrow*: `&Arc<T>` parameters that only need `&T`; `Arc::clone` in hot loops (atomic RMW each clone); `Arc<Mutex<_>>` coarse granularity; `Arc` where `Rc` (or a borrow) suffices; clones of large structs where a borrow would flow.
- *async-sql*: blocking work in async context; unbounded channels/queues; N+1 query shapes; FK columns without indexes (join scans); missing default LIMITs on list surfaces; per-call query construction cost. Both backends: SQLite **and** Postgres arms of every dual-path terminal.

**API & ergonomics.** Prelude correctness; naming consistency; errors that `?`-flow with good `From` plumbing; footgun defaults; discoverability; doc-claims vs reality (a docstring promising a builder method that doesn't exist is a finding against the *code*, not the docstring — CLAUDE.md); feature-flag hygiene (e.g. a REST-free app must compile with zero serializer code); semver discipline against `STABILITY.md`.

**Architecture & contracts.** Plugin-trait conformance (every built-in is the contract's proof — anything special-cased for a built-in is a finding); dependency direction (core never names a plugin; plugins import the facade only); **plugins use the ORM, not raw SQL** — `grep -rn "sqlx::query" plugins/<name>/src/` is mandatory; any hit must justify one of the two CLAUDE.md exceptions or it is a finding. Cargo.toml dependency directions.

**Tests & docs.** Behavioral test quality (real rows, real public path, read back the object graph — assert SQL only *alongside* a round-trip); coverage of the risky paths this sweep touched; docs present for every user-facing feature; rustdoc examples that would compile (read them — stale examples are findings).

**End-to-end completeness.** Where does an app author hit a wall: scaffolding, the declare→migrate→change→migrate loop, wiring admin/auth/rest/tasks together. Dead code, unwired claims, "documented but unbuilt" surfaces.

**Drift — first-class lens.** This codebase's signature failure mode is *the same rule implemented N times and fixed in N−1 places*. When you see a policy, guard, or transform implemented inline, grep for its siblings (the SQLite/PG twin, the SSE/WS twin, the form/JSON/admin twins, copied DDL strings, duplicated escapers) and check they agree. Divergence is a finding even when both copies are individually correct — name the drift hazard explicitly.

**Hygiene.** Stray artifacts, committed dbs/locks, gitignore gaps, dead config. Repo-wide hygiene (CI, top-level config, release plumbing) goes in the review dir's repo file — but an artifact physically inside the crate you are reviewing gets a finding (or out-of-scope note) in the crate file, with provenance if you can establish it.

### 3. Severity and numbering

| Label | Meaning |
|---|---|
| CRITICAL | Security hole, data loss/corruption, unsoundness, or a broken core path |
| HIGH | Real correctness bug on a reachable path; serious hot-path perf; defense-in-depth weakness |
| MEDIUM | Bounded correctness/perf/security issue; ergonomics trap with a workaround |
| LOW | Minor; cold-path perf; polish |
| INFO | Observation/opportunity; no action required |

**Calibration:** dev-time codegen/tooling that emits invalid or non-compiling output is MEDIUM unless the failure is silent or touches data (then HIGH). Cold-path micro-allocations are INFO at most, and only when the crate has little else to report. When torn between two levels, state which definition the finding meets — choose the lower level only if the higher definition is not clearly met.

Issue IDs are crate-prefixed and sequential in the final file: `CORE-12`, `AUTH-3`, `ADMIN-7`. Scratch/slice files use slice-local IDs (`ORM-1`); the assembler renumbers. **No severity inflation** — a sweep where everything is HIGH is a sweep nobody triages. Strengths and clean-areas sections are mandatory for the same reason.

### 4. Self-verification protocol (CRITICAL and HIGH only)

1. **Re-read the code** — the whole function and its callers, not just the cited line.
2. **Default-refute**: try to prove the finding wrong. Is the guard elsewhere? Is the path unreachable? Is it intended? Historical finder false-positive rate ≈ 6%.
3. **Trace one concrete scenario** — named inputs/state to a named outcome. If you cannot write one, downgrade or drop the finding.
4. **Quote the code exactly.** If the claim depends on a caller or another crate, cite that site too.

A verifier (main thread or a second agent) will re-read your CRITICAL/HIGH against the code before the finding is published. Write them so that check is a 2-minute read.

The `self-verified` tag is mandatory on CRITICAL/HIGH. It may also appear on MEDIUM/LOW when you genuinely ran the same checks, and must be absent when you did not — an untagged MEDIUM/LOW is normal, not a red flag. In a static-only sweep the tag means the three checks were performed **by reading** (re-read, refute-attempt, concrete trace), not that anything was executed; where a claim ultimately depends on runtime behavior, say so inside the finding ("static trace; needs runtime confirmation") rather than letting the tag imply repro.

### 5. Fan-out pattern (project-wide sweeps)

- **Pilot first.** One small crate with the rubric validates format and severity calibration before the fleet goes out.
- **Waves of 4–8 agents**, one crate-slice per agent, each writing only its own file. `umbral-core` (~130k LOC) is sliced by directory: slices write to `_scratch/`, then ONE assembler agent merges into a single `umbral-core.md` — sequential IDs, cross-slice dedup, one voice, crate-wide LOC in the banner. Routed notes whose target crate has *already* been reviewed go to the synthesis doc instead of back-editing finished files; routed notes for not-yet-reviewed crates are passed into that crate's prompt.
- **Budget-aware.** review_3's 115-agent workflow died on session limits mid-verification; verify-inside-the-reviewer plus ≤20 agents keeps a sweep completable. A completed 20-agent sweep beats a dead 115-agent one.
- **Prompt skeleton per agent**: crate/slice path; write path; "read `.claude/skills/umbral-review.md` and `planning/review_4/_TEMPLATE.md` first"; hard constraints (no builds, no code edits, write only your file); dedupe recipe (§1); return format — counts per severity, top 3 findings, file path, out-of-scope notes.
- **After the waves**: spot-verify every CRITICAL/HIGH against cited code, then write `00-synthesis.md` (cross-crate themes, ranked fix order, proposed tracker entries). Only then report done.

## Why
- Verification is *inside* each reviewer because review_3 proved a separate verify phase is the first casualty of a budget cut — and an unverified candidate list is a work-list, not a defect list.
- The dedupe rule exists because re-reporting tracker items wastes triage; reviewers must stand on `gaps*` history, not repeat it.
- Drift is elevated to a lens because it is empirically this tree's most common real defect class (two backends, two transports, two write paths — the copies drift).
- Strengths/clean-areas exist because a review that only lists flaws gets discounted; the maintainer needs to know what not to break.

## Pitfalls
- Re-reporting `gaps` entries as new — grep first, always.
- Claims from grep alone; the guard may be one call up.
- Testing what you cannot run: assess tests structurally (what do they assert, would they fail if the behavior broke) — never imply a suite passed.
- Single-backend reasoning: this framework's bugs love the PostgreSQL arm or the SQLite arm specifically.
- Writing padding nits for `repr(Rust)` structs (rustc already reorders them).
- Reviewing the admin's HTML/CSS polish as if it were behavior, and vice versa.
- Touching the dev-serving example apps or running their builds (see the dev-server memory / CLAUDE.md).
- Reviewing a crate in isolation: the sharpest findings often live at the contract boundary with a consumer — trace at least the documented call sites. (A pilot found its crate's best finding only by following a call into `inspect.rs`.)
- Padding a small clean crate with INFO-grade nits to look thorough. A healthy crate can come back with 2–4 findings; that is a valid result.

## See also
- `planning/review_4/README.md` — method, severity table, index of the current sweep
- `planning/review_4/_TEMPLATE.md` — the exact file format
- `planning/review_3/claude/00_SUMMARY.md` — prior sweep and its failure mode (worth reading before any big fan-out)
- `planning/review/README.md` — review_1's method (verified-claims policy lineage)
- CLAUDE.md — the architecture rules that double as review criteria (ORM-not-raw-SQL, facade-only imports, fix-don't-patch)
