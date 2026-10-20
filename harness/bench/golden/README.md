# golden — characterization snapshots of the instrumented app

These are **characterization snapshots**, not a spec. They are a record of how `harness/backend` responded *as it was* before the lib/bin restructure (Task 2) and the domain extraction (Task 3), so those refactors cannot silently change a response shape.

Each `<route>.json` pins three things about one route:

- the HTTP **status**,
- the set of **`Server-Timing` metric names** (a sorted list — the durations are deliberately excluded, they vary every request),
- a **sha256 of the response body** after the volatile fields are normalised out.

They are not an assertion that the behaviour is *correct*. They assert it is *unchanged*: if a refactor alters a body, adds or renames a `Server-Timing` metric, or changes a status code, the diff fails loudly instead of the regression riding along unnoticed.

## Capture

Against a **running** binary (see `harness/backend/README.md` for the seed env vars; port 8113 is the harness's):

```bash
harness/bench/golden/capture.sh 8113 harness/bench/golden/before
```

To use it as a gate after a refactor, capture to a scratch directory and diff:

```bash
harness/bench/golden/capture.sh 8113 /tmp/golden-after
diff -r harness/bench/golden/before /tmp/golden-after
```

`diff -r` exits non-zero on any difference, so an empty diff (exit 0) is the pass condition.

## The rules that make it stable

Two things have to hold, and both are easy to get wrong:

- **Same database.** The captures are compared against the live DB with the same seed env vars. Ids and counts in the bodies are stable for a fixed dataset; booting with a different seed (or letting the resumable seed top up to a different target) changes them legitimately and is not a regression. Restarting the binary is fine — the golden was verified to match again after a full restart.
- **Volatile values are normalised before hashing** — row ids, `created_at` values, and ISO-8601 timestamps are replaced (`capture.sh`'s three `sed` rules). Anything nondeterministic that escapes those rules makes the "after" comparison a false failure, so the script is verified stable by capturing three times from one process and once after a restart, then diffing all four.

## Files

- `capture.sh` — the capture script (`capture.sh <port> <outdir>`).
- `before/` — the pre-refactor snapshot. **Not committed** (`*.json` under this directory is gitignored; see `harness/.gitignore`), because it records a running process against a local database.
