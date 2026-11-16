# review_4 — whole-project sweep of umbral

**Status: COMPLETE** · 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) · **28 crates ≈ 274k LOC · 31 files · ~202 findings · 1 CRITICAL / 28 HIGH / ~61 MED / ~81 LOW / ~28 INFO**

> **Read first:** [00-synthesis.md](00-synthesis.md) — cross-crate themes, ranked fix order, and the full verification log.

## Method

- **Point-in-time review, not a tracker.** New findings only — anything already tracked (`gaps*.md`, `features.md`, `bugs.md`, `audit_2/`, `review_1–3/`, `hardening/`, `optimizations/`) is listed as `known (ref)` and does not count toward severity totals.
- **Every CRITICAL/HIGH is self-verified** (re-read the code, default-refute, trace a concrete scenario) by the reviewer, then spot-verified against the cited code before publication. Historical finder false-positive rate ≈ 6%; review_3's separate verify phase died on session limits — verification lives inside each reviewer this time.
- **Static analysis only** — no builds, no test runs, no code changes. (Disk at 97%, and a review must not perturb a tree with live consumers.)
- **Strengths and clean-areas sections are mandatory**; severity inflation is a defect of the review itself.
- The rubric is [`.claude/skills/umbral-review.md`](../../.claude/skills/umbral-review.md); the file format is [`_TEMPLATE.md`](_TEMPLATE.md).
- **Out of scope by decision (2026-10-10):** `examples/`, `umbral_website/`, `documentation/` (standalone projects).
- **Pilot (2026-10-10):** rubric validated on `umbral-casing` against a no-rubric baseline (`_scratch/baseline-casing.md` vs [`umbral-casing.md`](umbral-casing.md)); rubric v1.1 incorporated the pilot's feedback (cross-crate attribution rule, severity calibration notes, INFO short-form, count/size conventions).

## Severity

| Label | Meaning |
|---|---|
| CRITICAL | Security hole, data loss/corruption, unsoundness, or a broken core path |
| HIGH | Real correctness bug on a reachable path; serious hot-path perf; defense-in-depth weakness |
| MEDIUM | Bounded correctness/perf/security issue; ergonomics trap with a workaround |
| LOW | Minor; cold-path perf; polish |
| INFO | Observation/opportunity; no action required |

## Axes (tag on each finding)

`correctness` · `security` · `perf` (layout, alloc, async, sql) · `drift` (a rule enforced in one place and forgotten in a sibling — this codebase's signature failure mode) · `api` · `ergonomics` · `docs` · `tests` · `arch` · `hygiene`

## Index

| Crate | LOC | File | Status | C/H/M/L/I |
|---|---|---|---|---|
| umbral-core | 130,814 | [umbral-core.md](umbral-core.md) | ✅ done | 0/6/24/26/4 ⚠1↓ |
| umbral-macros | 8,403 | [umbral-macros.md](umbral-macros.md) | ✅ done | 0/1/2/2/1 |
| umbral | 1,162 | [umbral.md](umbral.md) | ✅ done | 0/0/0/3/1 |
| umbral-cli | 8,412 | [umbral-cli.md](umbral-cli.md) | ✅ done | 0/0/1/3/0 |
| umbral-casing | 285 | [umbral-casing.md](umbral-casing.md) | ✅ done | 0/0/1/2/1 |
| umbral-testing | 1,016 | [umbral-testing.md](umbral-testing.md) | ✅ done | 0/1/2/2/1 |
| umbral-admin | 28,018 | [umbral-admin.md](umbral-admin.md) | ✅ done | 0/4/2/3/1 |
| umbral-rest | 23,848 | [umbral-rest.md](umbral-rest.md) | ✅ done | 0/3/2/2/0 |
| umbral-auth | 13,213 | [umbral-auth.md](umbral-auth.md) | ✅ done | 0/2/1/2/2 |
| umbral-storage | 9,096 | [umbral-storage.md](umbral-storage.md) | ✅ done | 1/2/1/2/2 |
| umbral-realtime | 6,104 | [umbral-realtime.md](umbral-realtime.md) | ✅ done | 0/1/1/3/1 |
| umbral-sessions | 5,848 | [umbral-sessions.md](umbral-sessions.md) | ✅ done | 0/2/2/3/1 |
| umbral-tasks | 5,803 | [umbral-tasks.md](umbral-tasks.md) | ✅ done | 0/2/4/2/0 |
| umbral-openapi | 5,101 | [umbral-openapi.md](umbral-openapi.md) | ✅ done | 0/0/2/1/0 |
| umbral-graphql | 5,091 | [umbral-graphql.md](umbral-graphql.md) | ✅ done | 0/2/3/1/0 |
| umbral-cache | 3,478 | [umbral-cache.md](umbral-cache.md) | ✅ done | 0/0/1/3/1 |
| umbral-permissions | 3,461 | [umbral-permissions.md](umbral-permissions.md) | ✅ done | 0/0/0/2/1 |
| umbral-oauth | 3,127 | [umbral-oauth.md](umbral-oauth.md) | ✅ done | 0/0/2/0/1 |
| umbral-tenants | 2,853 | [umbral-tenants.md](umbral-tenants.md) | ✅ done | 0/0/1/1/1 |
| umbral-email | 2,208 | [umbral-email.md](umbral-email.md) | ✅ done | 0/0/2/2/0 |
| umbral-security | 2,108 | [umbral-security.md](umbral-security.md) | ✅ done | 0/0/1/1/0 |
| umbral-signals | 1,477 | [umbral-signals.md](umbral-signals.md) | ✅ done | 0/0/0/1/3 |
| umbral-logs | 1,205 | [umbral-logs.md](umbral-logs.md) | ✅ done | 0/0/0/2/0 |
| umbral-playground | 1,128 | [umbral-playground.md](umbral-playground.md) | ✅ done | 0/0/0/1/1 |
| umbral-rls | 969 | [umbral-rls.md](umbral-rls.md) | ✅ done | 0/1/2/1/0 |
| umbral-health | 926 | [umbral-health.md](umbral-health.md) | ✅ done | 0/0/0/1/0 |
| umbral-analytics | 810 | [umbral-analytics.md](umbral-analytics.md) | ✅ done | 0/0/1/2/2 |
| umbral-livereload | 628 | [umbral-livereload.md](umbral-livereload.md) | ✅ done | 0/0/2/1/2 |

ID prefixes: CORE · MACROS · UMBRAL · CLI · CASING · TESTING · ADMIN · REST · AUTH · STORAGE · RT · SESS · TASKS · OPENAPI · GRAPHQL · CACHE · PERM · OAUTH · TENANTS · EMAIL · SEC · SIG · LOGS · PLAY · RLS · HEALTH · ANALYTICS · LIVE

## Repo-level

- [repo-hygiene.md](repo-hygiene.md) — CI workflows, `deny.toml`, scripts, stray files, release config.
- [00-synthesis.md](00-synthesis.md) — cross-crate themes + ranked fix order (written after the waves).
- [ease-of-use.md](ease-of-use.md) — DX analysis (companion): what a developer types today, the three tiers of avoidable typing, the unhide list, ranked shortlist.
- [performance.md](performance.md) — performance (companion): verified perf findings, hot-path inventory, measurement-gated proposals (bench-first).
- Findings are filed as tracker entries 1–175 in [`planning/gaps7.md`](../gaps7.md) (+ a 28-item INFO appendix).

## Execution ledger

| Wave | Agents | Scope | Status |
|---|---|---|---|
| Pilot | 2 | umbral-casing (baseline vs rubric) — format validated, rubric v1.1 | ✅ done |
| W1 | 8 + 1 assembler | core crates; umbral-core in 5 slices merged by an assembler | ✅ done (assembler merged) |
| W2 | 8 | heavy plugins (admin, rest, auth, sessions+oauth, storage+cache, openapi+graphql+play, tasks+signals, realtime+health) | ✅ done |
| W3 | 3 | security cluster (sec+rls+tenants+perm), small plugins (email+logs+analytics+livereload), repo hygiene | ✅ done |

## Lineage

review_1 (2026-06-10, topic-based, verified-claims policy) → reviews-2 (codex) → review_3 (2026-08-16, per-issue files, triage closed; its 115-agent fan-out died on session limits) → **review_4 (this)**.
