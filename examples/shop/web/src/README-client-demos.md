# Generated-client demos

These directories are **not** the shop's live client (that's `api/`). They're
side-by-side outputs of `umbral gen-client` under different REST configs, kept so
you can diff how the generated client adapts. They're safe to delete.

`gen-client` emits **two files**:

- `client.js` — the whole runtime, one self-contained ES module. No imports, no
  build step: a browser can load it from `<script type="module">`, and any
  bundler takes it as-is.
- `client.d.ts` — every type (rows, choice unions, per-model
  `Filters`/`Ordering`/`Create`/`Update`, the paginator's envelope, the class
  declarations).

There's no `.ts` runtime because TypeScript types **erase** — the row and filter
types compile to zero JavaScript. So the runtime exists exactly once, and
`import { Umbral } from "./api/client"` still type-checks fully (TS resolves the
`.d.ts`, the bundler resolves the `.js`).

| Dir | What it shows |
|---|---|
| `api-pagenumber/` | `PageNumberPagination` — envelope gains `total_pages`/`current_page`/`page_size`/`next`/`previous`; builder gains `.page(v)` / `.pageSize(v)`. |
| `api-limitoffset/` | `LimitOffsetPagination` — envelope gains `limit`/`offset`/`next`/`previous`; builder gains `.limit(v)` / `.offset(v)`. |
| `api-custom/` | A **custom cursor paginator** that declared its shape via `Pagination::schema()`. Envelope is fully typed with *different properties* — `next_cursor` / `prev_cursor` (`string \| null`) and `has_more` (`boolean`) — and the builder gains `.cursor(v)` / `.pageSize(v)`. No `results`/`count` assumed. |
| `api-mixed-pk-demo/` | Per-model `id` typing in `UmbralResources`: `id: number` for an i64 PK, `id: string` for Uuid / String-slug PKs. |

Only the envelope, the paging builder methods, and the auth block differ across
the pagination variants — the per-model `Filters`/`Ordering`/`Create`/`Update`
types are identical, because pagination and auth are model-independent.

All variants show the **adaptive auth** surface too: `token` (Authorization
prefix read from the declared security scheme), `apiKey` (header read from the
scheme), and the `getAuthHeaders()` dynamic hook.

## Realtime is delegated, not duplicated

`client.on(table, handlers, { group })` does **not** open its own `EventSource`.
It loads the realtime plugin's already-served `/realtime/client.js` and routes
through `umbral.realtime.model(...)`, so it inherits ONE SSE connection shared
across every tab (via `SharedWorker`), presence, and graceful degradation. A
per-subscription `EventSource` would open one connection per model and exhaust
the browser's ~6-connections-per-origin cap at six subscriptions.

## Note on `api/`

`api/` is the shop's live client and was generated **before** the `.js` + `.d.ts`
change, so it still holds the old `models.ts` + `client.ts` pair. Refresh it with:

```bash
cd examples/shop && cargo run -- gen-client --out web/src/api/
```
