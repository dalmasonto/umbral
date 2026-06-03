import Dexie, { type EntityTable } from "dexie";
import { getAppScope } from "./scope";
import type { ResponseRecord } from "./store";

export interface HistoryRow extends ResponseRecord {
  /** Dexie auto-increment PK. Optional on the type because we never
   *  set it on insert — bulkAdd / put assigns it. */
  id?: number;
}

/** Per-key UI state slot. Keyed by a stable string the consumer
 *  picks (e.g. `"endpoint-tree.search"`). Value is whatever JSON the
 *  consumer needs to round-trip — Dexie serialises with
 *  structuredClone so non-primitive values (Records, arrays) work
 *  without us writing a JSON shape per slot. */
export interface EditorStateRow {
  /** The slot key. Primary. Convention: `<component>.<field>`. */
  key: string;
  /** Whatever value the consumer wants persisted. */
  value: unknown;
}

// Per-app Dexie database name (gap #71). Two apps in the same
// browser get two separate IndexedDB databases — history /
// editorState slots can't bleed across app boundaries.
const DB_NAME = `umbra-playground:${getAppScope()}`;

export const db = new Dexie(DB_NAME) as Dexie & {
  history: EntityTable<HistoryRow, "id">;
  editorState: EntityTable<EditorStateRow, "key">;
};

// v1: history table. v2 adds the editorState table for persistent
// UI slots (expanded sidebar groups, search input, active tabs).
// Upgrading from v1 is automatic — Dexie keeps the existing
// history table untouched and just adds the new one.
db.version(1).stores({
  history: "++id, operationId, timestamp",
});

db.version(2).stores({
  history: "++id, operationId, timestamp",
  editorState: "&key",
});
