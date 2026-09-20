# gaps6 — shipped (archive)

Closed entries from `planning/gaps6.md`, kept verbatim under their original numbers with a shipped note appended. The active file keeps only the one-line stub. See the gap-tracker convention in `CLAUDE.md`.

---

6. **`phase2_datatable::test_search_keystroke_replaces_history_not_push` is flaky** — `plugins/umbral-admin/tests/phase2_datatable.rs:310`. Fails intermittently (~1 in 3) even in isolation; passes on retry. Pre-existing (traces to commit 9657929f, predates the ORM heavy-relations branch) and unrelated to the ORM work.

**SHIPPED** (2026-09-20). The original write-up guessed a shared-router/session harness race; that theory didn't hold — the test fails at the same rate run completely alone, with a fresh router/pool/session per process. The real cause: `rows_fragment` (`plugins/umbral-admin/src/handlers/list.rs`) rebuilt the reflected `HX-Replace-Url`/`HX-Push-Url` nav URL by re-serializing the request's `Query<HashMap<String, String>>` with `serde_urlencoded::to_string(&params)`. `std::collections::HashMap`'s default hasher is randomized per-instance (`RandomState`), so the same two keys (`search`, `page_size`) iterate in a different order on different runs — `search=alp&page_size=25` about 2/3 of the time, `page_size=25&search=alp` the rest. Only this test asserted the exact multi-param string, so it alone caught the non-determinism.

Fix: added an `axum::extract::RawQuery` extractor (same pattern already used in `dashboard.rs::dashboard_widget_export`) alongside the existing `Query<HashMap<_,_>>`, and used the raw query string verbatim for both the non-HTMX redirect target and the `HX-Replace-Url`/`HX-Push-Url` nav URL — bytes the client actually sent, not a re-encoded HashMap. `params` (the parsed `HashMap`) is unchanged and still used for all the actual filter/search/sort logic; only the URL-reflection sites were touched. Verified with 30/30 passes looped on the single test plus a clean full-file run (13/13).
