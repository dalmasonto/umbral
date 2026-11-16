# review_4 · <crate-name>

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | <N> LOC (`<crates/… or plugins/…>`) |
| **Reviewer** | <agent label> |
| **Verdict** | healthy / minor work / needs attention / urgent |
| **Findings** | X critical · Y high · Z medium · W low · V info — N new, M known |

Size = all `.rs` under the crate (src + tests, excluding `target/`). N new = every numbered finding, INFO included; M known = `[open]` entries in the Known section (never numbered).

## Summary
Three to six bullets: what the crate does, overall state, the biggest risks, and what a reader should fix first. Written for someone who will not read the whole file.

## Findings
Ordered by severity (CRITICAL → INFO). Every numbered finding is **new** — anything already tracked goes only in the Known section below, never numbered. Each carries: exact location, quoted evidence, a concrete impact scenario, a fix sketch. **CRITICAL and HIGH must be self-verified** (see the rubric skill: re-read the code, try to refute, trace one concrete scenario); the tag may also appear on MEDIUM/LOW when genuinely applied, and must be absent otherwise. INFO findings may use the short form — **Where** + one observation line; do not manufacture Impact/Fix boilerplate for an observation.

### XX-1 · CRITICAL · security · self-verified
**<One-line claim — what is broken, stated as a fact.>**
- **Where**: `path/file.rs:123-140`
- **Evidence**:
  ```rust
  // 2–6 real lines, quoted verbatim from the tree
  ```
- **Impact**: <Concrete scenario: inputs/state → wrong outcome. For security: who can do what. For perf: under what load, what it costs. "Could be slow" is not an impact.>
- **Fix**: <One to three sentences. Where the real fix lives — not a full patch.>

### XX-2 · HIGH · perf/alloc · self-verified
…

### XX-3 · MEDIUM · drift
…

### XX-4 · LOW · docs
…

### XX-5 · INFO · ergonomics
…

## Strengths
What this crate does well — mandatory, and be specific (name the pattern/helper and why it is good). This keeps the review fair and tells future maintainers what to preserve.

## Clean areas checked
Areas looked at hard and found sound, one line each, with the reason (e.g. "SQL injection: every value bound, every identifier an `Alias`", "no `unsafe`, no `unwrap` reachable from a request path"). Renders the coverage legible so the next sweep does not redo it.

## Known issues already tracked elsewhere (not counted in the totals)
Tag each entry `[open]`, `[fixed/verified held]`, or `[partially held — see X-N]`; the banner's "M known" counts `[open]` and `[partially held]` entries (for multi-item batch entries, count per entry-line).
- `gaps6 #20` — <title> `[open]`
- `review_3/codex` — <title> `[fixed/verified held]`

## Out-of-scope notes
Observations that belong to *other* crates encountered while reading this one. Keep to one line each; the wave assembler routes them.
