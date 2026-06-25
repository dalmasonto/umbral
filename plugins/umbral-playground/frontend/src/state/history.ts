import { db, type HistoryRow } from "./db";
import { scopedKey } from "./scope";
import type { ResponseRecord } from "./store";

// Per-app legacy key (gap #71). The localStorage-era history was
// also unscoped; if an app upgrades through this code path we
// migrate the per-app slot only — the unscoped legacy key for the
// "default" app is checked separately by the bare-key fallback in
// `migrateFromLocalStorage` below so existing single-app users
// don't lose their history at upgrade time.
const LEGACY_STORAGE_KEY = scopedKey("umbral-playground:history:v1");
const LEGACY_STORAGE_KEY_UNSCOPED = "umbral-playground:history:v1";
const PER_OPERATION_CAP = 50;

/** Replace strategy + per-op cap.
 *
 *  IndexedDB removes the 5MB ceiling localStorage imposed, so the total
 *  byte cap from the localStorage era is gone. We still cap each
 *  operation's history at 50 records to keep the History tab usable —
 *  beyond that, oldest-wins eviction is the right policy. */
export async function loadHistory(): Promise<Record<string, ResponseRecord[]>> {
  await migrateFromLocalStorage();
  const rows = await db.history.orderBy("timestamp").toArray();
  const grouped: Record<string, ResponseRecord[]> = {};
  for (const row of rows) {
    // `id` is the storage-layer PK; strip it so the in-memory shape
    // matches what the rest of the app expects.
    const { id: _id, ...record } = row;
    (grouped[record.operationId] ??= []).push(record);
  }
  return grouped;
}

let saveTimer: ReturnType<typeof setTimeout> | null = null;
let pending: Record<string, ResponseRecord[]> | null = null;

export function saveHistoryDebounced(
  history: Record<string, ResponseRecord[]>,
): void {
  pending = history;
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    const snapshot = pending;
    pending = null;
    saveTimer = null;
    if (snapshot) void persistHistory(snapshot);
  }, 500);
}

async function persistHistory(
  history: Record<string, ResponseRecord[]>,
): Promise<void> {
  // Wipe-and-replace inside a transaction. Simpler than diffing rows,
  // and atomic from the reader's perspective. At playground scale
  // (a few dozen ops × ≤50 records) the throughput hit is negligible.
  try {
    await db.transaction("rw", db.history, async () => {
      await db.history.clear();
      const inserts: HistoryRow[] = [];
      for (const records of Object.values(history)) {
        for (const record of records.slice(-PER_OPERATION_CAP)) {
          inserts.push({ ...record });
        }
      }
      if (inserts.length > 0) {
        await db.history.bulkAdd(inserts);
      }
    });
  } catch (err) {
    // Quota exceeded, browser blocked storage, etc. The in-memory
    // history still works for the current session — we just won't
    // persist this snapshot.
    if (typeof console !== "undefined") {
      console.warn("[umbral-playground] history persist failed", err);
    }
  }
}

/** Best-effort one-shot import of an older localStorage-backed history.
 *  Runs at most once: after the first successful import we clear the
 *  legacy key, so subsequent loads skip straight to IndexedDB.
 *
 *  Looks for the per-app legacy key first; falls back to the bare
 *  unscoped key so existing single-app users (who upgrade through
 *  gap #71) don't lose their pre-scope history. The unscoped key
 *  is only honoured when the per-app key is empty AND the current
 *  scope is `"default"` — i.e. the app didn't pass an explicit
 *  name. Other scopes leave the unscoped legacy blob alone for
 *  whichever default-named app eventually picks it up. */
async function migrateFromLocalStorage(): Promise<void> {
  if (typeof localStorage === "undefined") return;
  let raw: string | null;
  try {
    raw = localStorage.getItem(LEGACY_STORAGE_KEY);
    if (!raw && LEGACY_STORAGE_KEY.startsWith("default:")) {
      raw = localStorage.getItem(LEGACY_STORAGE_KEY_UNSCOPED);
    }
  } catch {
    return;
  }
  if (!raw) return;
  try {
    const legacy = JSON.parse(raw) as Record<string, ResponseRecord[]>;
    const existing = await db.history.count();
    if (existing === 0) {
      const all: HistoryRow[] = [];
      for (const records of Object.values(legacy)) {
        if (!Array.isArray(records)) continue;
        for (const record of records) {
          if (record && typeof record === "object") {
            all.push({ ...record });
          }
        }
      }
      if (all.length > 0) {
        await db.history.bulkAdd(all);
      }
    }
    localStorage.removeItem(LEGACY_STORAGE_KEY);
    if (LEGACY_STORAGE_KEY.startsWith("default:")) {
      localStorage.removeItem(LEGACY_STORAGE_KEY_UNSCOPED);
    }
  } catch {
    // Legacy blob unparseable — drop it so we don't keep retrying.
    try {
      localStorage.removeItem(LEGACY_STORAGE_KEY);
    } catch {
      // ignore
    }
  }
}
