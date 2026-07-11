# Generated-client demos

These directories are **not** the shop's live client (that's `api/`). They're
side-by-side outputs of `umbral gen-client` under different REST configs, kept so
you can diff how the generated `client.ts` adapts. They're safe to delete.

| Dir | What it shows |
|---|---|
| `api-baseline/` | A backup of `api/` (the shop's live client) at the time these were made. |
| `api-pagenumber/` | `PageNumberPagination` — envelope gains `total_pages`/`current_page`/`page_size`/`next`/`previous`; builder gains `.page(n)` / `.pageSize(n)`. |
| `api-limitoffset/` | `LimitOffsetPagination` — envelope gains `limit`/`offset`/`next`/`previous`; builder gains `.limit(n)` / `.offset(n)`. |
| `api-custom/` | A **custom cursor paginator** that declared its shape via `Pagination::schema()`. Envelope is fully typed with *different properties* — `next_cursor` / `prev_cursor` (`string \| null`) and `has_more` (`boolean`) — and the builder gains `.cursor(v)` / `.pageSize(v)`. No `results`/`count` assumed. |
| `api-mixed-pk-demo/` | Per-model `id` typing: `id: number` for an i64 PK, `id: string` for Uuid / String-slug PKs. |

Only the envelope, the pagination builder methods, and the auth block differ
across the pagination variants — `models.ts` and the per-model
`Filters`/`Ordering`/`Create`/`Update` types are identical, because pagination
and auth are model-independent.

All variants also show the **adaptive auth** surface: `token` (prefix from the
declared security scheme), `apiKey` (header from the scheme), and the
`getAuthHeaders()` dynamic hook. See `documentation/docs/v0.0.1/rest/typescript-client.mdx`.
