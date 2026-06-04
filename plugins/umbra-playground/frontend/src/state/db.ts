import Dexie, { type EntityTable } from "dexie";
import { getAppScope } from "./scope";
import type { PlaygroundSettings, ResponseRecord } from "./store";

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

/** Workspace-settings row. Singleton — we always use the key
 *  `"workspace"`. Schema-versioned: bumping the `schema` field lets
 *  us evolve `PlaygroundSettings` shape over time without re-reading
 *  garbage. */
export interface SettingsRow {
  key: string;
  schema: number;
  value: PlaygroundSettings;
  updatedAt: number;
}

// Per-app Dexie database name (gap #71). Two apps in the same
// browser get two separate IndexedDB databases — history /
// editorState / settings slots can't bleed across app boundaries.
const DB_NAME = `umbra-playground:${getAppScope()}`;

export const db = new Dexie(DB_NAME) as Dexie & {
  history: EntityTable<HistoryRow, "id">;
  editorState: EntityTable<EditorStateRow, "key">;
  settings: EntityTable<SettingsRow, "key">;
};

// v1: history table. v2 adds the editorState table for persistent
// UI slots (expanded sidebar groups, search input, active tabs).
// v3 adds the settings table (workspace settings, previously stored
// in localStorage — moved to IndexedDB so the 5MB quota goes away
// and the unreliable-private-mode-throws localStorage edge cases
// stop biting us). Upgrades are automatic — Dexie keeps the
// existing tables untouched and just adds the new one.
db.version(1).stores({
  history: "++id, operationId, timestamp",
});

db.version(2).stores({
  history: "++id, operationId, timestamp",
  editorState: "&key",
});

db.version(3).stores({
  history: "++id, operationId, timestamp",
  editorState: "&key",
  settings: "&key",
});

/** Schema version stamped on every `SettingsRow`. Bump alongside any
 *  breaking change to `PlaygroundSettings`. Reads with a lower
 *  version go through the normaliser to fill in missing fields;
 *  reads with a higher version (downgrade) are discarded back to
 *  defaults so we don't pretend to understand them. */
export const SETTINGS_SCHEMA_VERSION = 1;
