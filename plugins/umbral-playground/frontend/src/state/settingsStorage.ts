/** Workspace-settings persistence on top of Dexie / IndexedDB.
 *
 *  Settings used to live in `localStorage` — convenient for sync
 *  boot reads, but the 5 MB origin quota and the "private mode
 *  silently throws" failure mode were causing real "settings
 *  aren't saving" reports (gap #76 follow-up).
 *
 *  The new layout:
 *
 *  - **Source of truth:** IndexedDB, in the `settings` table of the
 *    per-app Dexie DB (`umbral-playground:<app>`). Generous quota,
 *    transactional writes, structured-clone serialization.
 *  - **Boot cache:** `localStorage` mirrors the latest snapshot so
 *    the next page reload can render persisted state synchronously
 *    instead of flashing the defaults while Dexie hydrates.
 *
 *  Save flow (per call):
 *    1. Write Dexie first — that's the bit that matters.
 *    2. Mirror to localStorage as a best-effort cache. Failures here
 *       are warnings only, not "dirty" status, because Dexie has
 *       the truth.
 *
 *  Boot flow (across two ticks):
 *    1. Sync read localStorage at module load → initial state.
 *    2. After mount, async load Dexie. If Dexie has data, use it;
 *       it's authoritative.
 *
 *  Migration: on the first Dexie hydration where the `settings`
 *  table is empty AND the localStorage cache holds a row, seed
 *  Dexie from localStorage and leave the cache in place. Existing
 *  users don't lose their workspace on upgrade. */

import { db, SETTINGS_SCHEMA_VERSION } from "./db";
import { scopedKey } from "./scope";
import type { PlaygroundSettings } from "./store";

/** Singleton key — only one row in the table per app DB. */
const ROW_KEY = "workspace";

/** Per-app localStorage key for the sync boot cache. Same shape
 *  as before so an existing user's settings round-trip into the
 *  new world without action. */
export const LEGACY_LS_KEY = scopedKey("umbral-playground-settings:v1");

/** Sync-read the localStorage boot cache. Returns the parsed
 *  partial-settings blob or null. NEVER throws — boot must not
 *  hang on a malformed cache. */
export function readLocalStorageCache(): Partial<PlaygroundSettings> | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(LEGACY_LS_KEY);
    if (!raw) return null;
    return JSON.parse(raw) as Partial<PlaygroundSettings>;
  } catch {
    return null;
  }
}

/** Best-effort write to the localStorage boot cache. Errors are
 *  logged once and otherwise swallowed — the cache is convenience,
 *  not the source of truth.
 *
 *  Returns `true` on success so the store can report cache health
 *  separately from Dexie health if it wants. */
export function writeLocalStorageCache(settings: PlaygroundSettings): boolean {
  if (typeof window === "undefined") return false;
  try {
    window.localStorage.setItem(LEGACY_LS_KEY, JSON.stringify(settings));
    return true;
  } catch (e) {
    if (typeof console !== "undefined") {
      console.warn("[umbral-playground] localStorage cache write failed", e);
    }
    return false;
  }
}

/** Authoritative read from Dexie. Returns null when the table is
 *  empty (first-ever boot) or when Dexie / IndexedDB is unavailable
 *  (private mode in some browsers, etc.). */
export async function readSettingsFromDexie(): Promise<PlaygroundSettings | null> {
  try {
    const row = await db.settings.get(ROW_KEY);
    if (!row) return null;
    if (row.schema > SETTINGS_SCHEMA_VERSION) {
      // Future schema — refuse to interpret. The next save will
      // overwrite with the current version.
      return null;
    }
    return row.value;
  } catch (e) {
    if (typeof console !== "undefined") {
      console.warn("[umbral-playground] settings Dexie read failed", e);
    }
    return null;
  }
}

/** Authoritative write to Dexie. Returns `true` only when the row
 *  is persisted; callers branch the UI's saveStatus on this. */
export async function writeSettingsToDexie(
  settings: PlaygroundSettings,
): Promise<boolean> {
  try {
    await db.settings.put({
      key: ROW_KEY,
      schema: SETTINGS_SCHEMA_VERSION,
      value: settings,
      updatedAt: Date.now(),
    });
    return true;
  } catch (e) {
    if (typeof console !== "undefined") {
      console.warn("[umbral-playground] settings Dexie write failed", e);
    }
    return false;
  }
}

/** Combined save: Dexie first (the truth), then the localStorage
 *  cache (the convenience). Returns the Dexie outcome — the cache
 *  failing alone shouldn't trip the "dirty" UI state because the
 *  next reload will still pull the right settings from Dexie. */
export async function persistSettings(
  settings: PlaygroundSettings,
): Promise<boolean> {
  const dexieOk = await writeSettingsToDexie(settings);
  writeLocalStorageCache(settings);
  return dexieOk;
}

/** One-shot migration from the localStorage boot cache to Dexie.
 *  Called by the store the first time it hydrates after boot. If
 *  Dexie is empty AND localStorage holds a row, copy it over so
 *  upgraders don't see a blank workspace.
 *
 *  Returns the settings that should be applied (Dexie's row when
 *  present, the seeded localStorage row when not, or null when
 *  neither has anything — first-ever boot). */
export async function hydrateInitialSettings(): Promise<PlaygroundSettings | null> {
  const fromDexie = await readSettingsFromDexie();
  if (fromDexie) return fromDexie;
  const fromLs = readLocalStorageCache();
  if (!fromLs) return null;
  // Seed Dexie from the localStorage cache. The store will
  // normalize before handing us the value, so we don't need to
  // re-shape here.
  await writeSettingsToDexie(fromLs as PlaygroundSettings);
  return fromLs as PlaygroundSettings;
}
