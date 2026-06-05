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
      console.warn("[umbra-playground] tabs read failed", e);
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
      console.warn("[umbra-playground] tabs save failed", e);
    }
  }
}
