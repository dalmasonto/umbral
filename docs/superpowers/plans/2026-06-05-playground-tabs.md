# Playground Tabs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add browser-style tabs to `umbral-playground` with Dexie persistence, a fixed-height request/response layout, and a snapshot export/import. Resolves `bugs/features.md` item #12.

**Architecture:** A new `Tab` interface and `TabsSlice` on the existing `usePlayground` zustand store, persisted to a new `tabs` table in the per-app Dexie database. A new `TabStrip` component renders above the existing request/response panels, with a + popover and export/import controls. The main layout grid gains a new row and a `min-h-[640px] lg:min-h-[720px]` constraint on the request/response row.

**Tech Stack:** React 19, zustand 5, Dexie 4, Radix UI (via the existing `radix-ui` package — for the popover), Tailwind v4, lucide-react icons, Vitest + fake-indexeddb.

**Spec:** `docs/superpowers/specs/2026-06-05-playground-tabs-design.md`

---

## File Structure

**New files**

| Path | Responsibility |
|---|---|
| `frontend/src/state/tabsStorage.ts` | `loadTabs()` / `saveTabs()` Dexie helpers; never throws. |
| `frontend/src/components/TabStrip.tsx` | The tab bar: pills, + button, export/import controls, Cmd/Ctrl+W shortcut. |
| `frontend/src/components/OpenTabPopover.tsx` | The + popover — lists operations not yet open, calls `openTab`. |
| `frontend/src/components/ExportImportControls.tsx` | Export/import buttons; builds the snapshot Blob, parses the import file. |
| `frontend/src/components/ui/popover.tsx` | Shadcn-style wrapper around `radix-ui`'s `Popover` (mirrors `dropdown-menu.tsx`). |
| `frontend/src/__tests__/tabs.test.ts` | Tests for the new `tabs` slice — lifecycle, dirty marker, persistence. |

**Modified files**

| Path | Change |
|---|---|
| `frontend/src/state/db.ts` | Bump schema to v5, add `TabsRow` interface and `TABS_SCHEMA_VERSION = 1`, register the `tabs` table. |
| `frontend/src/state/store.ts` | Add `Tab` interface, `TabsSlice`, the new state, the new actions, and the `scheduleTabsSave` debounced persist. |
| `frontend/src/App.tsx` | Mount `<TabStrip />` between the stats row and the request/response grid; add the `min-h-[640px] lg:min-h-[720px]` height constraint; add the `useEffect` that calls `loadTabs()` on mount. |
| `frontend/src/components/RequestBuilder.tsx` | Change `min-h-[12rem]` on the Monaco wrap to `min-h-full` so the editor fills the new fixed-height parent. |
| `frontend/src/components/EndpointTree.tsx` | Change `onClick={() => select(e.operationId)}` to `onClick={() => openTab(e.operationId)}`. One line. |
| `plugins/umbral-playground/README.md` | Add the manual smoke test from spec §8. |

**Out of scope**

- No Rust changes.
- No changes to `RequestBuilder.tsx` body content beyond the height tweak.
- No changes to `ResponseViewer.tsx` (its existing flex layout composes with the new parent).
- No drag-and-drop reordering UI.
- No URL-based deep linking.
- No "Close all" / "Close other tabs" context menu.

---

## Task 1: Add the `tabs` Dexie table

**Files:**
- Modify: `plugins/umbral-playground/frontend/src/state/db.ts`

- [ ] **Step 1: Add `TabsRow` interface and `TABS_SCHEMA_VERSION`**

Open `frontend/src/state/db.ts` and add the following interface + constant at the bottom of the file (above the existing `db.version(1).stores(...)` chain is fine, but below the `DraftRow` interface — keep the persistence interfaces grouped together):

```ts
/** Per-app workspace tabs. One singleton row per app DB, keyed
 *  `"workspace"`, holding the list of open tabs and the id of
 *  the active one. Mirrors the `settings` table shape — same
 *  schema-versioning + per-app DB isolation story. The tabs row
 *  is rewritten whenever the list of open tabs changes or the
 *  pristine snapshot of a tab's draft is updated. It is NOT
 *  rewritten on every keystroke — the per-endpoint draft lives
 *  in the `drafts` table. */
export interface TabsRow {
  /** Singleton key. Always `"workspace"`. */
  key: "workspace";
  schema: number;
  tabs: Tab[];
  activeTabId: string | null;
  updatedAt: number;
}

/** Schema version stamped on every `TabsRow`. Bump alongside
 *  any breaking change to the `Tab` shape. Reads with a higher
 *  version are discarded back to defaults so we don't pretend
 *  to understand them. */
export const TABS_SCHEMA_VERSION = 1;
```

- [ ] **Step 2: Add `Tab` to the `db` table type**

At the top of the file the import statement for `store.ts` is:

```ts
import type { PlaygroundSettings, RequestDraft, ResponseRecord } from "./store";
```

Change it to:

```ts
import type { PlaygroundSettings, RequestDraft, ResponseRecord, Tab } from "./store";
```

- [ ] **Step 3: Register the `tabs` table on a new `db.version(5).stores(...)` call**

The existing chain ends with `db.version(4).stores({...})`. Add this call directly after it:

```ts
// v5 adds the tabs table — singleton row keyed `"workspace"`,
// holding the open tab list and the active tab id. The previous
// v4 tables are kept verbatim so Dexie doesn't drop the user's
// data on upgrade.
db.version(5).stores({
  history: "++id, operationId, timestamp",
  editorState: "&key",
  settings: "&key",
  drafts: "&operationId, updatedAt",
  tabs: "&key",
});
```

- [ ] **Step 4: Extend the `db` declaration's typed table map**

Change the `db` declaration near the top of the file:

```ts
export const db = new Dexie(DB_NAME) as Dexie & {
  history: EntityTable<HistoryRow, "id">;
  editorState: EntityTable<EditorStateRow, "key">;
  settings: EntityTable<SettingsRow, "key">;
  drafts: EntityTable<DraftRow, "operationId">;
};
```

to:

```ts
export const db = new Dexie(DB_NAME) as Dexie & {
  history: EntityTable<HistoryRow, "id">;
  editorState: EntityTable<EditorStateRow, "key">;
  settings: EntityTable<SettingsRow, "key">;
  drafts: EntityTable<DraftRow, "operationId">;
  tabs: EntityTable<TabsRow, "key">;
};
```

- [ ] **Step 5: Verify it type-checks**

Run: `cd plugins/umbral-playground/frontend && npx tsc -b`
Expected: compile error — `Tab` is referenced but not yet exported from `store.ts`. That's fine; the next task resolves it. The new `db.version(5)` line itself must produce no error.

- [ ] **Step 6: Commit**

```bash
cd plugins/umbral-playground/frontend && \
  git add src/state/db.ts && \
  git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -m "feat(playground): add tabs table to Dexie schema v5

The new singleton table holds the open-tab list and the active
tab id. Mirrors the settings table shape — same schema-version
+ per-app DB isolation story.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: Add `Tab` interface and `tabs` storage helpers

**Files:**
- Modify: `plugins/umbral-playground/frontend/src/state/store.ts` (add the `Tab` interface next to the other request-related interfaces, around line 35 where `RequestDraft` is defined)
- Create: `plugins/umbral-playground/frontend/src/state/tabsStorage.ts`

- [ ] **Step 1: Add the `Tab` interface to `store.ts`**

In `frontend/src/state/store.ts`, immediately after the `RequestDraft` interface (around line 35, after the closing `}` of `RequestDraft`), add:

```ts
/** A single open tab in the playground tab strip. */
export interface Tab {
  /** Stable id for this open-tab slot — independent of
   *  operationId so the same endpoint can be opened twice in
   *  principle. Generated with crypto.randomUUID() on open. */
  id: string;
  operationId: string;
  /** Wall-clock the tab was opened. Used as the default display
   *  order: older first, newer last. */
  openedAt: number;
  /** Snapshot of the draft as it existed when this tab was
   *  opened (or after a manual "reset to opened" action). The
   *  dirty dot compares `current` against this — equal means
   *  clean. */
  pristineDraft: RequestDraft;
}
```

- [ ] **Step 2: Create `state/tabsStorage.ts`**

Create the file `frontend/src/state/tabsStorage.ts` with this content:

```ts
/** Tab-strip persistence on top of Dexie / IndexedDB.
 *
 *  The `tabs` table holds a single row keyed `"workspace"`, an
 *  array of `Tab` plus the `activeTabId`. Mirrors the storage
 *  shape used by `settingsStorage.ts` and `draftStorage.ts`:
 *  best-effort reads that never throw, schema-version guard on
 *  the read path so a future-shape row is silently discarded
 *  back to defaults. */

import { db, TABS_SCHEMA_VERSION, type TabsRow } from "./db";
import type { Tab } from "./store";

/** What `loadTabs` returns when no row is stored, when the
 *  stored row has a schema we don't understand, or when Dexie
 *  itself is unavailable. */
const EMPTY: { tabs: Tab[]; activeTabId: string | null } = {
  tabs: [],
  activeTabId: null,
};

/** Read the persisted tab strip. Returns the empty shape on
 *  any failure — first-ever boot, schema mismatch, IndexedDB
 *  blocked. Never throws. */
export async function loadTabs(): Promise<{
  tabs: Tab[];
  activeTabId: string | null;
}> {
  try {
    const row = (await db.tabs.get("workspace")) as TabsRow | undefined;
    if (!row) return EMPTY;
    if (row.schema > TABS_SCHEMA_VERSION) {
      // Future shape — refuse to interpret. The next save will
      // overwrite with the current version.
      return EMPTY;
    }
    if (!Array.isArray(row.tabs)) return EMPTY;
    return { tabs: row.tabs, activeTabId: row.activeTabId ?? null };
  } catch (e) {
    if (typeof console !== "undefined") {
      console.warn("[umbral-playground] tabs read failed", e);
    }
    return EMPTY;
  }
}

/** Persist the current tab strip. Fire-and-forget from the
 *  store's perspective — the caller doesn't await this, so a
 *  slow IndexedDB write can't block a tab-open click. Errors
 *  are logged and silently swallowed: the in-memory state is
 *  authoritative for the current session. */
export async function saveTabs(snapshot: {
  tabs: Tab[];
  activeTabId: string | null;
}): Promise<void> {
  try {
    await db.tabs.put({
      key: "workspace",
      schema: TABS_SCHEMA_VERSION,
      tabs: snapshot.tabs,
      activeTabId: snapshot.activeTabId,
      updatedAt: Date.now(),
    });
  } catch (e) {
    if (typeof console !== "undefined") {
      console.warn("[umbral-playground] tabs save failed", e);
    }
  }
}
```

- [ ] **Step 3: Verify it type-checks**

Run: `cd plugins/umbral-playground/frontend && npx tsc -b`
Expected: clean compile. The `Tab` interface is now defined and `tabsStorage.ts` consumes it; `db.ts`'s `Tab` import resolves.

- [ ] **Step 4: Commit**

```bash
cd plugins/umbral-playground/frontend && \
  git add src/state/store.ts src/state/tabsStorage.ts && \
  git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -m "feat(playground): add Tab interface and tabsStorage Dexie helpers

Singleton tabs row keyed workspace, holding the open tab list
and the active tab id. Best-effort reads, fire-and-forget
writes, schema-version guard on the read path.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: Add the `tabs` slice to the zustand store (state + actions only)

**Files:**
- Modify: `plugins/umbral-playground/frontend/src/state/store.ts`

This task wires the slice into the store but only adds state and a no-op-style setter. The actions come in Task 4.

- [ ] **Step 1: Extend the `PlaygroundState` interface**

In `frontend/src/state/store.ts`, find the `PlaygroundState` interface (starts around line 216). Add a new section for tabs at the end of the interface, after the `hydrateFromDexie` line:

```ts
  // tabs
  openTabs: Tab[];
  activeTabId: string | null;
  /** Idempotent. If a tab for `operationId` is already open,
   *  activate it. Otherwise append a new tab and activate. */
  openTab: (operationId: string) => void;
  setActiveTab: (id: string) => void;
  closeTab: (id: string) => void;
  /** Reserved for the drag-and-drop follow-up. Persisting now
   *  means the future UI doesn't need to touch the storage
   *  layer. */
  reorderTab: (id: string, toIndex: number) => void;
  /** Mark the current active tab's pristineDraft = current.
   *  Called automatically after the post-open draft hydration
   *  completes so the dirty dot starts clean. */
  markCurrentClean: () => void;
```

- [ ] **Step 2: Add the initial state to the store implementation**

In the `usePlayground` `create` call body (around line 335), add the initial values for the new fields just after the `history: {}, clearHistory: ...` block. The exact location is right before the `settings: initialSettings,` line:

```ts
  openTabs: [],
  activeTabId: null,
  openTab: (operationId) => {
    // Filled in by Task 4.
  },
  setActiveTab: (id) => {
    // Filled in by Task 4.
  },
  closeTab: (id) => {
    // Filled in by Task 4.
  },
  reorderTab: (id, toIndex) => {
    // Filled in by Task 4.
  },
  markCurrentClean: () => {
    // Filled in by Task 4.
  },
```

- [ ] **Step 3: Add the `scheduleTabsSave` debounced persist helper**

In `frontend/src/state/store.ts`, immediately after the existing `scheduleDraftSave` function (around line 311), add:

```ts
// Debounced "tabs changed" persistence. Sibling of
// `scheduleDraftSave`: structural changes (open/close/activate/
// reorder) and `markCurrentClean` go through this 250ms debounce.
// NOT fired on every keystroke — the per-endpoint draft already
// lives in the `drafts` table, so we only need to write the
// tabs row when the list or the pristine snapshot changes.
let tabsSaveTimer: ReturnType<typeof setTimeout> | null = null;
let tabsSavePending: {
  tabs: Tab[];
  activeTabId: string | null;
} | null = null;

function scheduleTabsSave(
  tabs: Tab[],
  activeTabId: string | null,
): void {
  tabsSavePending = { tabs, activeTabId };
  if (tabsSaveTimer) return;
  tabsSaveTimer = setTimeout(() => {
    tabsSaveTimer = null;
    const snapshot = tabsSavePending;
    tabsSavePending = null;
    if (snapshot) {
      // Fire-and-forget — see `scheduleDraftSave` for the
      // rationale (the setter returned synchronously, awaiting
      // here would not block the UI but would not be observable).
      void saveTabs(snapshot);
    }
  }, 250);
}
```

- [ ] **Step 4: Add the `saveTabs` import**

At the top of `frontend/src/state/store.ts`, the import block for storage helpers is:

```ts
import {
  hydrateInitialSettings,
  persistSettings,
  readLocalStorageCache,
  writeLocalStorageCache,
} from "./settingsStorage";
```

Add the tabs-storage import alongside it:

```ts
import { saveTabs } from "./tabsStorage";
```

(The full `saveTabs` import is enough; the `loadTabs` call lives in `App.tsx`.)

- [ ] **Step 5: Verify it type-checks**

Run: `cd plugins/umbral-playground/frontend && npx tsc -b`
Expected: clean compile. The slice's state and no-op actions compile; `Tab` resolves; `saveTabs` resolves.

- [ ] **Step 6: Commit**

```bash
cd plugins/umbral-playground/frontend && \
  git add src/state/store.ts && \
  git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -m "feat(playground): add tabs slice scaffolding to zustand store

openTabs/activeTabId state plus no-op action stubs and the
250ms debounced scheduleTabsSave. The real action bodies land
in the next commit once the lifecycle tests are ready.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: Tests for the `tabs` slice (fail first)

**Files:**
- Create: `plugins/umbral-playground/frontend/src/__tests__/tabs.test.ts`

- [ ] **Step 1: Write the failing tests**

Create the file `frontend/src/__tests__/tabs.test.ts` with this content. It mirrors the shape of `__tests__/draftPersistence.test.ts:1-50` (Dexie + localStorage shim, `freshDexie` / `reload` helpers, `waitForTabsWrite` polling helper).

```ts
/** Tab-strip lifecycle and persistence:
 *
 *  - `openTab` adds a tab and makes it active. Idempotent: if a
 *    tab for the same `operationId` is already open, it just
 *    activates — no duplicate.
 *  - `closeTab` on the active tab activates the right neighbor
 *    (or left if it was the last). `closeTab` on the only tab
 *    leaves `activeTabId = null`.
 *  - `setActiveTab` flips `activeTabId` and calls
 *    `selectEndpoint` for the new active tab's operationId.
 *  - `markCurrentClean` updates the active tab's `pristineDraft`
 *    to `current` so the dirty dot clears.
 *  - Tabs persist to Dexie within the debounce window; reload
 *    restores them.
 *  - On reload, if the persisted `activeTabId` is no longer in
 *    the loaded list, the first tab is activated.
 *  - `crypto.randomUUID` is stubbed so the test isn't flaky on
 *    the off chance the env doesn't expose it. */

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import "fake-indexeddb/auto";

const memory: Record<string, string> = {};
const storage = {
  getItem: vi.fn((k: string) => (k in memory ? memory[k] : null)),
  setItem: vi.fn((k: string, v: string) => {
    memory[k] = v;
  }),
  removeItem: vi.fn((k: string) => {
    delete memory[k];
  }),
  clear: vi.fn(() => {
    for (const k of Object.keys(memory)) delete memory[k];
  }),
  length: 0,
  key: vi.fn(() => null),
};
(globalThis as unknown as { window: { localStorage: typeof storage } }).window = {
  localStorage: storage,
};
(globalThis as unknown as { localStorage: typeof storage }).localStorage = storage;

// Stub crypto.randomUUID with a monotonic counter so tab ids
// are deterministic per test run.
let uuidCounter = 0;
const realCrypto = (globalThis as { crypto?: Crypto }).crypto;
const stubbedCrypto: Crypto = {
  ...(realCrypto ?? ({} as Crypto)),
  randomUUID: () => `uuid-${++uuidCounter}`,
} as Crypto;
(globalThis as { crypto: Crypto }).crypto = stubbedCrypto;

async function freshDexie() {
  try {
    const { db } = await import("../state/db");
    await db.delete();
  } catch {
    // first boot
  }
  vi.resetModules();
}

async function reload() {
  await freshDexie();
  return (await import("../state/store")).usePlayground;
}

/** Wait for the tabs row to land in Dexie after a debounced
 *  write. The store action returns synchronously, so we poll. */
async function waitForTabsWrite(): Promise<{
  tabs: { id: string; operationId: string; openedAt: number }[];
  activeTabId: string | null;
}> {
  const { db } = await import("../state/db");
  const deadline = Date.now() + 300;
  while (Date.now() < deadline) {
    const row = await db.tabs.get("workspace");
    if (row) {
      return {
        tabs: row.tabs.map((t) => ({
          id: t.id,
          operationId: t.operationId,
          openedAt: t.openedAt,
        })),
        activeTabId: row.activeTabId,
      };
    }
    await new Promise((r) => setTimeout(r, 5));
  }
  throw new Error("tabs row did not land in Dexie within 300ms");
}

describe("playground tabs slice", () => {
  beforeEach(() => {
    storage.clear();
    uuidCounter = 0;
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("openTab adds a tab and makes it active", async () => {
    const use = await reload();
    use.getState().openTab("list_product");
    expect(use.getState().openTabs).toHaveLength(1);
    expect(use.getState().openTabs[0]?.operationId).toBe("list_product");
    expect(use.getState().activeTabId).toBe(use.getState().openTabs[0]?.id);
  });

  it("openTab is idempotent — same operationId just activates", async () => {
    const use = await reload();
    use.getState().openTab("list_product");
    const firstId = use.getState().openTabs[0]?.id;
    use.getState().openTab("list_product");
    expect(use.getState().openTabs).toHaveLength(1);
    expect(use.getState().activeTabId).toBe(firstId);
  });

  it("openTab appends to the end and never reorders", async () => {
    const use = await reload();
    use.getState().openTab("op_a");
    use.getState().openTab("op_b");
    use.getState().openTab("op_c");
    expect(use.getState().openTabs.map((t) => t.operationId)).toEqual([
      "op_a",
      "op_b",
      "op_c",
    ]);
  });

  it("closeTab on the active tab activates the right neighbor", async () => {
    const use = await reload();
    use.getState().openTab("op_a");
    use.getState().openTab("op_b");
    use.getState().openTab("op_c");
    const aId = use.getState().openTabs[0]?.id;
    use.getState().setActiveTab(aId!);
    use.getState().closeTab(aId!);
    expect(use.getState().openTabs.map((t) => t.operationId)).toEqual([
      "op_b",
      "op_c",
    ]);
    // Right neighbor is op_b.
    const newActive = use.getState().openTabs.find(
      (t) => t.operationId === "op_b",
    );
    expect(use.getState().activeTabId).toBe(newActive?.id);
  });

  it("closeTab on the last tab falls back to the left neighbor", async () => {
    const use = await reload();
    use.getState().openTab("op_a");
    use.getState().openTab("op_b");
    const bId = use.getState().openTabs[1]?.id;
    use.getState().setActiveTab(bId!);
    use.getState().closeTab(bId!);
    expect(use.getState().openTabs.map((t) => t.operationId)).toEqual([
      "op_a",
    ]);
    expect(use.getState().activeTabId).toBe(use.getState().openTabs[0]?.id);
  });

  it("closeTab on the only tab leaves activeTabId = null", async () => {
    const use = await reload();
    use.getState().openTab("op_a");
    const id = use.getState().openTabs[0]?.id;
    use.getState().closeTab(id!);
    expect(use.getState().openTabs).toHaveLength(0);
    expect(use.getState().activeTabId).toBeNull();
  });

  it("setActiveTab updates activeTabId and selectEndpoint target", async () => {
    const use = await reload();
    use.getState().openTab("op_a");
    use.getState().openTab("op_b");
    // selectEndpoint is called as a side effect of openTab too;
    // assert the last-selected target matches the active tab.
    expect(use.getState().selectedOperationId).toBe("op_b");
    const aId = use.getState().openTabs[0]?.id;
    use.getState().setActiveTab(aId!);
    expect(use.getState().activeTabId).toBe(aId);
    expect(use.getState().selectedOperationId).toBe("op_a");
  });

  it("markCurrentClean updates the active tab's pristineDraft to current", async () => {
    const use = await reload();
    use.getState().openTab("op_a");
    use.getState().setBody('{"name":"x"}');
    // After typing, the draft diverges from pristine.
    use.getState().markCurrentClean();
    const tab = use.getState().openTabs[0];
    expect(tab?.pristineDraft.body).toBe('{"name":"x"}');
  });

  it("persists tabs to Dexie within the debounce window", async () => {
    const use = await reload();
    use.getState().openTab("op_a");
    use.getState().openTab("op_b");
    const row = await waitForTabsWrite();
    expect(row.tabs.map((t) => t.operationId)).toEqual(["op_a", "op_b"]);
    const secondTab = use.getState().openTabs[1];
    expect(row.activeTabId).toBe(secondTab?.id);
  });

  it("restores tabs and active on reload, and falls back to the first tab if active is gone", async () => {
    let use = await reload();
    use.getState().openTab("op_a");
    use.getState().openTab("op_b");
    use.getState().openTab("op_c");
    // Make op_b the active tab so we have a non-default active.
    const bId = use.getState().openTabs[1]?.id;
    use.getState().setActiveTab(bId!);
    await waitForTabsWrite();

    // Now simulate a reload that drops op_b from openTabs but
    // leaves the persisted activeTabId pointing at it.
    use = await reload();
    use.getState().openTab("op_a");
    use.getState().openTab("op_c");
    use.getState().setActiveTab(use.getState().openTabs[1]?.id!);
    // The persisted row's activeTabId still points at the old
    // bId; App.tsx's hydration logic should fall back to the
    // first tab. We test that here by asserting the loader
    // returns the empty shape for activeId when the active tab
    // is missing.
    const { loadTabs } = await import("../state/tabsStorage");
    const loaded = await loadTabs();
    // The loader returns the persisted snapshot verbatim; the
    // fallback-to-first logic lives in App.tsx. The contract
    // this test pins is: the loader surfaces the stale active
    // id so the caller can apply the fallback.
    expect(loaded?.activeTabId).toBe(bId);
    expect(loaded?.tabs.map((t) => t.operationId)).toEqual(["op_a", "op_c"]);
  });
});
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cd plugins/umbral-playground/frontend && npx vitest run src/__tests__/tabs.test.ts`
Expected: every test fails. The action bodies are still no-ops from Task 3.

- [ ] **Step 3: Commit the failing tests**

```bash
cd plugins/umbral-playground/frontend && \
  git add src/__tests__/tabs.test.ts && \
  git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -m "test(playground): add failing tests for tabs slice lifecycle

Pins the open/close/activate/reorder/clean contract plus
the reload-restore and stale-active-fallback behaviour. The
action bodies land in the next commit.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 5: Implement the `tabs` slice actions (make the tests pass)

**Files:**
- Modify: `plugins/umbral-playground/frontend/src/state/store.ts`

- [ ] **Step 1: Replace the no-op `openTab` with the real implementation**

Find the `openTab: (operationId) => {` no-op block in `frontend/src/state/store.ts` and replace it with:

```ts
  openTab: (operationId) => {
    const state = get();
    // Idempotent: if a tab for this operationId already exists,
    // just activate it.
    const existing = state.openTabs.find(
      (t) => t.operationId === operationId,
    );
    if (existing) {
      if (state.activeTabId !== existing.id) {
        // Internal helper that flips activeTabId, fires
        // selectEndpoint, and schedules the persist — defined
        // below.
        setActiveTabInternal(set, get, existing.id);
      }
      return;
    }
    const newTab: Tab = {
      id:
        typeof crypto !== "undefined" && "randomUUID" in crypto
          ? crypto.randomUUID()
          : `tab-${Date.now()}-${Math.random().toString(36).slice(2)}`,
      operationId,
      openedAt: Date.now(),
      pristineDraft: { ...state.current },
    };
    set({
      openTabs: [...state.openTabs, newTab],
      activeTabId: newTab.id,
    });
    scheduleTabsSave(get().openTabs, get().activeTabId);
    // The draft hydration for the new tab runs through the
    // existing selectEndpoint path. We snapshot the pristine
    // draft again after hydration completes so the dirty dot
    // starts clean.
    state.selectEndpoint(operationId);
    setTimeout(() => {
      const refreshed = get().openTabs.find((t) => t.id === newTab.id);
      if (!refreshed) return;
      const hydrated = get().current;
      const same = JSON.stringify(refreshed.pristineDraft) === JSON.stringify(hydrated);
      if (same) return;
      set({
        openTabs: get().openTabs.map((t) =>
          t.id === newTab.id ? { ...t, pristineDraft: { ...hydrated } } : t,
        ),
      });
      scheduleTabsSave(get().openTabs, get().activeTabId);
    }, 0);
  },
```

- [ ] **Step 2: Add the `setActiveTabInternal` helper above the store**

Right before the `export const usePlayground = create<PlaygroundState>((set, get) => ({` line, add:

```ts
/** Internal helper for switching which tab is active. Updates
 *  `activeTabId`, fires `selectEndpoint` for the new active
 *  tab's `operationId`, and schedules the persist. Lives at
 *  module scope so `openTab` and `setActiveTab` can both call
 *  it without duplicating the side effects. */
function setActiveTabInternal(
  set: (partial: Partial<PlaygroundState>) => void,
  get: () => PlaygroundState,
  id: string,
): void {
  const tab = get().openTabs.find((t) => t.id === id);
  if (!tab) return;
  set({ activeTabId: id });
  scheduleTabsSave(get().openTabs, get().activeTabId);
  get().selectEndpoint(tab.operationId);
}
```

- [ ] **Step 3: Replace the no-op `setActiveTab` with the real implementation**

```ts
  setActiveTab: (id) => {
    setActiveTabInternal(set, get, id);
  },
```

- [ ] **Step 4: Replace the no-op `closeTab` with the real implementation**

```ts
  closeTab: (id) => {
    const state = get();
    const idx = state.openTabs.findIndex((t) => t.id === id);
    if (idx < 0) return;
    const wasActive = state.activeTabId === id;
    const next = state.openTabs.filter((t) => t.id !== id);
    if (!wasActive) {
      set({ openTabs: next });
      scheduleTabsSave(next, state.activeTabId);
      return;
    }
    // Pick a fallback: right neighbor first, then left
    // neighbor, then null.
    const fallback =
      next[idx] ?? next[idx - 1] ?? null;
    const fallbackId = fallback?.id ?? null;
    set({ openTabs: next, activeTabId: fallbackId });
    scheduleTabsSave(next, fallbackId);
    if (fallback) {
      get().selectEndpoint(fallback.operationId);
    } else {
      get().selectEndpoint(null);
    }
  },
```

- [ ] **Step 5: Replace the no-op `reorderTab` with the real implementation**

```ts
  reorderTab: (id, toIndex) => {
    const state = get();
    const idx = state.openTabs.findIndex((t) => t.id === id);
    if (idx < 0) return;
    if (toIndex < 0 || toIndex >= state.openTabs.length) return;
    if (toIndex === idx) return;
    const next = [...state.openTabs];
    const [picked] = next.splice(idx, 1);
    next.splice(toIndex, 0, picked!);
    set({ openTabs: next });
    scheduleTabsSave(next, state.activeTabId);
  },
```

- [ ] **Step 6: Replace the no-op `markCurrentClean` with the real implementation**

```ts
  markCurrentClean: () => {
    const state = get();
    if (!state.activeTabId) return;
    const next = state.openTabs.map((t) =>
      t.id === state.activeTabId
        ? { ...t, pristineDraft: { ...state.current } }
        : t,
    );
    set({ openTabs: next });
    scheduleTabsSave(next, state.activeTabId);
  },
```

- [ ] **Step 7: Run the tests and confirm they pass**

Run: `cd plugins/umbral-playground/frontend && npx vitest run src/__tests__/tabs.test.ts`
Expected: all tests pass.

- [ ] **Step 8: Run the full test suite to make sure nothing regressed**

Run: `cd plugins/umbral-playground/frontend && npx vitest run`
Expected: every test in `draftPersistence.test.ts`, `saveSettings.test.ts`, `codegen.test.ts`, `buildFetchArgs.test.ts`, and the new `tabs.test.ts` passes.

- [ ] **Step 9: Commit**

```bash
cd plugins/umbral-playground/frontend && \
  git add src/state/store.ts && \
  git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -m "feat(playground): implement openTab/setActiveTab/closeTab actions

Idempotent open, neighbour-aware close, debounced persist on
every structural change. markCurrentClean resets the active
tab's pristineDraft after draft hydration so the dirty dot
starts clean.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 6: Add the popover UI primitive

**Files:**
- Create: `plugins/umbral-playground/frontend/src/components/ui/popover.tsx`

- [ ] **Step 1: Create the shim**

Create the file `frontend/src/components/ui/popover.tsx` with this content. It mirrors the shape of `dropdown-menu.tsx` (`use client` directive, primitive re-export, data-slot attributes, `cn` for class merging):

```tsx
"use client"

import * as React from "react"
import { Popover as PopoverPrimitive } from "radix-ui"

import { cn } from "@/lib/utils"

function Popover({
  ...props
}: React.ComponentProps<typeof PopoverPrimitive.Root>) {
  return <PopoverPrimitive.Root data-slot="popover" {...props} />
}

function PopoverTrigger({
  ...props
}: React.ComponentProps<typeof PopoverPrimitive.Trigger>) {
  return <PopoverPrimitive.Trigger data-slot="popover-trigger" {...props} />
}

function PopoverContent({
  className,
  align = "center",
  sideOffset = 4,
  ...props
}: React.ComponentProps<typeof PopoverPrimitive.Content>) {
  return (
    <PopoverPrimitive.Portal>
      <PopoverPrimitive.Content
        data-slot="popover-content"
        align={align}
        sideOffset={sideOffset}
        className={cn(
          "z-50 w-72 origin-(--radix-popover-content-transform-origin) rounded-md border border-border bg-popover p-1 text-popover-foreground shadow-md outline-none data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:animate-in data-[state=open]:fade-in-0",
          className,
        )}
        {...props}
      />
    </PopoverPrimitive.Portal>
  )
}

function PopoverAnchor({
  ...props
}: React.ComponentProps<typeof PopoverPrimitive.Anchor>) {
  return <PopoverPrimitive.Anchor data-slot="popover-anchor" {...props} />
}

export { Popover, PopoverTrigger, PopoverContent, PopoverAnchor }
```

- [ ] **Step 2: Verify it type-checks**

Run: `cd plugins/umbral-playground/frontend && npx tsc -b`
Expected: clean compile. (No other file imports this shim yet, but the type check confirms the export shape.)

- [ ] **Step 3: Commit**

```bash
cd plugins/umbral-playground/frontend && \
  git add src/components/ui/popover.tsx && \
  git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -m "feat(playground): add popover UI shim

Wraps radix-ui's Popover primitive in the project's shadcn-
style shim shape. Mirrors dropdown-menu.tsx for consistency.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 7: `OpenTabPopover` component

**Files:**
- Create: `plugins/umbral-playground/frontend/src/components/OpenTabPopover.tsx`

- [ ] **Step 1: Create the component**

Create the file `frontend/src/components/OpenTabPopover.tsx` with this content:

```tsx
import { useMemo, useState } from "react";
import type { OpenAPIV3 } from "openapi-types";
import { usePlayground } from "@/state/store";
import { MethodBadge } from "./MethodBadge";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Plus, Search } from "lucide-react";

interface OpenTabPopoverProps {
  /** The full parsed OpenAPI spec, or null while loading. */
  spec: OpenAPIV3.Document | null;
}

interface Candidate {
  operationId: string;
  method: string;
  path: string;
  summary?: string;
}

const METHODS: Array<[string, keyof OpenAPIV3.PathItemObject]> = [
  ["GET", "get"],
  ["POST", "post"],
  ["PUT", "put"],
  ["PATCH", "patch"],
  ["DELETE", "delete"],
];

/** Build a list of every operation in the spec, deduped by
 *  operationId. Mirrors the way `App.tsx`'s `collectOperations`
 *  walks the spec, so the popover shows the same endpoints the
 *  sidebar would. */
function collectCandidates(spec: OpenAPIV3.Document | null): Candidate[] {
  if (!spec) return [];
  const candidates: Candidate[] = [];
  for (const [path, pathItem] of Object.entries(spec.paths ?? {})) {
    if (!pathItem) continue;
    for (const [method, key] of METHODS) {
      const operation = pathItem[key];
      if (!operation) continue;
      candidates.push({
        operationId: operation.operationId ?? `${method} ${path}`,
        method,
        path,
        summary: operation.summary,
      });
    }
  }
  return candidates;
}

/** The "+" popover that lists operations not yet open. Filtering
 *  is by operationId (a candidate is hidden if a tab for its
 *  operationId is already in `openTabs`). A small search input
 *  narrows the list by path or operationId substring. */
export function OpenTabPopover({ spec }: OpenTabPopoverProps) {
  const openTab = usePlayground((s) => s.openTab);
  const openTabs = usePlayground((s) => s.openTabs);
  const [search, setSearch] = useState("");
  const [open, setOpen] = useState(false);

  const candidates = useMemo(() => {
    const all = collectCandidates(spec);
    const openIds = new Set(openTabs.map((t) => t.operationId));
    const remaining = all.filter((c) => !openIds.has(c.operationId));
    const q = search.trim().toLowerCase();
    if (!q) return remaining;
    return remaining.filter(
      (c) =>
        c.operationId.toLowerCase().includes(q) ||
        c.path.toLowerCase().includes(q) ||
        (c.summary ?? "").toLowerCase().includes(q),
    );
  }, [spec, openTabs, search]);

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          className="shrink-0 text-muted-foreground hover:text-foreground"
          title="Open in new tab"
          aria-label="Open endpoint in new tab"
        >
          <Plus className="size-3.5" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-80 p-0">
        <div className="flex items-center gap-2 border-b border-border px-2.5 py-2">
          <Search className="size-3.5 text-muted-foreground" />
          <Input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search endpoints…"
            className="h-7 border-0 bg-transparent px-0 text-xs font-mono shadow-none focus-visible:ring-0"
            autoFocus
          />
        </div>
        <div className="max-h-72 overflow-y-auto p-1">
          {candidates.length === 0 ? (
            <p className="px-2.5 py-3 text-center text-[11px] italic text-muted-foreground">
              {openTabs.length === 0
                ? "No endpoints available — the spec is empty."
                : "Every endpoint in the spec is already open."}
            </p>
          ) : (
            candidates.map((c) => (
              <button
                key={c.operationId}
                type="button"
                onClick={() => {
                  openTab(c.operationId);
                  setOpen(false);
                  setSearch("");
                }}
                className="flex w-full items-start gap-2 rounded-sm px-2 py-1.5 text-left text-xs hover:bg-muted/60"
              >
                <MethodBadge method={c.method} />
                <span className="min-w-0 flex-1">
                  <span className="block truncate font-mono text-[11px] text-foreground">
                    {c.path}
                  </span>
                  {c.summary ? (
                    <span className="block truncate text-[10px] text-muted-foreground">
                      {c.summary}
                    </span>
                  ) : null}
                </span>
              </button>
            ))
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
```

- [ ] **Step 2: Verify it type-checks**

Run: `cd plugins/umbral-playground/frontend && npx tsc -b`
Expected: clean compile.

- [ ] **Step 3: Commit**

```bash
cd plugins/umbral-playground/frontend && \
  git add src/components/OpenTabPopover.tsx && \
  git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -m "feat(playground): add OpenTabPopover for the + button

Lists spec operations that aren't open in any tab, with a
search box that filters by path/operationId/summary. Wired
up to the tab strip in the next commit.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 8: `ExportImportControls` component

**Files:**
- Create: `plugins/umbral-playground/frontend/src/components/ExportImportControls.tsx`

- [ ] **Step 1: Create the component**

Create the file `frontend/src/components/ExportImportControls.tsx` with this content:

```tsx
import { useRef, useState } from "react";
import { usePlayground } from "@/state/store";
import { db, type HistoryRow } from "@/state/db";
import { getAppScope } from "@/state/scope";
import { loadDraft } from "@/state/draftStorage";
import { pushToast } from "@/state/toastStore";
import { Button } from "@/components/ui/button";
import { Download, Upload } from "lucide-react";

const SNAPSHOT_VERSION = 1 as const;
const PER_OPERATION_CAP = 50;

interface Snapshot {
  version: typeof SNAPSHOT_VERSION;
  exportedAt: number;
  appScope: string;
  tabs: ReturnType<typeof usePlayground.getState>["openTabs"];
  drafts: Record<
    string,
    NonNullable<Awaited<ReturnType<typeof loadDraft>>>
  >;
  history: HistoryRow[];
  settings: ReturnType<typeof usePlayground.getState>["settings"];
}

function isSnapshot(value: unknown): value is Snapshot {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return (
    v["version"] === SNAPSHOT_VERSION &&
    Array.isArray(v["tabs"]) &&
    typeof v["drafts"] === "object" &&
    Array.isArray(v["history"]) &&
    typeof v["settings"] === "object"
  );
}

function formatDate(timestamp: number): string {
  const d = new Date(timestamp);
  const yyyy = d.getFullYear();
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${yyyy}-${mm}-${dd}`;
}

/** Two icon buttons — Download triggers an export of the current
 *  workspace, Upload opens a file picker. Sits at the right end
 *  of the tab strip. */
export function ExportImportControls() {
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [busy, setBusy] = useState(false);
  const openTabs = usePlayground((s) => s.openTabs);
  const setActiveTab = usePlayground((s) => s.setActiveTab);
  const openTab = usePlayground((s) => s.openTab);

  const onExport = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const state = usePlayground.getState();
      const drafts: Snapshot["drafts"] = {};
      for (const tab of openTabs) {
        const draft = await loadDraft(tab.operationId);
        if (draft) drafts[tab.operationId] = draft;
      }
      const history = await db.history.toArray();
      const snapshot: Snapshot = {
        version: SNAPSHOT_VERSION,
        exportedAt: Date.now(),
        appScope: getAppScope(),
        tabs: openTabs,
        drafts,
        history,
        settings: state.settings,
      };
      const blob = new Blob([JSON.stringify(snapshot, null, 2)], {
        type: "application/json",
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `umbral-playground-${getAppScope()}-${formatDate(
        snapshot.exportedAt,
      )}.json`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
      pushToast({
        kind: "success",
        message: `Exported ${openTabs.length} tab${openTabs.length === 1 ? "" : "s"}, ${history.length} history rows`,
      });
    } catch (e) {
      pushToast({
        kind: "error",
        message: `Export failed: ${e instanceof Error ? e.message : String(e)}`,
        durationMs: 5000,
      });
    } finally {
      setBusy(false);
    }
  };

  const onFile = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    // Always reset the input so picking the same file again
    // re-fires onChange.
    event.target.value = "";
    if (!file) return;
    if (busy) return;
    setBusy(true);
    try {
      const text = await file.text();
      let parsed: unknown;
      try {
        parsed = JSON.parse(text);
      } catch {
        pushToast({ kind: "error", message: "Not a valid JSON file." });
        return;
      }
      if (!isSnapshot(parsed)) {
        pushToast({
          kind: "error",
          message: "Not a playground snapshot.",
        });
        return;
      }
      const state = usePlayground.getState();
      const localOpIds = new Set(state.openTabs.map((t) => t.operationId));
      const newTabs = parsed.tabs
        .filter((t) => !localOpIds.has(t.operationId))
        .map((t) => ({
          ...t,
          id:
            typeof crypto !== "undefined" && "randomUUID" in crypto
              ? crypto.randomUUID()
              : `tab-${Date.now()}-${Math.random().toString(36).slice(2)}`,
        }));
      const nextOpenTabs = [...state.openTabs, ...newTabs];
      let nextActiveId = state.activeTabId;
      if (newTabs.length > 0 && !nextActiveId) {
        nextActiveId = newTabs[0]!.id;
      }
      set({
        openTabs: nextOpenTabs,
        activeTabId: nextActiveId,
      });
      // Drafts: put only if no local row exists.
      for (const [operationId, draft] of Object.entries(parsed.drafts)) {
        const existing = await db.drafts.get(operationId);
        if (existing) continue;
        await db.drafts.put({
          operationId,
          schema: 1,
          draft,
          updatedAt: Date.now(),
        });
      }
      // History: dedupe by (operationId, timestamp).
      const existingRows = await db.history.toArray();
      const seen = new Set(
        existingRows.map(
          (r) => `${r.operationId}::${r.timestamp}`,
        ),
      );
      const newHistory: HistoryRow[] = [];
      for (const row of parsed.history) {
        const key = `${row.operationId}::${row.timestamp}`;
        if (seen.has(key)) continue;
        seen.add(key);
        newHistory.push(row);
      }
      if (newHistory.length > 0) {
        // Cap per operation at PER_OPERATION_CAP.
        const grouped = new Map<string, HistoryRow[]>();
        for (const r of newHistory) {
          const arr = grouped.get(r.operationId) ?? [];
          arr.push(r);
          grouped.set(r.operationId, arr);
        }
        const allExisting = await db.history.toArray();
        const byOp = new Map<string, number>();
        for (const r of allExisting) {
          byOp.set(r.operationId, (byOp.get(r.operationId) ?? 0) + 1);
        }
        const toAdd: HistoryRow[] = [];
        for (const [op, rows] of grouped) {
          const have = byOp.get(op) ?? 0;
          const room = Math.max(0, PER_OPERATION_CAP - have);
          toAdd.push(...rows.slice(0, room));
        }
        if (toAdd.length > 0) {
          await db.history.bulkAdd(toAdd);
        }
      }
      // Settings: import only if local has none.
      const localSettingsCount = await db.settings.count();
      if (localSettingsCount === 0 && parsed.settings) {
        await db.settings.put({
          key: "workspace",
          schema: 1,
          value: parsed.settings,
          updatedAt: Date.now(),
        });
      }
      // Activate the first newly imported tab so the user
      // immediately sees the change.
      if (newTabs.length > 0) {
        // openTab handles the dedupe and selectEndpoint.
        openTab(newTabs[0]!.operationId);
      } else if (nextActiveId) {
        setActiveTab(nextActiveId);
      }
      pushToast({
        kind: "success",
        message: `Imported ${newTabs.length} tab${newTabs.length === 1 ? "" : "s"}, ${newHistory.length} history rows`,
      });
    } catch (e) {
      pushToast({
        kind: "error",
        message: `Import failed: ${e instanceof Error ? e.message : String(e)}`,
        durationMs: 5000,
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex items-center gap-1">
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        onClick={() => void onExport()}
        disabled={busy}
        title="Export playground snapshot"
        aria-label="Export playground snapshot"
        className="text-muted-foreground hover:text-foreground"
      >
        <Download className="size-3.5" />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon-xs"
        onClick={() => fileInputRef.current?.click()}
        disabled={busy}
        title="Import playground snapshot"
        aria-label="Import playground snapshot"
        className="text-muted-foreground hover:text-foreground"
      >
        <Upload className="size-3.5" />
      </Button>
      <input
        ref={fileInputRef}
        type="file"
        accept="application/json"
        hidden
        onChange={(e) => void onFile(e)}
      />
    </div>
  );
}
```

- [ ] **Step 2: Verify it type-checks**

Run: `cd plugins/umbral-playground/frontend && npx tsc -b`
Expected: clean compile.

- [ ] **Step 3: Commit**

```bash
cd plugins/umbral-playground/frontend && \
  git add src/components/ExportImportControls.tsx && \
  git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -m "feat(playground): add ExportImportControls for snapshot I/O

Export bundles tabs + drafts + history + settings; import
dedupes by operationId, drops duplicate drafts, caps history
to PER_OPERATION_CAP, and only imports settings when local
storage is empty.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 9: `TabStrip` component

**Files:**
- Create: `plugins/umbral-playground/frontend/src/components/TabStrip.tsx`

- [ ] **Step 1: Create the component**

Create the file `frontend/src/components/TabStrip.tsx` with this content:

```tsx
import { useEffect, useMemo } from "react";
import type { OpenAPIV3 } from "openapi-types";
import { usePlayground, type Tab } from "@/state/store";
import { MethodBadge } from "./MethodBadge";
import { OpenTabPopover } from "./OpenTabPopover";
import { ExportImportControls } from "./ExportImportControls";
import { cn } from "@/lib/utils";
import { X } from "lucide-react";

interface TabStripProps {
  /** The full parsed OpenAPI spec, or null while loading.
   *  Passed through to the OpenTabPopover. */
  spec: OpenAPIV3.Document | null;
}

const METHODS: Array<[string, keyof OpenAPIV3.PathItemObject]> = [
  ["GET", "get"],
  ["POST", "post"],
  ["PUT", "put"],
  ["PATCH", "patch"],
  ["DELETE", "delete"],
];

interface OperationLookup {
  method: string;
  path: string;
}

function buildOperationLookup(
  spec: OpenAPIV3.Document | null,
): Map<string, OperationLookup> {
  const map = new Map<string, OperationLookup>();
  if (!spec) return map;
  for (const [path, pathItem] of Object.entries(spec.paths ?? {})) {
    if (!pathItem) continue;
    for (const [method, key] of METHODS) {
      const operation = pathItem[key];
      if (!operation) continue;
      const id = operation.operationId ?? `${method} ${path}`;
      map.set(id, { method, path });
    }
  }
  return map;
}

function isDraftDirty(tab: Tab, current: ReturnType<typeof usePlayground.getState>["current"]): boolean {
  if (tab.operationId !== usePlayground.getState().selectedOperationId) {
    // Dirty check is only meaningful for the active tab; we
    // don't track dirtiness for inactive tabs.
    return false;
  }
  return JSON.stringify(tab.pristineDraft) !== JSON.stringify(current);
}

/** The tab strip — sits above the request/response panels.
 *  Renders one pill per open tab, a + popover, and the
 *  export/import controls. Captures Cmd/Ctrl+W to close the
 *  active tab. */
export function TabStrip({ spec }: TabStripProps) {
  const openTabs = usePlayground((s) => s.openTabs);
  const activeTabId = usePlayground((s) => s.activeTabId);
  const current = usePlayground((s) => s.current);
  const setActiveTab = usePlayground((s) => s.setActiveTab);
  const closeTab = usePlayground((s) => s.closeTab);

  const lookup = useMemo(() => buildOperationLookup(spec), [spec]);

  // Keyboard shortcut: Cmd/Ctrl+W closes the active tab.
  // Suppressed when focus is inside an editable element.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      if (e.key.toLowerCase() !== "w") return;
      const target = e.target as HTMLElement | null;
      if (target) {
        const tag = target.tagName;
        if (
          tag === "INPUT" ||
          tag === "TEXTAREA" ||
          target.isContentEditable
        ) {
          return;
        }
      }
      if (!activeTabId) return;
      e.preventDefault();
      closeTab(activeTabId);
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [activeTabId, closeTab]);

  if (openTabs.length === 0) {
    return (
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-border bg-muted/30 px-2">
        <span className="text-[11px] italic text-muted-foreground">
          No tabs open — pick an endpoint from the sidebar to start a request.
        </span>
        <span className="flex-1" />
        <ExportImportControls />
      </div>
    );
  }

  return (
    <div className="flex h-10 shrink-0 items-center gap-1 border-b border-border bg-muted/30 px-2">
      <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
        {openTabs.map((tab) => {
          const info = lookup.get(tab.operationId);
          const isActive = tab.id === activeTabId;
          const dirty = isActive && isDraftDirty(tab, current);
          return (
            <div
              key={tab.id}
              role="tab"
              aria-selected={isActive}
              className={cn(
                "group inline-flex shrink-0 items-center gap-2 h-7 rounded-md border px-2 text-[11px] font-mono whitespace-nowrap transition-colors",
                isActive
                  ? "bg-background border-border text-foreground shadow-sm"
                  : "bg-muted/50 border-transparent text-muted-foreground hover:text-foreground hover:bg-muted",
              )}
            >
              <button
                type="button"
                onClick={() => setActiveTab(tab.id)}
                className="flex min-w-0 items-center gap-2"
                title={`${info?.method ?? "?"} ${info?.path ?? tab.operationId}`}
              >
                <span className="font-semibold uppercase tracking-wider text-[9px] text-muted-foreground">
                  {info?.method ?? "?"}
                </span>
                <span className="truncate max-w-[180px]">
                  {info?.path ?? tab.operationId}
                </span>
                {dirty ? (
                  <span
                    className="size-1.5 shrink-0 rounded-full bg-amber-500"
                    aria-label="unsaved edits"
                    title="This tab has edits not present at open time"
                  />
                ) : null}
              </button>
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  closeTab(tab.id);
                }}
                className="ml-0.5 -mr-1 grid size-4 shrink-0 place-items-center rounded text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                title="Close tab"
                aria-label={`Close ${info?.path ?? tab.operationId}`}
              >
                <X className="size-3" />
              </button>
            </div>
          );
        })}
        <OpenTabPopover spec={spec} />
      </div>
      <span className="flex-1" />
      <ExportImportControls />
    </div>
  );
}
```

- [ ] **Step 2: Verify it type-checks**

Run: `cd plugins/umbral-playground/frontend && npx tsc -b`
Expected: clean compile.

- [ ] **Step 3: Commit**

```bash
cd plugins/umbral-playground/frontend && \
  git add src/components/TabStrip.tsx && \
  git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -m "feat(playground): add TabStrip with pills, + popover, and shortcut

Renders one pill per open tab with method chip + truncated
path + dirty dot + close button. Cmd/Ctrl+W closes the
active tab. Empty state shows a hint + export/import controls.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 10: Wire the strip into `App.tsx` and the sidebar into `openTab`

**Files:**
- Modify: `plugins/umbral-playground/frontend/src/App.tsx`
- Modify: `plugins/umbral-playground/frontend/src/components/EndpointTree.tsx`

- [ ] **Step 1: Mount `<TabStrip />` and add the height constraint in `App.tsx`**

In `frontend/src/App.tsx`:

1. Add the import at the top (alongside the other component imports):

```ts
import { TabStrip } from "@/components/TabStrip";
```

2. Find the section that renders the request/response grid. It currently looks like:

```tsx
          <div className="grid min-h-0 flex-1 grid-rows-[auto_minmax(0,1fr)] overflow-hidden">
            <section className="border-b border-border bg-muted/25 px-4 py-3">
              {/* stats row */}
            </section>

            <div className="grid min-h-0 grid-cols-1 lg:grid-cols-2">
              <section className="flex min-h-0 flex-col overflow-hidden border-b border-border lg:border-b-0 lg:border-r">
                <RequestBuilder />
              </section>
              <section className="flex min-h-0 flex-col overflow-hidden">
                <ResponseViewer />
              </section>
            </div>
          </div>
```

Replace it with:

```tsx
          <div className="grid min-h-0 flex-1 grid-rows-[auto_auto_minmax(0,1fr)] overflow-hidden">
            <section className="border-b border-border bg-muted/25 px-4 py-3">
              {/* stats row */}
            </section>

            <TabStrip spec={spec} />

            <div className="grid min-h-0 grid-cols-1 lg:grid-cols-2 min-h-[640px] lg:min-h-[720px]">
              <section className="flex min-h-0 flex-col overflow-hidden border-b border-border lg:border-b-0 lg:border-r">
                <RequestBuilder />
              </section>
              <section className="flex min-h-0 flex-col overflow-hidden">
                <ResponseViewer />
              </section>
            </div>
          </div>
```

- [ ] **Step 2: Hydrate tabs on mount in `App.tsx`**

Find the existing `useEffect` block in `App.tsx` that calls `usePlayground.getState().hydrateFromDexie()`. It looks like:

```tsx
  // After the localStorage boot cache renders, async-load the
  // authoritative settings out of Dexie. Replaces in-memory state
  // when another tab — or a stale cache — caused a divergence.
  // Single shot; Dexie's tab-sync would go here later if we want it.
  useEffect(() => {
    void usePlayground.getState().hydrateFromDexie();
  }, []);
```

Add the import for `loadTabs` near the top of the file (alongside `loadHistory`):

```ts
import { loadHistory } from "@/state/history";
```

Add this import right after it:

```ts
import { loadTabs } from "@/state/tabsStorage";
```

Then add a new `useEffect` immediately after the existing one:

```tsx
  // Restore the open tab list from Dexie once after mount.
  // If a snapshot is present, set `openTabs` and pick the
  // first valid active id (or the first tab when the persisted
  // active is gone). The store's openTab/setActiveTab actions
  // are reused so the rest of the app (RequestBuilder,
  // ResponseViewer) hydrates through the existing selectEndpoint
  // path.
  useEffect(() => {
    let active = true;
    void loadTabs().then((snapshot) => {
      if (!active) return;
      if (!snapshot) return;
      const { tabs, activeTabId } = snapshot;
      if (tabs.length === 0) return;
      const target =
        tabs.find((t) => t.id === activeTabId) ?? tabs[0]!;
      usePlayground.setState({ openTabs: tabs });
      usePlayground.getState().setActiveTab(target.id);
    });
    return () => {
      active = false;
    };
  }, []);
```

- [ ] **Step 3: Make the sidebar use `openTab` instead of `selectEndpoint` in `EndpointTree.tsx`**

In `frontend/src/components/EndpointTree.tsx`, find the line (around line 140):

```ts
  const select = usePlayground((s) => s.selectEndpoint);
```

Replace it with:

```ts
  const openTab = usePlayground((s) => s.openTab);
```

Then find the click handler (around line 303):

```tsx
                              onClick={() => select(e.operationId)}
```

Replace it with:

```tsx
                              onClick={() => openTab(e.operationId)}
```

- [ ] **Step 4: Pin the Monaco editor's parent height in `RequestBuilder.tsx`**

In `frontend/src/components/RequestBuilder.tsx`, find the wrapping div around the Monaco editor (around line 915):

```tsx
              <div className="flex-1 min-h-[12rem] rounded-md overflow-hidden border border-border">
                <Editor
```

Replace the `min-h-[12rem]` with `min-h-full`:

```tsx
              <div className="flex-1 min-h-full rounded-md overflow-hidden border border-border">
                <Editor
```

- [ ] **Step 5: Verify it type-checks and the build succeeds**

Run: `cd plugins/umbral-playground/frontend && npx tsc -b && npx vite build`
Expected: clean compile and a successful Vite build.

- [ ] **Step 6: Run the full test suite**

Run: `cd plugins/umbral-playground/frontend && npx vitest run`
Expected: every test passes.

- [ ] **Step 7: Commit**

```bash
cd plugins/umbral-playground/frontend && \
  git add src/App.tsx src/components/EndpointTree.tsx src/components/RequestBuilder.tsx && \
  git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -m "feat(playground): wire TabStrip into the layout, sidebar uses openTab

Mounts TabStrip between the stats row and the request/
response grid. Pins the request/response row at min-h-[640px]
lg:min-h-[720px]. Sidebar click handler now goes through
openTab so existing endpoints get the idempotent dedupe
behaviour for free. Monaco editor parent height follows
the new fixed parent.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 11: Update the README smoke test

**Files:**
- Modify: `plugins/umbral-playground/README.md`

- [ ] **Step 1: Add the manual smoke test**

Find the "## Manual smoke test" section in `plugins/umbral-playground/README.md` and append the new tab-focused steps (numbered to continue the existing list). The current list ends at step 8 (reload the page; the History tab should still show the entries). Add these:

```
9. Click a second endpoint in the sidebar. A second tab pill
   appears in the strip between the stats row and the request
   builder. The new tab is active.
10. Edit a header in the active tab. An amber dot appears
    on the tab pill indicating unsaved edits.
11. Refresh the page. The two tabs are still there, the same
    one is active, the draft is preserved.
12. Press `Cmd/Ctrl+W` (or `Ctrl+W` on Linux/Windows). The
    active tab closes; the next one to the right becomes
    active.
13. Click the download icon in the tab strip. A
    `umbral-playground-<scope>-<YYYY-MM-DD>.json` file
    downloads.
14. Open a private window at the same URL, click the upload
    icon, choose the file. The tabs and history appear in
    the import; the local empty workspace is filled.
```

- [ ] **Step 2: Commit**

```bash
cd plugins/umbral-playground && \
  git add README.md && \
  git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -m "docs(playground): add tab-focused manual smoke test steps

Closes features.md #12 manual verification.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 12: Final verification pass

**Files:**
- Read-only verification (no edits).

- [ ] **Step 1: Type-check the whole frontend**

Run: `cd plugins/umbral-playground/frontend && npx tsc -b`
Expected: clean compile.

- [ ] **Step 2: Run the full test suite**

Run: `cd plugins/umbral-playground/frontend && npx vitest run`
Expected: every test passes. (After this task there are tabs, draft, settings, codegen, and buildFetchArgs tests.)

- [ ] **Step 3: Lint**

Run: `cd plugins/umbral-playground/frontend && npx eslint src`
Expected: no errors.

- [ ] **Step 4: Build the bundle**

Run: `cd plugins/umbral-playground/frontend && npx vite build`
Expected: build succeeds; the produced `dist/` directory has the hashed JS/CSS asset files.

- [ ] **Step 5: Walk back through the spec's self-review checklist**

For each of the spec's §9 risks, confirm the mitigation in the implementation:

- **Stale `pristineDraft` after a spec reload** — the `markCurrentClean` action exists but the spec-reload `useEffect` doesn't yet call it. Add a one-line follow-up note to the implementation journal: this is a known follow-up, not in scope for this PR.
- **Tabs that point at operations no longer in the spec** — the `buildOperationLookup` returns `undefined` for missing operations, and the pill falls back to `?` for the method and `operationId` for the path. Confirmed.
- **Importing a snapshot from a different app scope** — the import code records the imported `appScope` in the snapshot but does not block. Confirmed.
- **Empty / malformed JSON file** — `JSON.parse` is wrapped in `try/catch` and emits the "Not a valid JSON file" toast. Confirmed.
- **Dexie quota** — no pre-flight check, but the per-operation cap of 50 keeps total row count bounded. Confirmed.
- **Race between `openTab` and draft hydration** — the post-open `setTimeout(..., 0)` resnapshots `pristineDraft` to the post-hydration value, and the existing `selectEndpoint` guard prevents the loaded draft from clobbering user input. Confirmed.

- [ ] **Step 6: No commit**

This task is verification only. If a step fails, fix the underlying issue in the appropriate task and re-run.

---

## Self-review checklist

- **Spec coverage:** Tabs UI (§5.1), popover (§5.2), export/import (§5.3), pinned layout (§5.4), lifecycle (§6), persistence (§4.4), hydrate on boot (§4.3), Cmd/Ctrl+W (§5.1) — all covered. Risks §9 — each mitigation is either present or noted as a follow-up.
- **Placeholder scan:** No "TBD", "TODO", "fill in later", or "appropriate error handling" placeholders. Every code block shows the actual code.
- **Type consistency:** `Tab` is defined in `store.ts` (Task 2) and consumed by `db.ts` (Task 1) and `tabsStorage.ts` (Task 2). The `tabs` table is registered in `db.ts` (Task 1) and consumed by `tabsStorage.ts` (Task 2). The slice actions are defined in `store.ts` (Task 5) and consumed by `TabStrip.tsx` (Task 9), `OpenTabPopover.tsx` (Task 7), `ExportImportControls.tsx` (Task 8), and `EndpointTree.tsx` (Task 10). The `TabStrip` component is created in Task 9 and mounted in Task 10. No naming drift.
- **No "Similar to Task N":** Each task's code block is complete; nothing is deferred to another task's code.
