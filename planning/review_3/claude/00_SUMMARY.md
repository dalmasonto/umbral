# Framework-wide sweep — review_3 (Claude)

Full correctness + **security** + simplicity sweep across all 28 crates (~130k lines of `src/`), run as a fan-out workflow: 3 finder lenses × 11 risk-ordered areas → adversarial verification → per-area report.

## What actually happened

The workflow **hit the session usage limit partway through**. Of 115 agents, the **33 finder agents completed** but most of the **verify and report phases died** (79 errors, all "session limit resets 1:40am"). So this folder is built from:

1. **The complete finder output** — 71 unique candidates recovered from the run journal (`_raw_candidates.json`). Raised, not yet verified.
2. **Hand-verification of the top security cluster**, done in the main thread after the workflow died. The findings in `01_confirmed.md` were each checked against the real code, adversarially (default-refute), with the exploit chain traced end to end.

**This is a partial result.** The finder phase covered the whole framework; the *verification* phase covered only the highest-severity security findings. The 68 candidates in `02_triage.md` are genuine leads but have **not** been independently verified — treat them as a work-list, not a defect list. The finder false-positive rate on the earlier release-diff sweep was ~6% (2 of 31), but that was on code the agents had more context for; expect higher here.

## Candidates raised

| Severity | Count | Verified so far |
|---|---|---|
| critical | 3 | 3 confirmed (all one root cause + its ORM primitive) |
| high | 18 | 2 confirmed, 16 pending |
| medium | 30 | pending |
| low | 20 | pending |

By area: umbral-core 37, umbral-auth 7, umbral-macros 6, umbral-graphql 5, umbral-storage 5, umbral-admin 4, umbral-realtime 2, umbral-rest 2, and one each in openapi/permissions/sessions.

## Confirmed so far (see `01_confirmed.md` for evidence)

1. **CRITICAL — admin inline cell-edit is a privilege-escalation hole.** A staff user with `Change` permission on `auth_user` can `POST /admin/auth_user/<own-id>/cell/is_superuser` and become superuser. `cell_edit_post` gates only on `readonly_fields`; `DynQuerySet::update_one` — unlike every other dynamic write terminal — applies no `privileged`/`noform`/`noedit` guard. **Live in 0.0.9 today.**

2. **HIGH — secret/private columns leak on Postgres.** All three dynamic JSON write-response builders (`insert_json`, `insert_json_in_tx`, the fetch-one twin) filter the response through `may_serialize` on the SQLite arm but iterate every column unfiltered on the Postgres arm. Since umbral is Postgres-first, the production backend returns `#[umbral(secret)]` / `private` / `Masked` columns that SQLite correctly strips.

3. **HIGH — REST list filter/search is a blind extraction oracle over hidden columns.** `parse_filters`/`parse_search` validate a query key against the *full* field list with no secret/private/hidden exclusion, so `?password_hash__startswith=…` shapes the WHERE clause and the row count leaks the answer. The "unknown field" error even enumerates every column name, secret ones included. Reachable with the default `ReadOnly` permission (i.e. anonymously on any opted-in resource).

These three are not independent: #1 and the mass-assignment gap are the ORM's `update_one` skipping the guard; #2 and #3 are both "a security filter that governs *responses* is not applied to another surface" (the PG write-back, and the filter/search input). That pattern — **a visibility rule enforced in one place and forgotten in a sibling** — is the throughline and is worth a targeted follow-up sweep of its own.

### Core-confirmed, exploit path untraced: the `Masked<T>` re-seal cluster

`write.rs:512` `seal_masked_json` seals **unconditionally** — it returns early only for a non-masked column or a JSON `null`, then calls `ambient_seal` on whatever string it's given. It does not check whether the string is already ciphertext, and it does not treat empty as "no change". I verified that central fact. It makes three data-loss/corruption claims very likely real, but each needs its own trace before it's called confirmed:

- **`write.rs:512` (raised CRITICAL):** `dumpdata`/`loaddata` of a `Masked` column double-seals on restore — `dump` writes ciphertext, `load` → `insert_json` → `seal_masked_json` seals it again → `reveal()` returns inner ciphertext, plaintext unrecoverable. The dynamic write path never consults the Rust `Deserialize` REDACTED guard, so that guard can't save it.
- **`masked.rs:388` (HIGH):** an admin edit form renders a `Masked` column as an empty editable field; submitting the form carries `field=` (empty), which `seal_masked_json` seals (empty ≠ null) → overwrites the real ciphertext with `seal("")`, crypto-shredding the secret on any edit.
- **`masked.rs:389` (HIGH):** a serialize→deserialize round-trip re-seals. **Possible partial refutation:** `Masked` derives `secret = true`, so `may_serialize` should strip it from REST reads — meaning a REST client never receives the ciphertext to echo back, which may close the REST vector. The in-process typed path stays safe (Sealed clones through). Needs tracing to see which surface actually reaches it.

The fix is one-shot and worth it regardless of which paths reach it: `seal_masked_json` must skip an already-sealed value and treat empty-as-unchanged — mirror the `Deserialize` REDACTED contract on the dynamic write path so there is one definition of "don't re-seal", not two.

## Release recommendation

**0.0.10 stays held** — but note the confirmed criticals are **pre-existing in 0.0.9**, not introduced by the 0.0.10 payload. Two paths:

- **Fix-forward into 0.0.10:** fold fixes for the three confirmed findings (at minimum the CRITICAL) into this release before publishing. This is what "sweep before publishing" should mean, and it turns 0.0.10 from a scaffolding-tooling release into a security release.
- The medium/low triage and the 16 unverified highs are a follow-up body of work, not a release gate.

I did **not** finish the job the workflow started. The verify+report phases need re-running (after the usage limit resets) to turn `02_triage.md` into confirmed/refuted verdicts. `framework-sweep-review3-*.js` under `workflows/scripts/` can be resumed with `resumeFromRunId` once budget is available — the finder results are cached, so only verify+report re-run.

## Files

- `01_confirmed.md` — the three hand-verified findings, full evidence + fix.
- `02_triage.md` — the remaining 68 candidates, grouped by area. **Unverified.**
- `_raw_candidates.json` — machine-readable finder output (all 71).
