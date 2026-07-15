---
name: specra-doc-taxonomy
description: Use when adding or organizing pages in the umbral docs site (documentation/, a Specra + SvelteKit site) — explains tags vs tab_group vs badge vs category, and which one actually partitions the sidebar.
---

# Specra doc taxonomy: tags ≠ tab_group ≠ badge

## Context

The user-facing docs under `documentation/docs/v0.0.1/` are a Specra (SvelteKit + MDX) site. Specra has FIVE separate taxonomy dimensions and their names are misleading — `tags:` is the one that sounds most important but does the least. This skill records which lever does what, learned while wiring the 5-tab layout in 2026-07.

## Approach

Frontmatter/config levers, weakest to strongest partitioning power:

- **`tags: [a, b]`** (page frontmatter) — DECORATIVE ONLY. Renders rounded chips at the bottom of the article (gated by `features.showTags`, default true) + feeds MeiliSearch when search is on. **No tag index page, no sidebar filtering, no grouping.** Don't reach for `tags` expecting navigation.
- **`badge: new`** (page frontmatter or `_category_.json`) — a small status pill on the sidebar row. Presets: `new, updated, beta, experimental, pre-release, deprecated, coming-soon`; or `{ text, color }`; or an array. Needs specra ≥ 0.2.68. Good for "New in 0.0.x".
- **`_category_.json`** (per folder) — sidebar grouping/order: `label`, `position`, `collapsed`, `icon`, `link`, and it can carry `tab_group` + `badge` for the whole folder.
- **`tab_group: <id>`** (page frontmatter OR `_category_.json`) — THE real partitioner. Renders a sticky tab bar under the header that **filters the sidebar** to the active tab. File-level beats folder-level. A page/folder with no `tab_group` falls into the FIRST declared tab group.
- **versions** (`_version_.json`) / **products** (`_product_.json`) — the version + product switchers above everything.

To use `tab_group` you must ALSO declare the tabs in `specra.config.json` under `navigation.tabGroups`:

```json
"navigation": {
  "tabGroups": [
    { "id": "guides", "label": "Guides", "icon": "book-open" },
    { "id": "data",   "label": "Data",   "icon": "database" }
  ]
}
```

Then assign each folder in its `_category_.json`: `{ "label": "ORM", "position": 3, "tab_group": "data" }`, and each root-level page in frontmatter: `tab_group: guides`.

## umbral's current 5-tab scheme (set 2026-07)

| Tab (`id`) | icon | areas |
|---|---|---|
| Guides (`guides`) | book-open | getting-started, examples, idioms, about, features |
| Data (`data`) | database | orm, migrations, backends |
| Web (`web`) | globe | web, templates, rest, graphql, realtime |
| Plugins (`plugins`) | puzzle | plugins, auth, admin |
| Operations (`operations`) | server | deployment, observability, testing, cli |

**When you add a new doc area/page, set its `tab_group`** — otherwise it silently lands in `guides` (the first tab).

## Verifying a deploy

- Build locally with `yarn build` (matches `deploy-docs.yml`; static Vite build, not a dev server — safe to run). It writes to `build/` (gitignored) and lists any broken internal links as `[404]` warnings without failing.
- The tab bar hydrates from EMBEDDED JS data, not server-rendered text. So grepping deployed HTML for `>Guides<` FAILS even when tabs work — grep for `tabGroups`, the tab `id`s (`guides`), or `label:"Guides"` instead.
- Deploy is manual: `gh workflow run deploy-docs.yml` → peaceiris pushes to `gh-pages` → https://dalmasonto.github.io/umbral/. Expect CDN propagation lag of a minute or two.

## Pitfalls

- Reaching for `tags:` to build navigation — it can't; use `tab_group`.
- Forgetting to declare a `tab_group` id in `specra.config.json` `navigation.tabGroups` — the assignment then silently no-ops.
- `documentation/` carries BOTH `yarn.lock` and `pnpm-lock.yaml`; the CI (`deploy-docs.yml`) uses **yarn** (`--frozen-lockfile` against `yarn.lock`). Keep yarn.lock authoritative or the deploy breaks.
- Pre-existing broken cross-links in the content (e.g. `auth/user-in-templates` linking to `/umbral/v0.0.1/...` missing the `/docs/` segment) show as build 404 warnings but don't fail the build — a separate cleanup, not a deploy blocker.

## See also

- The engine source lives OUTSIDE this repo at `/home/dalmas/E/projects/documentation-system/specra/specra-docs` (the `specra` npm package). Frontmatter schema: `node_modules/specra/dist/mdx.d.ts`. Tab-group teaching doc: that repo's `docs/v1.0.0/tab-groups.en.mdx`.
- CLAUDE.md → "Documentation" + "ship a feature, ship its doc page".
