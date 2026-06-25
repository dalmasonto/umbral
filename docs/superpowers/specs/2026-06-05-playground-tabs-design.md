# Playground tabs — design spec

**Status:** approved design, ready for implementation planning
**Date:** 2026-06-05
**Scope:** `bugs/features.md` item #12 — playground tabs with Dexie persistence and a fixed-height request/response layout. UI-only; no Rust changes.

## 1. Goal

Give the `umbral-playground` UI a browser-style tab strip so users can keep multiple endpoints open at once, see at a glance which one is active, and have that state survive a reload. Pin the request and response panels to a known minimum height so the layout doesn't reflow when switching between tabs, switching between body types, or seeding a fresh draft. Bundle a snapshot export/import so a workspace can move between browsers or be shared between colleagues.

This is a frontend-only change. The Rust plugin crate is untouched.

## 2. Non-goals

- No drag-and-drop tab reordering in this PR. The `reorderTab` action exists on the store so a follow-up can wire the gesture without re-persisting the shape, but the UI doesn't ship it.
- No per-tab response state. The `lastResponse` for an endpoint already lives in the per-endpoint `history[operationId]` lookup; tabs don't own response memory separately.
- No Rust-side changes. The plugin crate, the build script, the asset pipeline, and the route registration are all untouched.
- No URL-based deep linking to a specific tab.
- No "Close all" / "Close other tabs" context menu.

## 3. Architecture

The change touches four layers inside `plugins/umbral-playground/frontend/`:

| Layer | What gets added | Why |
|---|---|---|
| Persistence | New `tabs` table in `state/db.ts` (Dexie schema v5), new `state/tabsStorage.ts` | One singleton row keyed `"workspace"`, holding the array of `Tab` and the `activeTabId`. Mirrors the `settings` table shape. |
| Store | New `Tab` interface and `TabsSlice` on `usePlayground` | Tabs are first-class state. Existing `selectedOperationId` stays for backward compat with components that subscribe to it; it's now derived from `activeTab`'s `operationId`. |
| Components | New `TabStrip`, `OpenTabPopover`, `ExportImportControls`. `App.tsx` wires the strip in. | Self-contained UI. The existing `RequestBuilder` and `ResponseViewer` are pinned-height consumers; their internals don't grow new state. |
| Layout | New row in the main grid: `grid-rows-[auto_auto_minmax(0,1fr)]`. The request/response grid below gets `min-h-[640px] lg:min-h-[720px]`. | Tab strip sits at fixed `h-10`. Request and response panels get a known minimum height so the user always knows where the scrollable area starts. |

The store is the single source of truth. Components subscribe to `openTabs`, `activeTabId`, and the `openTab` / `setActiveTab` / `closeTab` actions. The sidebar click handler changes from calling `selectEndpoint` directly to calling `openTab` (which is idempotent and handles the dedupe case) — so the new behavior is automatic for every existing user of the sidebar.

## 4. Data model

### 4.1 The `Tab` interface

```ts
// in state/store.ts
export interface Tab {
  /** Stable id for this open-tab slot — independent of
   *  operationId so the same endpoint can be opened twice in
   *  principle. Generated with crypto.randomUUID() on open. */
  id: string;
  operationId: string;
  /** Wall-clock the tab was opened. Used as the default
   *  display order. */
  openedAt: number;
  /** Snapshot of the draft as it existed when this tab was
   *  opened (or after a manual "reset to opened" action). The
   *  dirty dot compares `current` against this — equal means
   *  clean. */
  pristineDraft: RequestDraft;
}
```

### 4.2 The Dexie table

```ts
// in state/db.ts
export interface TabsRow {
  key: "workspace";   // singleton
  schema: number;
  tabs: Tab[];
  activeTabId: string | null;
  updatedAt: number;
}
export const TABS_SCHEMA_VERSION = 1;

db.version(5).stores({
  history: "++id, operationId, timestamp",
  editorState: "&key",
  settings: "&key",
  drafts: "&operationId, updatedAt",
  tabs: "&key",
});
```

The singleton-row shape matches the `settings` table. We get per-app isolation from the per-app DB name (`umbral-playground:<scope>`) for free. The shape is small enough that one write per change is fine.

### 4.3 The Zustand slice

```ts
interface TabsSlice {
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
}
```

The slice is hydrated at boot by an `useEffect` in `App.tsx` that calls `loadTabs()` from `state/tabsStorage.ts`. The first time the app loads with the new code, no `tabs` row exists, the loader returns `null`, and the playground falls back to the existing "no endpoint selected" empty state. Existing users see no change on first load after upgrade.

### 4.4 Persist cadence

The store gets a `scheduleTabsSave()` sibling to the existing `scheduleDraftSave()` — same 250ms debounce. The tabs row is written when:

- `openTab` / `closeTab` / `setActiveTab` / `reorderTab` runs (structural changes).
- `markCurrentClean` runs (pristine snapshot updated after a tab's draft hydrates).

The tabs row is **not** re-written on every keystroke. The `current` draft for an open tab already lives in the `drafts` table; the `tabs` row only changes when the list or the pristine snapshot changes. Keystroke volume is high; tab-state-change volume is low.

## 5. Component layer

### 5.1 `TabStrip.tsx`

A single row that lives above the request/response panels. Renders one pill per open tab, a `+` button to open a picker, and the export/import controls on the right.

Pill anatomy:

- Method chip on the left (e.g. `POST`), uppercase tracking-wide, sized `9-10px` to fit a 28-32px tall pill. Reuses the existing `MethodBadge` component (`components/MethodBadge.tsx`) for color consistency with the rest of the playground.
- Operation path on the right of the method chip, truncated at `max-w-[180px]`.
- Dirty dot — a 6px amber circle to the right of the path — when the active tab's `current` differs from `pristineDraft`.
- × button on the far right, with hover state turning the icon destructive-red. Click handler stops propagation so it doesn't also activate the tab.

Active pill: solid background (`bg-background`), normal border, foreground text. Inactive pill: muted background, transparent border, muted text. Active and inactive share the same height; the active pill gets a `border-b-2 border-primary` underline so the row of tabs reads as a connected bar.

The strip uses `overflow-x-auto` so 30+ tabs scroll horizontally rather than reflowing the page. The `+` button and the export/import controls stay pinned to the right via `flex-1` spacer.

**Keyboard shortcut (captured at the strip level):**

- `Cmd/Ctrl+W` — close the active tab. Suppressed when the focus is inside an `<input>` or `<textarea>` so it doesn't fire while the user is typing.

We deliberately do **not** add `Cmd/Ctrl+Tab` and `Cmd/Ctrl+Shift+Tab` — browsers reserve those for native tab switching and most don't let the page override them. The pill click and the + popover are the primary ways to switch tabs; the keyboard shortcut is a small bonus for power users.

### 5.2 `OpenTabPopover.tsx`

A small popover anchored to the `+` button. Lists every operation in the current spec, filtered to those without an open tab. Each row is a button: method chip + path + summary. Click → `openTab(operationId)`. The popover is a Radix Popover from `components/ui/popover` (already a dep via `radix-ui`).

### 5.3 `ExportImportControls.tsx`

Two icon buttons (`Download`, `Upload` from `lucide-react`) at the right end of the tab strip. The file input is hidden (`type="file" hidden`); the upload button triggers `fileInputRef.current?.click()`.

**Export flow** (`onExport`):

1. Build the snapshot:
   ```ts
   {
     version: 1,
     exportedAt: Date.now(),
     appScope: getAppScope(),
     tabs: openTabs,
     drafts: Object.fromEntries(
       openTabs.map(t => [t.operationId, getDraft(t.operationId)])
     ),
     history: await db.history.toArray(),
     settings: get().settings,
   }
   ```
2. `JSON.stringify(snapshot, null, 2)` → `Blob` → hidden `<a download>` click.
3. Filename: `umbral-playground-<appScope>-<YYYY-MM-DD>.json`.

**Import flow** (`onFile`):

1. Read file as text. `JSON.parse` inside a try/catch.
2. Validate shape: `typeof snapshot === "object"`, `snapshot.version === 1`, `Array.isArray(snapshot.tabs)`. Fail → toast "Not a playground snapshot."
3. Merge tabs: for each imported tab, if a tab with the same `operationId` exists locally, skip; else append. New tab ids are regenerated with `crypto.randomUUID()` to avoid collisions.
4. Merge drafts: for each imported `drafts[operationId]`, if no local draft row exists, `db.drafts.put(...)`. Otherwise skip.
5. Merge history: for each imported history row, dedupe by `(operationId, timestamp)` and `bulkAdd` the missing ones. Cap per-op at the existing `PER_OPERATION_CAP` (50).
6. Settings: import only if the local `settings` table is empty. Otherwise leave alone.
7. If the imported tabs list is non-empty, set the first imported tab as `activeTabId` and call `setActiveTab` so the UI refreshes.
8. Toast: "Imported N tabs, M history rows." (`pushToast` from `state/toastStore.ts:5`.)

### 5.4 Pinned-height layout

In `App.tsx`, the main grid becomes:

```tsx
<div className="grid min-h-0 flex-1 grid-rows-[auto_auto_minmax(0,1fr)] overflow-hidden">
  <StatsRow />              {/* existing, h-auto */}
  <TabStrip />              {/* new, h-10, shrink-0 */}
  <div className="grid min-h-0 grid-cols-1 lg:grid-cols-2 min-h-[640px] lg:min-h-[720px]">
    <RequestBuilder />      {/* existing */}
    <ResponseViewer />      {/* existing */}
  </div>
</div>
```

The `min-h-[640px] lg:min-h-[720px]` is the "fixed known height" — it sets the row's minimum height so the user always knows where the scrollable area starts. Inside each panel the existing `flex flex-col h-full min-h-0` composes with the new parent and the inner `flex-1 overflow-y-auto` (or `flex-1 min-h-0 overflow-y-auto`) tab content scroll area constrains scrolling to the right region.

`min-h` and not `h`: a fixed `h` would lock the panel even when the user has a tiny window. `min-h` gives "always at least this tall" without forbidding taller windows.

The Monaco editor inside the JSON body tab currently uses `height="100%"` against a `flex-1 min-h-[12rem]` parent. The `min-h-[12rem]` becomes `min-h-full` so the editor fills the new fixed-height parent.

## 6. Tab lifecycle

**Open:** `openTab(operationId)` is idempotent. If a tab for that `operationId` already exists, it becomes active. Otherwise a new `Tab { id, operationId, openedAt, pristineDraft: current }` is appended; `activeTabId` is set to the new id; `selectEndpoint(operationId)` runs the existing draft-hydration path. Once the draft hydrates, `markCurrentClean` is called to refresh `pristineDraft` to the post-hydration value.

**Activate:** `setActiveTab(id)` updates `activeTabId` and calls `selectEndpoint(operationId)` for the corresponding tab.

**Close:** `closeTab(id)` removes the tab from `openTabs`. "Right neighbor" means the tab with the next-higher index in `openTabs`; "left neighbor" is the next-lower. If the closed tab was active, activate the right neighbor (or the left neighbor if it was the last). If no tabs remain, `activeTabId = null` and the existing "select an endpoint" empty state renders. Closing the active tab also calls `selectEndpoint` for the new active tab's `operationId` so the right panel rehydrates.

**Reload:** `App.tsx` calls `loadTabs()` on mount. If a snapshot is present, `openTabs` is restored. If the persisted `activeTabId` is in the loaded list, it becomes active; otherwise the first tab becomes active; if the list is empty, the playground stays in its current empty state.

**Dirty marker:** A small amber dot on the tab pill. Computed as `JSON.stringify(current) !== JSON.stringify(pristineDraft)`. The dot is visible only when the tab is open and the user has edited any draft field.

## 7. File-by-file change list

**New files**

- `plugins/umbral-playground/frontend/src/state/tabsStorage.ts` — `loadTabs()` / `saveTabs()` Dexie helpers.
- `plugins/umbral-playground/frontend/src/components/TabStrip.tsx` — the tab bar.
- `plugins/umbral-playground/frontend/src/components/OpenTabPopover.tsx` — the `+` popover.
- `plugins/umbral-playground/frontend/src/components/ExportImportControls.tsx` — export/import buttons.
- `plugins/umbral-playground/frontend/src/__tests__/tabs.test.ts` — slice and persistence tests.

**Modified files**

- `plugins/umbral-playground/frontend/src/state/db.ts` — add `TabsRow` interface, bump schema to v5, register the `tabs` table, export `TABS_SCHEMA_VERSION = 1`.
- `plugins/umbral-playground/frontend/src/state/store.ts` — add the `Tab` interface, the `TabsSlice` interface, the new state and actions on `usePlayground`, and the `scheduleTabsSave` debounced persist.
- `plugins/umbral-playground/frontend/src/App.tsx` — render `<TabStrip />` between the stats row and the request/response grid; add `min-h-[640px] lg:min-h-[720px]` to the request/response row; add the `useEffect` that calls `loadTabs()` on mount.
- `plugins/umbral-playground/frontend/src/components/RequestBuilder.tsx` — change `min-h-[12rem]` on the Monaco wrap to `min-h-full` so the editor fills the new fixed-height parent.
- `plugins/umbral-playground/frontend/src/components/EndpointTree.tsx` — change the `onClick` handler to call `openTab(operationId)` instead of `selectEndpoint(id)`. Single line. The existing active-entry highlight in the sidebar (which subscribes to `selectedOperationId`) keeps working unchanged because `selectedOperationId` becomes derived from the active tab's `operationId` — when the user activates a tab, the highlight in the sidebar follows automatically.

**Untouched**

- `crates/umbral-playground/src/**` (no Rust changes).
- `RequestBuilder.tsx` body content beyond the height tweak.
- `ResponseViewer.tsx` (its `flex-1 min-h-0 overflow-y-auto` already composes with the new parent).
- `state/history.ts`, `state/draftStorage.ts`, `state/settingsStorage.ts`, `state/editorState.ts` — all their public APIs stay.
- The spec loader, the URL bar, the response status bar, the Codegen panel, the History dialog, the Toaster, the Settings sheet — all stay as-is.

## 8. Testing

**Unit tests** in `__tests__/tabs.test.ts` (mirroring the patterns in `__tests__/draftPersistence.test.ts`):

- `openTab(operationId)` adds a tab and makes it active.
- `openTab(operationId)` on an already-open `operationId` only activates, does not duplicate.
- `closeTab(id)` on the active tab activates the right neighbor.
- `closeTab(id)` on the last tab falls back to the left neighbor.
- `closeTab(id)` on the only tab leaves `activeTabId = null`.
- `setActiveTab(id)` updates `activeTabId` and runs `selectEndpoint` with the right `operationId`.
- `markCurrentClean` updates the active tab's `pristineDraft` to `current`.
- Tabs persist to Dexie within the debounce window; reload restores them.
- On reload, if the persisted `activeTabId` is no longer in the loaded list, the first tab is activated.

**Manual smoke test** (added to `README.md`):

1. `cargo run` in `examples/shop`.
2. Open `http://localhost:<port>/api/playground/`.
3. Click three different endpoints in the sidebar. The tab strip fills with three pills, the active one underlined.
4. Edit a header in the active tab. The dirty dot appears on the pill.
5. Refresh the page. The three pills are still there, the same one is active, the draft is preserved.
6. Press `Cmd/Ctrl+W`. The active tab closes; the next one to the right becomes active.
7. Click the export button. A `.json` file downloads. Open it — it contains `tabs`, `drafts`, `history`, `settings`.
8. Open a private window at the same URL. Click import, choose the file. The tabs and history appear; the local empty workspace is filled.

## 9. Risks and edge cases

- **Stale `pristineDraft` after a spec reload.** If the spec reloads and an operation's `method` changes, the dirty dot will fire forever. Mitigation: when the spec is reloaded, reset `pristineDraft = current` for the active tab. One line in the existing `useEffect` that watches the spec.
- **Tabs that point at operations no longer in the spec.** Possible after a spec reload that drops an operation. The pill will show `undefined` for the method. Mitigation: the pill falls back to a question-mark chip and the × button is still there. The store's `loadTabs` post-reload doesn't filter — the user can close the tab or the spec can come back.
- **Importing a snapshot from a different app scope.** Each app has its own Dexie DB (via `scopedKey`). Importing a file with `appScope: "shop"` into a browser at `"admin"` will write the rows to the current app's DB. The `appScope` field is informational (logged in the toast) but does not block the import. The user is in charge.
- **Empty / malformed JSON file.** `JSON.parse` throws on `''`; we wrap in try/catch and toast "Not a valid JSON file."
- **Dexie quota.** The history table is the biggest row count. Importing 50 ops × 50 records × 2 apps = 5000 rows; fits comfortably in the IndexedDB quota. No pre-flight quota check.
- **Race between `openTab` and draft hydration.** `openTab` snapshots `pristineDraft` synchronously, but the actual draft loads asynchronously via the existing `selectEndpoint` path. If the user types in the (still empty) Body field before the draft hydrates, the typed value would be clobbered by the loaded draft. The existing store already guards against this (`if (cur.current.url !== "" || cur.current.body !== "") return;` in `selectEndpoint`). The new `markCurrentClean` runs after that guard, so the pristine snapshot is set to the post-hydration value, not the empty starting value. The dirty dot starts clean.

## 10. Out of scope (deferred)

- Drag-and-drop tab reordering UI. The `reorderTab` action exists; the gesture is a follow-up.
- Per-tab response state.
- "Close all tabs" / "Close other tabs" context menu.
- Sharing a single tab via a permalink (`/playground?tab=<id>` URL scheme).
