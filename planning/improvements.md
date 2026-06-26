# Imrpovements to be made to the system

Improvements
1. Query speed optimizations - 2026-06-02T00:54:08.766778Z  WARN sqlx::query: slow statement: execution time exceeded alert threshold summary="INSERT INTO session (id, …" db.statement="\n\nINSERT INTO session (id, user_id, data, created_at, expires_at) VALUES (?, ?, ?, ?, ?)\n" rows_affected=1 rows_returned=0 elapsed=1.741646396s elapsed_secs=1.7416463960000002 slow_threshold=1s
2026-06-02T00:54:09.320865Z  WARN sqlx::query: slow statement: execution time exceeded alert threshold summary="INSERT INTO session (id, …" db.statement="\n\nINSERT INTO session (id, user_id, data, created_at, expires_at) VALUES (?, ?, ?, ?, ?)\n" rows_affected=1 rows_returned=0 elapsed=3.141787279s elapsed_secs=3.141787279 slow_threshold=1s
2026-06-02T00:54:09.693453Z  WARN sqlx::query: slow statement: execution time exceeded alert threshold summary="INSERT INTO session (id, …" db.statement="\n\nINSERT INTO session (id, user_id, data, created_at, expires_at) VALUES (?, ?, ?, ?, ?)\n" rows_affected=1 rows_returned=0 elapsed=1.741824915s elapsed_secs=1.741824915 slow_threshold=1s
2026-06-02T00:54:09.784737Z  WARN sqlx::query: slow statement: execution time exceeded alert threshold summary="SELECT COUNT(\"*\") FROM \"article\"" db.statement="" rows_affected=0 rows_returned=1 elapsed=1.029511906s elapsed_secs=1.029511906 slow_threshold=1s
2026-06-02T00:54:10.767442Z  WARN sqlx::query: slow statement: execution time exceeded alert threshold summary="SELECT COUNT(\"*\") FROM \"article\"" db.statement="" rows_affected=0 rows_returned=1 elapsed=1.429625264s elapsed_secs=1.429625264 slow_threshold=1s
2026-06-02T00:54:11.107393Z  WARN sqlx::query: slow statement: execution time exceeded alert threshold summary="INSERT INTO session (id, …" db.statement="\n\nINSERT INTO session (id, user_id, data, created_at, expires_at) VALUES (?, ?, ?, ?, ?)\n" rows_affected=1 rows_returned=0 elapsed=4.346439276s elapsed_secs=4.346439276 slow_threshold=1s

Read the interpretation of this at `./bugs/helper-files/server-speed-report.md`

We will need to populate our test database with almost 1/5/10/20/100 million records with faked long data to do proper testing using `ApacheBench` Ref - ./bugs/tests/webservertesting.md. We will finish doing the ecomerce and contentplugin example from `tests` folder and use them to test the system end to end to see bottlenecks and improvements.

## 2026-06-02 — Improvement 1 closed

Root cause: `umbral::db::connect_sqlite` used `SqlitePool::connect(url)` with zero PRAGMAs. SQLite then ran the pool in `journal_mode = DELETE` + `synchronous = FULL` — the safe defaults that serialise every concurrent writer behind a full-file lock. The user-visible symptom was sessions and writes taking 1-4 seconds per INSERT once any other connection touched the file. `SessionLayer` creates an anonymous session on every cookie-less request, so even an unauthenticated browse hammered the writer lock.

Fix lands in `crates/umbral-core/src/db.rs`: `connect_sqlite` now applies `journal_mode = WAL`, `synchronous = NORMAL`, `busy_timeout = 5000ms`, `foreign_keys = ON` via `SqliteConnectOptions`, plus turns the per-statement INFO logging off (it added measurable overhead at high RPS). Five regression tests in `crates/umbral-core/tests/sqlite_pragmas.rs` lock the configuration so a future "just call SqlitePool::connect" tweak can't quietly undo this.

Measured on derive-demo (`/` endpoint, anonymous browse triggers session INSERT + article COUNT):

| Metric | Before (reported) | After (this fix) |
|---|---|---|
| INSERT INTO session elapsed | 1.7 - 4.3 s | < 50 ms (no slow-query warnings) |
| Throughput at concurrency 50 | ~80 req/s | ~3,140 req/s |
| Throughput at concurrency 200 | (would have timed out) | ~7,590 req/s |
| p99 latency | > 3,700 ms | 44 ms |
| Worst-case (p100) latency | 9,393 ms | 78 - 92 ms |
| Failed requests | 1 (length mismatch) | 0 |

The COUNT(*) slowness was a knock-on of the writer lock: SELECT was waiting in line behind the session INSERTs holding the journal lock. WAL eliminates the reader-vs-writer block, so reads stopped queueing on writes.

Next improvements (still open):
- Populate test DBs with 1/5/10/20/100M faked rows for sustained-throughput testing. The PRAGMA fix removes the easy bottleneck; finding the next one needs realistic data volume.
- Finish the ecommerce + content-plugin examples under `bugs/tests/` to exercise more complex query shapes (joins, transactions, FK cascades).
